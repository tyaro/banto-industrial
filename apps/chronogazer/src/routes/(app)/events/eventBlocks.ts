/**
 * イベント一覧（`/events`）の**ブロックキャッシュの判断**（#409 オーナー
 * レビュー P2 の 3 件）。`tagsPageLogic.ts` / `hubAdmin.ts` と同じ作法で、
 * 状態遷移を**純関数**に出して表テストで固定し、画面（`+page.svelte`）は
 * これを呼ぶだけにする。
 *
 * ここが決めるのは 5 つ:
 *
 * 1. **どのブロックを取るか**（[`blocksToFetch`]。表示範囲・総件数の有無・
 *    失敗したブロックを 1 つの述語にまとめる - 「再読み込みで何を取り直すか」
 *    を別々に列挙しない）、
 * 2. **何を渡して取るか**（[`blockRequest`]。世代のスナップショット境界
 *    `asOfId`）、
 * 3. **応答をどこに入れるか**（[`applyOutcome`]。世代違いの応答は採らない）、
 * 4. **失敗をどう持つか**（[`applyOutcome`]。**ブロック単位**。別ブロックの
 *    成功で消さない）、
 * 5. **画面に何を出すか**（[`viewState`]）。
 *
 * ### なぜ「世代」と `asOfId`（スナップショット境界）が要るか
 *
 * サーバーは各リクエスト時点のデータを `ORDER BY ts DESC, id DESC LIMIT …
 * OFFSET …` で返す。ブロック取得の**合間に先頭へイベントが追加される**と
 * `OFFSET` がずれ、境界で行が重複し、末尾の行が一覧から漏れる。そこで
 * **同じ世代のブロックは同じデータ集合から取る**: 世代の最初の応答が返した
 * 境界（`collect_events.id` の最大値）を固定し、後続ブロックにすべて渡す。
 * `collect_events.id` は `INTEGER PRIMARY KEY AUTOINCREMENT` なので単調増加
 * かつ再利用されず、`id <= asOfId` で**集合のメンバーが確定する**。
 *
 * **「件数が増えたら全体を取り直す」方式を採らなかった理由**: イベントが
 * 流れ続けている間（接続が瞬断を繰り返しているなど、まさにこの一覧を見たい
 * とき）は毎回取り直しになり、**一覧が永久に読み終わらない**。これも
 * 「回復導線が、必要なときだけ死ぬ」型になる。新しいイベントは利用者が
 * 「再読み込み」した（= 新しい世代を始めた）ときに入る。
 *
 * **将来の制約**: 保持期間による削除（R0 §3.4、**まだ未実装**）が入ると、
 * 古い行（小さい `id`）が消えて末尾側の `OFFSET` がずれる。削除を実装する
 * ときは、この取得方法（サーバー側の `read_events` も含めて）を見直すこと
 * （`/audit-log` は削除があるので、同じ境界の総件数が減ったらスナップショット
 * 失効として採らない - `routes/(app)/audit-log/auditBlocks.ts`）。
 *
 * ### 汎用部との関係（#410）
 *
 * 世代・境界・飛行中・ブロック単位の失敗・世代違いの応答の排除は、
 * `/audit-log` と共通の `$lib/blockCache` に出した。ここに残るのは `/events`
 * 固有の部分だけ: 失敗の型（`Readout` の 3 状態 + 往復の失敗）、境界の
 * 食い違いの扱い（[`BOUNDARY_MISMATCH_MESSAGE`]）、画面に出す値
 * （[`viewState`]）。**挙動は #409 のときから変えていない**（#409 の表テストが
 * そのまま通ることで確かめている）。
 *
 * ### 自動では再試行しない
 *
 * **今の世代で失敗したブロックは、スクロールでは取り直さない**（失敗の直後に
 * 同じ要求が飛び続けるのを避ける）。取り直すのは利用者が「再読み込み」を
 * 押したとき（= 新しい世代）だけで、**失敗の表示と再試行手段は、その
 * ブロックの取得が成功するまで残る** - 別ブロックの成功では消さない。
 */
import type { CollectEventList, CollectEventRow, ReadoutState } from '$lib/banto/collectAdmin';
import {
	BlockLoader,
	applyOutcome as applyGenericOutcome,
	blocksToFetch as genericBlocksToFetch,
	initialCache as genericInitialCache,
	markInFlight as genericMarkInFlight,
	newGeneration as genericNewGeneration,
	type ApplyResult as GenericApplyResult,
	type BlockCache as GenericBlockCache,
	type BlockPolicy,
	type BlockRequest
} from '$lib/blockCache';

export { BLOCK_SIZE, blockRequest, blocksFor, type BlockRequest } from '$lib/blockCache';

/**
 * ブロック 1 つの取得が**行を入れられなかった**ときの持ち方。
 *
 * - `readout`: サーバーは答えたが「読めなかった」/「走っていない」
 *   （`Readout` の 3 状態を空に潰さない）、
 * - `error`: 往復そのものが失敗した / こちらが待つのをやめた。
 */
export type BlockFailure =
	{ kind: 'readout'; readout: Exclude<ReadoutState, 'ready'> } | { kind: 'error'; message: string };

/** ブロック 1 つの取得の結末（`Readout` と往復の失敗を 1 つにしたもの）。 */
export type BlockOutcome = { kind: 'ready'; list: CollectEventList } | BlockFailure;

/** ブロックキャッシュの状態（汎用部の `BlockCache` を `/events` の失敗型で固定）。 */
export type BlockCache = GenericBlockCache<BlockFailure>;

/** 画面に出す値（[`viewState`] が [`BlockCache`] から導く）。 */
export interface EventsViewState {
	loading: boolean;
	/** `null` = まだ一度も読めていない（「0 件」と言い切らない）。 */
	readout: ReadoutState | null;
	errorText: string | null;
	/** 取得に失敗したままのブロック数。 */
	failedBlockCount: number;
	totalCount: number;
}

/** [`applyOutcome`] の結果。行の書き込みは画面（`$state`）に任せる。 */
export type ApplyResult = GenericApplyResult<CollectEventRow, BlockFailure>;

/**
 * 要求した境界と違う境界の応答が返ったときの文言。**採らない**（採ると
 * 重複・欠落が戻る）。サーバーが `asOfId` を無視している場合にここへ来るので、
 * 黙って取り直し続けない（無限の取り直しになる）。
 */
export const BOUNDARY_MISMATCH_MESSAGE =
	'イベントの取得範囲がサーバー側で切り替わりました。「再読み込み」でもう一度読み込んでください。';

/**
 * `/events` の方針: 境界が食い違った応答だけを採らない。**世代は止めない**
 * （#409 の挙動のまま）。総件数の食い違いは見ない - `collect_events` には
 * まだ削除が無い（モジュール doc「将来の制約」）。
 */
const EVENTS_POLICY: BlockPolicy<CollectEventRow, BlockFailure> = {
	reject(snapshot, list) {
		return list.asOfId !== snapshot.asOfId
			? { kind: 'error', message: BOUNDARY_MISMATCH_MESSAGE }
			: null;
	}
};

export function initialCache(): BlockCache {
	return genericInitialCache<BlockFailure>();
}

export function markInFlight(cache: BlockCache, blocks: readonly number[]): BlockCache {
	return genericMarkInFlight(cache, blocks);
}

/** 「再読み込み」= 新しい世代（汎用部の `newGeneration` そのもの。失敗は捨てない）。 */
export function newGeneration(cache: BlockCache): BlockCache {
	return genericNewGeneration(cache);
}

/** **何を取るかを決める唯一の述語**（汎用部の `blocksToFetch` そのもの）。 */
export function blocksToFetch(cache: BlockCache, start: number, end: number): number[] {
	return genericBlocksToFetch(cache, start, end, EVENTS_POLICY);
}

/** 1 ブロックの結末を取り込む（汎用部の `applyOutcome` に `/events` の方針を渡す）。 */
export function applyOutcome(
	cache: BlockCache,
	block: number,
	generation: number,
	outcome: BlockOutcome
): ApplyResult {
	return applyGenericOutcome(cache, block, generation, outcome, EVENTS_POLICY);
}

/**
 * 画面に出す値。**失敗しているブロックが 1 つでもあれば、失敗を出し続ける**
 * （別ブロックが成功していても `readout: 'ready'` で塗り潰さない）。
 */
export function viewState(cache: BlockCache): EventsViewState {
	const failures = [...cache.failed.entries()].sort(([a], [b]) => a - b);
	const readoutFailure = failures.find(([, record]) => record.failure.kind === 'readout');
	const errorFailure = failures.find(([, record]) => record.failure.kind === 'error');
	return {
		loading: cache.inFlight.size > 0,
		readout:
			readoutFailure && readoutFailure[1].failure.kind === 'readout'
				? readoutFailure[1].failure.readout
				: cache.everRead
					? 'ready'
					: null,
		errorText:
			errorFailure && errorFailure[1].failure.kind === 'error'
				? errorFailure[1].failure.message
				: null,
		failedBlockCount: cache.failed.size,
		totalCount: cache.totalCount
	};
}

/** 取得を 1 本走らせる関数（画面は `runWithLimit` + `listCollectEvents`、テストは偽物）。 */
export type BlockFetcher = (request: BlockRequest) => Promise<BlockOutcome>;

/** 画面（`$state`）への書き戻し口。 */
export interface EventBlockSink {
	/** 行の配列をこの長さで作り直す。 */
	resetRows(length: number): void;
	writeRows(offset: number, rows: CollectEventRow[]): void;
	update(view: EventsViewState): void;
}

/**
 * `/events` の駆動役（汎用部の `BlockLoader` に、`/events` の方針と
 * [`viewState`] への変換を渡しただけ）。
 */
export class EventBlockLoader extends BlockLoader<CollectEventRow, BlockFailure> {
	constructor(fetcher: BlockFetcher, sink: EventBlockSink) {
		super(
			fetcher,
			{
				resetRows: (length) => sink.resetRows(length),
				writeRows: (offset, rows) => sink.writeRows(offset, rows),
				update: (cache) => sink.update(viewState(cache))
			},
			EVENTS_POLICY,
			(err) => ({ kind: 'error', message: String(err) })
		);
	}
}
