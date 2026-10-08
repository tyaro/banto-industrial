/**
 * リアルタイムトレンド（R1-D の D-3b）のバッファの持ち主。画面（`/monitor`）が
 * 1 つ持ち、**現在値のポーラー（`valuesPoller.svelte.ts`）の結果を書き足す**だけで、
 * 自分では現在値を取りに行かない（ポーラーは D-1 のものを使い回す）。自分で取りに
 * 行くのは初期窓の履歴（D-3a の `getCollectHistory`）だけ。判断は `trendLogic.ts`。
 *
 * docs/implementation-checklist.md §5 の「バックグラウンドの処理を足したら」:
 *
 * - **識別子が変わったときの持ち越しをしない**: 構成（表示グループ・時間窓・刻み・
 *   ペンの並び）が変わったら格子を作り直し、世代を進める。前の構成向けに飛んで
 *   いた履歴の応答は捨てる（`isPollGenerationCurrent`）。グループをまたいで線を
 *   持ち越さない。
 * - **飛行中の要求を止める**: 世代を進めるたびに前の要求の `AbortController` を
 *   abort する（REST はソケットを畳む）。Tauri の `invoke` は止められないので、
 *   `runWithLimit` の打ち切りと世代の確認で遅れた応答を採らない。
 * - **返ってこない障害**: 履歴の 1 回に上限（[`HISTORY_FETCH_TIMEOUT_MS`]。サーバーの
 *   打ち切り 10 秒より少し長い）。打ち切ったら「履歴なし・現在値だけ」にして先へ進む。
 * - **無駄撃ちの抑制**: 履歴は構成が変わったときに 1 回だけ読む（ポーリングしない）。
 *   以後は現在値で足す。
 * - 履歴の要求は構成を変えてから [`HISTORY_FLUSH_GRACE_MS`] 待つ。収集の書き手は
 *   約 1 秒ごとに flush するので、すぐ読むと「現在値を書き始めた刻み」の直前が
 *   まだデータファイルに無く、履歴と現在値の境目で線が切れる。
 */
import {
	getCollectHistory,
	isPollGenerationCurrent,
	runWithLimit,
	type CollectHistory,
	type CollectHistoryParams,
	type Readout
} from '../banto/collectAdmin';
import {
	appendLive,
	emptyTrendBuffer,
	historyRequest,
	mergeHistory,
	type TrendBuffer,
	type TrendHistoryState
} from './trendLogic';

/** 履歴 1 回の上限（ms）。サーバーの `HISTORY_READ_TIMEOUT`（10 秒）+ 余裕。 */
export const HISTORY_FETCH_TIMEOUT_MS = 12_000;
/** 構成を変えてから履歴を要求するまでの待ち（ms）。書き手の flush（約 1 秒）+ 余裕。 */
export const HISTORY_FLUSH_GRACE_MS = 2_000;

export type HistoryFetch = (
	params: CollectHistoryParams,
	signal: AbortSignal
) => Promise<Readout<CollectHistory>>;

/** 格子の構成。どれかが変わったら作り直す。 */
export interface TrendConfig {
	groupId: number;
	windowMs: number;
	stepMs: number;
	/** ペンの並びどおりのタグ ID。 */
	penTagIds: readonly number[];
	/** タグの収集周期（履歴の点を次の点まで有効にする長さ、`mergeHistory`）。 */
	periodMsOf?: (tagId: number) => number | null;
}

export function trendConfigKey(config: TrendConfig): string {
	return `${config.groupId}|${config.windowMs}|${config.stepMs}|${config.penTagIds.join(',')}`;
}

export interface TrendFeedOptions {
	fetchHistory?: HistoryFetch;
	timeoutMs?: number;
	graceMs?: number;
}

export class TrendFeed {
	/** 今の構成のキー（`null` = 未構成）。 */
	key = $state<string | null>(null);
	/** 今の構成の表示グループ（画面が値を書き足してよいかの確認に使う）。 */
	groupId = $state<number | null>(null);
	buffer = $state.raw<TrendBuffer | null>(null);
	historyState = $state<TrendHistoryState>('idle');
	/** 履歴に居なかったタグ（登録が見つからない）。 */
	unknownTagIds = $state.raw<number[]>([]);
	/** 履歴の系列が `simulation` のタグ。 */
	simulationTagIds = $state.raw<number[]>([]);

	readonly #fetch: HistoryFetch;
	readonly #timeoutMs: number;
	readonly #graceMs: number;
	#generation = 0;
	#controller: AbortController | null = null;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#config: TrendConfig | null = null;

	constructor(options: TrendFeedOptions = {}) {
		this.#fetch = options.fetchHistory ?? ((params, signal) => getCollectHistory(params, signal));
		this.#timeoutMs = options.timeoutMs ?? HISTORY_FETCH_TIMEOUT_MS;
		this.#graceMs = options.graceMs ?? HISTORY_FLUSH_GRACE_MS;
	}

	/**
	 * 構成を当てる。同じ構成なら何もしない。変わったら格子を `nowMs`（サーバーの時計）
	 * で作り直し、履歴を [`HISTORY_FLUSH_GRACE_MS`] 後に 1 回読む。
	 */
	configure(config: TrendConfig, nowMs: number): void {
		const key = trendConfigKey(config);
		if (key === this.key) {
			this.#config = config;
			return;
		}
		this.#cancel();
		const generation = this.#generation;
		this.#config = config;
		this.key = key;
		this.groupId = config.groupId;
		this.buffer = emptyTrendBuffer(config.stepMs, config.windowMs, config.penTagIds.length, nowMs);
		this.unknownTagIds = [];
		this.simulationTagIds = [];
		this.historyState = 'loading';
		this.#timer = setTimeout(() => {
			this.#timer = null;
			void this.#loadHistory(generation);
		}, this.#graceMs);
	}

	/** 現在値を 1 回分書き足す（`values` はペンの並びどおり、`nowMs` はサーバーの時計）。 */
	append(nowMs: number, values: readonly (number | null)[]): void {
		if (this.buffer === null) return;
		this.buffer = appendLive(this.buffer, nowMs, values);
	}

	/** 止めて捨てる（画面を離れた・トレンドでないグループになった）。 */
	reset(): void {
		this.#cancel();
		this.#config = null;
		this.key = null;
		this.groupId = null;
		this.buffer = null;
		this.historyState = 'idle';
		this.unknownTagIds = [];
		this.simulationTagIds = [];
	}

	#cancel(): void {
		this.#generation += 1;
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		this.#controller?.abort();
		this.#controller = null;
	}

	async #loadHistory(generation: number): Promise<void> {
		const config = this.#config;
		const buffer = this.buffer;
		if (config === null || buffer === null) return;
		const params = historyRequest(buffer, config.penTagIds);
		if (params === null) {
			this.historyState = 'ready';
			return;
		}
		const outcome = await runWithLimit((signal) => {
			// 世代を進めたら abort できるよう、runWithLimit の signal を外からも引く。
			const controller = new AbortController();
			signal.addEventListener('abort', () => controller.abort(), { once: true });
			this.#controller = controller;
			return this.#fetch(params, controller.signal);
		}, this.#timeoutMs);
		if (!isPollGenerationCurrent(generation, this.#generation)) return;
		this.#controller = null;
		if (outcome.kind !== 'ok' || outcome.value.state !== 'ready') {
			this.historyState = 'unavailable';
			return;
		}
		const history = outcome.value.data;
		const current = this.buffer;
		if (current !== null) {
			this.buffer = mergeHistory(current, config.penTagIds, history, config.periodMsOf);
		}
		this.unknownTagIds = history.unknownTagIds.filter((id) => config.penTagIds.includes(id));
		this.simulationTagIds = history.series.filter((s) => s.simulation).map((s) => s.tagId);
		this.historyState = 'ready';
	}
}
