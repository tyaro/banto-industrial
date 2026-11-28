/**
 * タグモニタのストリームの表示状態と、表のセルの表示（#441 のレビュー対応）。
 *
 * タグ登録の表（`routes/(app)/tags/+page.svelte`、`tagValueStream.svelte.ts` 経由。もとはタグモニタ `/monitor`）が `connectTagStream` の
 * コールバックから受けた出来事を {@link streamViewReducer} で状態へ畳み、
 * 表のセルは {@link monitorCellDisplay} で決める。どちらも純関数にして
 * `monitorStreamView.test.ts` で表にして確かめる。
 *
 * 表示のモード（{@link cellDisplayMode}）は 3 つ:
 *
 * | モード | いつ | 値 | 品質 |
 * | --- | --- | --- | --- |
 * | `live` | 接続中でスナップショットを受けた後、または通常の再接続中 | 受信した値 | 受信した品質 |
 * | `awaitingSnapshot` | 接続・購読の変更・再開の直後、最初の値を受けるまで | `--` | 陳腐化 |
 * | `halted` | close `1008` で止まっている間（手動の再開待ち・ログイン状態の確認中） | 最終受信値（列名も「最終受信値」） | 陳腐化（受信時: …） |
 *
 * `halted` を別にしたのは、`1008` の後は自動で再接続しない（#441）ので、
 * 最後の値と品質「良好」が長く残り、今の値に見えてしまうため。生の
 * `row.v` / `q` / `t` は書き換えず、表示だけを変える（再開してスナップ
 * ショットが届けば通常の表示へ戻る）。通常の再接続中（`1006` など）の表示は
 * 従来どおり（状態の行が「値は最後の受信内容のまま停止しています」と出す）。
 *
 * 購読の拒否（#1254、PR #1282 のレビュー）: サーバーが subscribe を `error` フレームで
 * 拒否した（`connectTagStream` の `onRejected`）ら `rejected` に code と detail を持つ。
 * ソケットは開いたままでも値は来ないので、状態の行は「更新中」と出さず警告にし、セルは
 * 最初の値の待ちと同じ `--`（陳腐化）にする。次の購読（購読し直し・接続し直し）の `data`
 * を受けたら解く（拒否されたままの間は、接続し直しても `data` が来ないので解けない）。
 * 状態の行の「再接続」ボタン（`resumable`）で購読し直せる。
 */
import type { StreamCloseAction } from './streamClose';

export type StreamHalt = Exclude<StreamCloseAction, { kind: 'reconnect' }>;

export interface StreamViewState {
	/** ソケットが開いている。 */
	connected: boolean;
	/** 直近の切断がバックプレッシャ切断（`1013`）。再接続の成功で解除。 */
	backpressure: boolean;
	/** 接続・購読の変更・再開の直後で、最初の値をまだ受けていない。 */
	awaitingSnapshot: boolean;
	/** close `1008` で止まっている理由。止まっていなければ `null`。 */
	halt: StreamHalt | null;
	/** 今の購読がサーバーに拒否された（`error` フレーム）。拒否されていなければ `null`。 */
	rejected: StreamRejection | null;
}

/** 購読の拒否（`{ op: "error", code, detail }`）。 */
export interface StreamRejection {
	code: string;
	detail: string;
}

export type StreamViewEvent =
	| { type: 'connected' }
	| { type: 'disconnected'; code?: number }
	| { type: 'data' }
	| { type: 'resubscribed' }
	| { type: 'halted'; action: StreamHalt }
	/** 今の購読への subscribe がサーバーに拒否された。 */
	| { type: 'rejected'; code: string; detail: string }
	/** 手動の再開、またはログイン状態の確認で有効と分かって再開した。 */
	| { type: 'resumed' }
	/** ログイン状態の確認そのものが失敗した（`refreshAll()` の reject）。 */
	| { type: 'recheckFailed'; reason: string };

/** `recheckFailed` のときの説明。 */
export const RECHECK_FAILED_MESSAGE =
	'ログイン状態を確認できなかったため、リアルタイム更新を止めています。';

export function initialStreamView(): StreamViewState {
	return {
		connected: false,
		backpressure: false,
		awaitingSnapshot: true,
		halt: null,
		rejected: null
	};
}

export function streamViewReducer(state: StreamViewState, event: StreamViewEvent): StreamViewState {
	switch (event.type) {
		case 'connected':
			return { ...state, connected: true, backpressure: false, awaitingSnapshot: true };
		case 'disconnected':
			return {
				...state,
				connected: false,
				backpressure: event.code === 1013 ? true : state.backpressure
			};
		case 'data':
			return { ...state, awaitingSnapshot: false, rejected: null };
		case 'resubscribed':
			return { ...state, awaitingSnapshot: true };
		case 'halted':
			return { ...state, halt: event.action };
		case 'rejected':
			return {
				...state,
				awaitingSnapshot: true,
				rejected: { code: event.code, detail: event.detail }
			};
		case 'resumed':
			// 次のスナップショットまでは今の値として見せない（再開の直後に
			// 「良好」の古い値へ戻らないように）。
			return { ...state, halt: null, awaitingSnapshot: true };
		case 'recheckFailed':
			return {
				...state,
				halt: { kind: 'halt', reason: event.reason, message: RECHECK_FAILED_MESSAGE }
			};
	}
}

export type CellDisplayMode = 'live' | 'awaitingSnapshot' | 'halted';

export function cellDisplayMode(state: StreamViewState): CellDisplayMode {
	if (state.halt !== null && !state.connected) return 'halted';
	if (state.awaitingSnapshot) return 'awaitingSnapshot';
	return 'live';
}

/** 表の 1 行のうち、表示に使う部分。 */
export interface MonitorCellRow {
	v: number | null;
	q: string;
	unit?: string | null;
	/** 起動後に一度も良好値を受けていない（`q` は良好でない）。 */
	unverified?: boolean;
}

export interface MonitorCellDisplay {
	/** 値の列に出す文字列。 */
	value: string;
	/** 色分けの区分（`good` / `bad` / `stale` / `unverified`）。 */
	qualityClass: 'good' | 'bad' | 'stale' | 'unverified';
	/** 品質の列に出す文字列。 */
	qualityLabel: string;
}

const QUALITY_LABELS: Readonly<Record<string, string>> = {
	good: '良好',
	bad: '不良',
	stale: '陳腐化'
};

/**
 * 未確認 = 起動後にまだ一度も実機から良好な品質の値を受けていない（収集開始後の不良と区別する）。
 * 応答はあっても Uncertain（OPC UA）しか来ていないタグもここに入る。
 */
export const UNVERIFIED_LABEL = '未確認';
/** 「未確認」の説明（品質のセルの `title`）。 */
export const UNVERIFIED_TITLE =
	'未確認 = Hub の起動後に、まだ一度も実機から良好な品質の値を受けていない状態（収集開始後の不良とは別）';

export function qualityLabel(q: string): string {
	return Object.hasOwn(QUALITY_LABELS, q) ? QUALITY_LABELS[q] : q;
}

/** status/+page.svelte と同じ規約: good=通常, bad=danger, stale=muted。 */
export function qualityClass(q: string): 'good' | 'bad' | 'stale' {
	if (q === 'bad') return 'bad';
	if (q === 'stale') return 'stale';
	return 'good';
}

/** 品質が良好で値があるときだけ値を出す（それ以外は `--`）。 */
export function formatValue(row: MonitorCellRow): string {
	if (row.q !== 'good' || row.v === null) return '--';
	return row.unit ? `${row.v} ${row.unit}` : String(row.v);
}

export function monitorCellDisplay(row: MonitorCellRow, mode: CellDisplayMode): MonitorCellDisplay {
	switch (mode) {
		case 'live':
			if (row.unverified && row.q !== 'good') {
				return { value: '--', qualityClass: 'unverified', qualityLabel: UNVERIFIED_LABEL };
			}
			return {
				value: formatValue(row),
				qualityClass: qualityClass(row.q),
				qualityLabel: qualityLabel(row.q)
			};
		case 'awaitingSnapshot':
			return { value: '--', qualityClass: 'stale', qualityLabel: qualityLabel('stale') };
		case 'halted':
			return {
				value: formatValue(row),
				qualityClass: 'stale',
				qualityLabel: `${qualityLabel('stale')}（受信時: ${
					row.unverified && row.q !== 'good' ? UNVERIFIED_LABEL : qualityLabel(row.q)
				}）`
			};
	}
}

/** 値と品質の列名。止まっている間は「最終受信値」「受信時の品質」と分かる名前にする。 */
export function monitorColumnLabels(mode: CellDisplayMode): { value: string; quality: string } {
	return mode === 'halted'
		? { value: '最終受信値', quality: '品質（受信時）' }
		: { value: '値', quality: '品質' };
}

/**
 * ストリームの状態の行（モニタの `data-testid="monitor-stream-status"` の行と同じ文言）。
 * #1255 でタグ登録の表にも値の列を足したので、文言の選び方を純関数にして両方で同じにする。
 *
 * - `warning`: バックプレッシャ切断・止まっている間（色を変える）。
 * - `resumable`: close `1008` の `halt`、または購読の拒否（`rejected`。押すと購読し直す）。
 *   利用者の操作で再開できる。ボタンを出す。
 */
export interface StreamStatusLine {
	kind: 'connected' | 'rejected' | 'recheckSession' | 'halt' | 'backpressure' | 'reconnecting';
	text: string;
	warning: boolean;
	resumable: boolean;
}

export function streamStatusLine(view: StreamViewState): StreamStatusLine {
	if (view.connected && view.rejected !== null) {
		const { code, detail } = view.rejected;
		return {
			kind: 'rejected',
			text: `サーバーが購読を拒否しました（${code}${detail === '' ? '' : `: ${detail}`}）。リアルタイム更新は止まっています`,
			warning: true,
			resumable: true
		};
	}
	if (view.connected) {
		return {
			kind: 'connected',
			text: '接続中（リアルタイム更新中）',
			warning: false,
			resumable: false
		};
	}
	if (view.halt?.kind === 'recheckSession') {
		return {
			kind: 'recheckSession',
			text: 'ログイン状態を確認しています…（リアルタイム更新は止まっています）',
			warning: true,
			resumable: false
		};
	}
	if (view.halt?.kind === 'halt') {
		return { kind: 'halt', text: view.halt.message, warning: true, resumable: true };
	}
	if (view.backpressure) {
		return {
			kind: 'backpressure',
			text: 'サーバーが遅い購読者として切断しました（自動再接続中…）',
			warning: true,
			resumable: false
		};
	}
	return {
		kind: 'reconnecting',
		text: '再接続中…（値は最後の受信内容のまま停止しています）',
		warning: false,
		resumable: false
	};
}
