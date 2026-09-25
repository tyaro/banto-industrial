/**
 * 監査ログ一覧（`/audit-log`）の**ブロックキャッシュの判断**（#428）。
 * chronogazer の同名ファイル（#410）と同じ方針・同じ形で、違いは import の
 * 書き方だけ（banto-hub の vitest は `$lib` の別名を解決しないので、値の
 * import は相対パス）。
 *
 * banto-hub の修正前の `AuditLogWindow`（`+page.svelte` に埋め込み）で、
 * chronogazer と同じ欠陥を**実際に再現した**（修正前のクラスを逐語で写して
 * vitest で回した - 結果は PR #428 の本文）:
 *
 * 1. **総件数が未取得だと要求を 1 本も出さない** - 初回取得が失敗した後も、
 *    正常に 0 件（絞り込み）になった後に絞り込みを外しても、`BantoGrid` が
 *    通知する表示範囲が `{0, 0}` なので空振りし、**回復の手段が無かった**
 *    （「再読み込み」ボタン自体も無かった）、
 * 2. **ブロックの合間に行が増える・消えると、境界で重複・欠落する**
 *    （監査ログは操作・ログイン失敗・API キーの拒否のたびに増え、一覧を読む
 *    たびに保持期間の削除が走る）、
 * 3. **失敗を状態として持っていない** - 失敗はトースト（4 秒で消える）だけで、
 *    未取得・取得失敗・正常な 0 件の区別も再試行の手段も無かった。
 *
 * 世代・境界・飛行中・ブロック単位の失敗・世代違いの応答の排除は
 * `$lib/blockCache`（chronogazer の複製）が行い、ここには `/audit-log` 固有の
 * 部分だけを置く:
 *
 * - **保持期間の削除によるスナップショット失効**（[`AUDIT_POLICY`]）。境界
 *   （`id <= asOfId`）は**追加**にしか効かない。`audit_log` の行は書き換え
 *   られず、境界より後の行は集合に入らないので、**同じ境界・同じ条件の総件数が
 *   世代の最初と変わったら、それは削除**で、`OFFSET` がずれている可能性がある。
 *   その応答は採らず、**この世代ではもう取らない**（取っても採れない）。
 *   読み直すのは利用者が「再読み込み」を押したとき。自動で世代を切り直すと、
 *   削除が続く間（保持件数の上限に張り付いて記録が続いているとき）に
 *   **一覧が永久に読み終わらない**。
 * - **並べ替え・絞り込みの変更は新しい問い合わせ**（`BlockLoader.restart`）:
 *   前の問い合わせの行・件数・失敗を持ち越さない。
 * - 画面に出す値（[`auditViewState`]）。
 *
 * `audit_log.id` は `INTEGER PRIMARY KEY AUTOINCREMENT`
 * （`core/src/db.rs` の `apply_app_schema`）なので単調増加かつ削除後も
 * 再利用されない - 境界が「集合のメンバー」を決められる前提はこれ。
 */
import type { AuditLogEntry } from '$lib/banto/auditLogAdmin';
import {
	BlockLoader,
	isGenerationHalted,
	type BlockCache,
	type BlockFetcher,
	type BlockOutcome,
	type BlockPolicy
} from '../../../lib/blockCache';

/**
 * ブロック 1 つの取得が行を入れられなかったときの持ち方。
 *
 * - `error`: 往復そのものが失敗した / 待つのをやめた / 境界の食い違い、
 * - `expired`: 取得の途中で集合の行が削除された（スナップショット失効）。
 */
export type AuditBlockFailure = { kind: 'error'; message: string } | { kind: 'expired' };

export type AuditBlockOutcome = BlockOutcome<AuditLogEntry, AuditBlockFailure>;
export type AuditBlockCache = BlockCache<AuditBlockFailure>;
export type AuditBlockFetcher = BlockFetcher<AuditLogEntry, AuditBlockFailure>;

/** 画面に出す値（[`auditViewState`] が導く）。 */
export interface AuditViewState {
	loading: boolean;
	/** `null` = この問い合わせではまだ一度も読めていない（「0 件」と言い切らない）。 */
	totalCount: number | null;
	/** 往復の失敗の文言（失敗したブロックのうち先頭のもの）。 */
	errorText: string | null;
	/** 取得の途中で削除が起き、続きの読み込みを止めている。 */
	expired: boolean;
	/** 取得に失敗したままのブロック数。 */
	failedBlockCount: number;
}

/** 要求した境界と違う境界の応答（サーバーが `asOfId` を無視したとき）。 */
export const AUDIT_BOUNDARY_MISMATCH_MESSAGE =
	'監査ログの取得範囲がサーバー側で切り替わりました。「再読み込み」でもう一度読み込んでください。';

/** スナップショット失効の文言。 */
export const AUDIT_SNAPSHOT_EXPIRED_MESSAGE =
	'読み込みの途中で、保持ポリシーにより古い記録が削除されました。このまま続きを読むと行がずれるため、続きの読み込みを止めています。「再読み込み」で最新の状態から読み直してください。';

/**
 * `/audit-log` の方針:
 *
 * - 境界が食い違った応答は採らない（サーバーが `asOfId` を無視したとき）、
 * - **同じ境界の総件数が変わった応答は採らず（失効）、この世代を止める**。
 */
export const AUDIT_POLICY: BlockPolicy<AuditLogEntry, AuditBlockFailure> = {
	reject(snapshot, list) {
		if (list.asOfId !== snapshot.asOfId) {
			return { kind: 'error', message: AUDIT_BOUNDARY_MISMATCH_MESSAGE };
		}
		if (list.totalCount !== snapshot.totalCount) return { kind: 'expired' };
		return null;
	},
	haltsGeneration(failure) {
		return failure.kind === 'expired';
	}
};

/**
 * 画面に出す値。失敗しているブロックが 1 つでもあれば出し続ける（別ブロックの
 * 成功で消さない）。**失効は今の世代のものだけ**を「止めている」と言う -
 * 前の世代の失効は「再読み込み」でそのブロックを取り直している最中なので。
 */
export function auditViewState(cache: AuditBlockCache): AuditViewState {
	const failures = [...cache.failed.entries()].sort(([a], [b]) => a - b);
	const error = failures.find(([, record]) => record.failure.kind === 'error');
	return {
		loading: cache.inFlight.size > 0,
		totalCount: cache.everRead ? cache.totalCount : null,
		errorText: error && error[1].failure.kind === 'error' ? error[1].failure.message : null,
		expired: isGenerationHalted(cache, AUDIT_POLICY),
		failedBlockCount: cache.failed.size
	};
}

/** 画面（`$state`）への書き戻し口。 */
export interface AuditBlockSink {
	resetRows(length: number): void;
	writeRows(offset: number, rows: AuditLogEntry[]): void;
	update(view: AuditViewState): void;
}

/**
 * `/audit-log` の駆動役（汎用部の `BlockLoader` に `/audit-log` の方針と
 * [`auditViewState`] への変換を渡しただけ）。画面は表示範囲の変更を
 * `setRange`、「再読み込み」を `reload`、並べ替え・絞り込みの変更を
 * `restart` で伝える。
 */
export class AuditBlockLoader extends BlockLoader<AuditLogEntry, AuditBlockFailure> {
	constructor(fetcher: AuditBlockFetcher, sink: AuditBlockSink) {
		super(
			fetcher,
			{
				resetRows: (length) => sink.resetRows(length),
				writeRows: (offset, rows) => sink.writeRows(offset, rows),
				update: (cache) => sink.update(auditViewState(cache))
			},
			AUDIT_POLICY,
			(err) => ({ kind: 'error', message: String(err) })
		);
	}
}
