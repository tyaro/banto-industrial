/**
 * タグモニタのストリームの表示状態と、表のセルの表示（#441 のレビュー対応）。
 *
 * モニタ（`routes/(app)/monitor/+page.svelte`）が `connectTagStream` の
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
}

export type StreamViewEvent =
	| { type: 'connected' }
	| { type: 'disconnected'; code?: number }
	| { type: 'data' }
	| { type: 'resubscribed' }
	| { type: 'halted'; action: StreamHalt }
	/** 手動の再開、またはログイン状態の確認で有効と分かって再開した。 */
	| { type: 'resumed' }
	/** ログイン状態の確認そのものが失敗した（`invalidateAll()` の reject）。 */
	| { type: 'recheckFailed'; reason: string };

/** `recheckFailed` のときの説明。 */
export const RECHECK_FAILED_MESSAGE =
	'ログイン状態を確認できなかったため、リアルタイム更新を止めています。';

export function initialStreamView(): StreamViewState {
	return { connected: false, backpressure: false, awaitingSnapshot: true, halt: null };
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
			return { ...state, awaitingSnapshot: false };
		case 'resubscribed':
			return { ...state, awaitingSnapshot: true };
		case 'halted':
			return { ...state, halt: event.action };
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
}

export interface MonitorCellDisplay {
	/** 値の列に出す文字列。 */
	value: string;
	/** 色分けの区分（`good` / `bad` / `stale`）。 */
	qualityClass: 'good' | 'bad' | 'stale';
	/** 品質の列に出す文字列。 */
	qualityLabel: string;
}

const QUALITY_LABELS: Readonly<Record<string, string>> = {
	good: '良好',
	bad: '不良',
	stale: '陳腐化'
};

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
				qualityLabel: `${qualityLabel('stale')}（受信時: ${qualityLabel(row.q)}）`
			};
	}
}

/** 値と品質の列名。止まっている間は「最終受信値」「受信時の品質」と分かる名前にする。 */
export function monitorColumnLabels(mode: CellDisplayMode): { value: string; quality: string } {
	return mode === 'halted'
		? { value: '最終受信値', quality: '品質（受信時）' }
		: { value: '値', quality: '品質' };
}
