/**
 * 監視画面のトレンド（R1-D の D-3b。設計書 docs/chronogazer/trend-design.md §8・§10 の 4b で
 * ハイブリッドにした）のデータの持ち主。画面（`/monitor`）が 1 つ持つ。
 *
 * 持つもの（描画には毎回 `hybridData` の結果 [`TrendFeed.data`] を `TrendChart` に渡す）:
 *
 * - **直近**: ペンごとのリングバッファ（`LiveBuffers`）。**現在値のポーラー
 *   （`valuesPoller.svelte.ts`）の結果を書き足す**だけで、自分では現在値を取りに行かない。
 *   `null` の値（bad・stale）は欠測のサンプル。ポーリングの失敗・収集の停止・タブが隠れた間は
 *   画面が [`TrendFeed.breakLive`] を呼び、途切れを知らせる（その範囲は履歴で描く）。
 *   保持時間は `liveRetainMs(時間窓)`、サンプル間隔はタグの収集周期とポーリングの周期の長い方。
 * - **過去**: 表示範囲に要る履歴の区間（`createRangeLoader`、debounce 200 ms・abort）。
 *   区間の数は描画域の幅から（`historyBins`）。いつ読み直すかは `needsHistoryReload`
 *   （追従中は毎秒読まない。拡大・縮小・パンで範囲を出た・区間の幅が変わった・途切れの後）。
 * - **境界の区間**: 履歴の区間が `liveFrom` をまたぐとき `[区間の始まり, liveFrom)` を読み直す
 *   （`boundaryPlan` → `createBoundaryLoader`。結果は `BoundaryValues`、古いキーは `pruneBoundary`）。
 * - **追従**: 追従中は右端を今にして進める。拡大・縮小・パンは [`TrendFeed.rangeChange`]、
 *   「最新へ」は [`TrendFeed.followLatest`]（判断は `trendLogic.ts` の `rangeChangeFollow` ほか）。
 *   追従をやめている間も、現在値の書き足しは止めない。
 *
 * docs/implementation-checklist.md §5 の「バックグラウンドの処理を足したら」:
 *
 * - **識別子が変わったときの持ち越しをしない**: 構成（表示グループ・ペンの並び）が変わったら
 *   リングバッファ・履歴・境界をすべて捨て、世代を進める。前の構成向けの応答は捨てる
 *   （ローダーを作り直し、受けたときも世代を確かめる）。グループをまたいで線を持ち越さない。
 * - **飛行中の要求を止める**: 世代を進めるたびに前のローダーを `dispose` し、飛行中の要求を
 *   abort する（REST はソケットを畳む）。Tauri の `invoke` は止められないので、`runWithLimit` の
 *   打ち切りとローダーの「古い結果を捨てる」で遅れた応答を採らない。
 * - **返ってこない障害**: 履歴の 1 回に上限（[`HISTORY_FETCH_TIMEOUT_MS`]。サーバーの打ち切り
 *   10 秒より少し長い）。打ち切ったら「履歴なし・現在値だけ」（注記）にして先へ進む。
 * - **無駄撃ちの抑制**: 履歴は `needsHistoryReload` が真のときだけ読む（ポーリングしない）。
 *   境界の読み直しは計画が変わったときだけ（失敗した要求は繰り返さない、部品の約束）。幅が
 *   2 ms 未満の境界は、両端を含む形にすると `fromMs == toMs` になりサーバーが 422 で断るので、
 *   呼ばずに欠測にする（[`skipTinyRanges`]）。
 * - 構成を当てた直後の 1 回目の履歴は [`HISTORY_FLUSH_GRACE_MS`] 待つ。収集の書き手は約 1 秒
 *   ごとに flush するので、すぐ読むと `liveFrom` の直前がまだデータファイルに無い。途切れの後に
 *   書き直し始めたとき（`liveFrom` が新しい）の読み直しも同じだけ待つ。
 */
import {
	binMs as binMsOf,
	boundaryPlan,
	createBoundaryLoader,
	createRangeLoader,
	hybridData,
	liveAppendAll,
	liveBreakAll,
	liveRetainMs,
	pruneBoundary,
	type BoundaryLoader,
	type BoundaryValues,
	type LiveBuffers,
	type LiveSample,
	type RangeLoader,
	type TimeRange,
	type TrendData,
	type TrendFetch
} from '@banto-industrial/trend';
import {
	getCollectHistory,
	runWithLimit,
	type CollectHistory,
	type CollectHistoryParams,
	type Readout
} from '../banto/collectAdmin';
import { createHistoryFetch, historyBins, type HistoryExtras } from '../historical/historyLogic';
import {
	needsHistoryReload,
	rangeChangeFollow,
	startFollow,
	tickFollow,
	type FollowState,
	type HistoryRequested,
	type TrendHistoryState
} from './trendLogic';
import { trendPenId } from './trendPens';

/** 履歴 1 回の上限（ms）。サーバーの `HISTORY_READ_TIMEOUT`（10 秒）+ 余裕。 */
export const HISTORY_FETCH_TIMEOUT_MS = 12_000;
/** 構成を当ててから履歴を要求するまでの待ち（ms）。書き手の flush（約 1 秒）+ 余裕。 */
export const HISTORY_FLUSH_GRACE_MS = 2_000;
/** 表示範囲が止まってから履歴・境界を読むまでの待ち（ms、設計書 §8）。 */
export const TREND_HISTORY_DEBOUNCE_MS = 200;
/** 履歴の口に渡せる範囲の幅の下限（ms）。これ未満は両端を含む形で `fromMs == toMs` になる。 */
export const MIN_HISTORY_RANGE_MS = 2;

export type HistoryFetch = (
	params: CollectHistoryParams,
	signal: AbortSignal
) => Promise<Readout<CollectHistory>>;

/** 構成。グループとペンの並びが変わったら作り直す（[`trendConfigKey`]）。 */
export interface TrendConfig {
	groupId: number;
	/** ペンの並びどおりのタグ ID。 */
	penTagIds: readonly number[];
	/** 時間窓（追従の幅・リングバッファの保持時間）。変わったら追従に戻す。 */
	windowMs: number;
	/** 描画域の幅（px）。履歴の区間の数を決める。 */
	plotWidthPx: number;
	/** ペンの並びどおりのサンプル間隔（ms。収集周期とポーリングの周期の長い方）。 */
	sampleIntervalMs: readonly (number | null)[];
}

export function trendConfigKey(config: Pick<TrendConfig, 'groupId' | 'penTagIds'>): string {
	return `${config.groupId}|${config.penTagIds.join(',')}`;
}

/**
 * 幅が [`MIN_HISTORY_RANGE_MS`] 未満の範囲はサーバーを呼ばず、空の結果（欠測）を返す。
 * 境界の区間の読み直しに使う。
 */
export function skipTinyRanges(fetch: TrendFetch): TrendFetch {
	return (req) =>
		req.range.endMs - req.range.startMs < MIN_HISTORY_RANGE_MS ? Promise.resolve({}) : fetch(req);
}

function abortError(): Error {
	const e = new Error('aborted');
	e.name = 'AbortError';
	return e;
}

/**
 * 履歴の読み出しに上限を付ける。打ち切り・失敗は投げ、`ready` でないものは
 * `createHistoryFetch` が `HistoryUnavailableError` にする。外の `signal` の abort は内へ伝える。
 */
function withLimit(fetch: HistoryFetch, timeoutMs: number): HistoryFetch {
	return async (params, signal) => {
		const outcome = await runWithLimit((limitSignal) => {
			const controller = new AbortController();
			const abort = () => controller.abort();
			if (signal.aborted) abort();
			signal.addEventListener('abort', abort, { once: true });
			limitSignal.addEventListener('abort', abort, { once: true });
			return fetch(params, controller.signal);
		}, timeoutMs);
		if (outcome.kind === 'ok') return outcome.value;
		if (signal.aborted) throw abortError();
		if (outcome.kind === 'timedOut') throw new Error('履歴の読み出しが時間切れになりました');
		throw outcome.error;
	};
}

export interface TrendFeedOptions {
	fetchHistory?: HistoryFetch;
	timeoutMs?: number;
	graceMs?: number;
	debounceMs?: number;
}

function sameNumbers(a: Readonly<Record<string, number>>, b: Readonly<Record<string, number>>) {
	const ka = Object.keys(a);
	return ka.length === Object.keys(b).length && ka.every((k) => a[k] === b[k]);
}

export class TrendFeed {
	/** 今の構成のキー（`null` = 未構成）。 */
	key = $state<string | null>(null);
	/** 今の構成の表示グループ（画面が値を書き足してよいかの確認に使う）。 */
	groupId = $state<number | null>(null);
	/** ペンの並びどおりの部品のペンの ID（`trendPenId`）。 */
	penIds = $state.raw<readonly string[]>([]);
	live = $state.raw<LiveBuffers>({});
	/** 履歴（読み出し中・失敗なら `null`）。 */
	history = $state.raw<TrendData | null>(null);
	boundary = $state.raw<BoundaryValues>({});
	historyState = $state<TrendHistoryState>('idle');
	/** 追従の状態（`null` = 未構成）。 */
	follow = $state.raw<FollowState | null>(null);
	/** 最後に受けた表示の時計（サーバーの時刻。`null` = まだ無い）。 */
	nowMs = $state<number | null>(null);
	/** 履歴の区間の数。 */
	bins = $state(1);
	/** ペン ID → サンプル間隔（ms）。 */
	sampleIntervalMs = $state.raw<Readonly<Record<string, number>>>({});
	/** 履歴に居なかったタグ（登録が見つからない）。 */
	unknownTagIds = $state.raw<number[]>([]);
	/** 履歴の系列が `simulation` のタグ。 */
	simulationTagIds = $state.raw<number[]>([]);

	/** 表示範囲（`null` = 未構成）。 */
	readonly view = $derived(this.follow?.view ?? null);
	/** 追従中か（未構成なら true）。 */
	readonly following = $derived(this.follow?.following ?? true);
	/** 描画データ（履歴とリングバッファのつなぎ合わせ）。 */
	readonly data = $derived.by((): TrendData | null => {
		const view = this.view;
		if (view === null || this.penIds.length === 0) return null;
		return hybridData({
			history: this.history,
			live: this.live,
			penIds: this.penIds,
			binMs: binMsOf(view, this.bins),
			boundary: this.boundary,
			sampleIntervalMs: this.sampleIntervalMs,
			nowMs: this.nowMs ?? undefined
		});
	});

	readonly #fetch: HistoryFetch;
	readonly #graceMs: number;
	readonly #debounceMs: number;
	#generation = 0;
	#windowMs = 0;
	#loader: RangeLoader | null = null;
	#boundaryLoader: BoundaryLoader | null = null;
	/** 1 回目の履歴の待ちが済んだか。 */
	#graceDone = false;
	#graceTimer: ReturnType<typeof setTimeout> | null = null;
	/** 書き直し始めた直後の読み直しを待つタイマ。 */
	#retryTimer: ReturnType<typeof setTimeout> | null = null;
	/** 最後に出した履歴の要求（失敗しても残す。範囲を出るまで繰り返さない）。 */
	#requested: HistoryRequested | null = null;
	/**
	 * 書き足し済みの現在値のスナップショット（ポーラーは成功のたびに新しいオブジェクトを
	 * 作る）。同じスナップショットを 2 回書かない。構成を作り直したら `null` に戻す。
	 */
	#appliedSnapshot: object | null = null;

	constructor(options: TrendFeedOptions = {}) {
		const raw = options.fetchHistory ?? ((params, signal) => getCollectHistory(params, signal));
		this.#fetch = withLimit(raw, options.timeoutMs ?? HISTORY_FETCH_TIMEOUT_MS);
		this.#graceMs = options.graceMs ?? HISTORY_FLUSH_GRACE_MS;
		this.#debounceMs = options.debounceMs ?? TREND_HISTORY_DEBOUNCE_MS;
	}

	/**
	 * 構成を当てる。構成が変わったら全部捨てて作り直し（追従から始め、履歴は
	 * [`HISTORY_FLUSH_GRACE_MS`] 後に読む）、`true` を返す。同じ構成なら、時間窓が変わったときは
	 * 追従に戻し、追従中は右端を `nowMs` に進める。
	 */
	configure(config: TrendConfig, rawNowMs: number): boolean {
		// 時計は小数を含みうる（performance.now）。REST・Tauri の口は i64 なので入口で整数にそろえる。
		const nowMs = Math.floor(rawNowMs);
		this.nowMs = nowMs;
		const key = trendConfigKey(config);
		const rebuilt = key !== this.key;
		if (rebuilt) {
			this.#rebuild(config, key, nowMs);
		} else if (config.windowMs !== this.#windowMs) {
			this.#windowMs = config.windowMs;
			this.follow = startFollow(config.windowMs, nowMs);
		} else if (this.follow !== null) {
			this.follow = tickFollow(this.follow, nowMs);
		}
		const bins = historyBins(config.plotWidthPx, new Set(config.penTagIds).size);
		if (bins !== this.bins) this.bins = bins;
		const intervals: Record<string, number> = {};
		this.penIds.forEach((id, i) => {
			const v = config.sampleIntervalMs[i];
			if (typeof v === 'number' && Number.isFinite(v) && v > 0) intervals[id] = v;
		});
		if (!sameNumbers(intervals, this.sampleIntervalMs)) this.sampleIntervalMs = intervals;
		this.#maybeLoad();
		return rebuilt;
	}

	/**
	 * 構成を当て、まだ書いていないスナップショットなら書き足す（画面はこれだけを呼ぶ）。
	 * `snapshot` は現在値の表（同じオブジェクトは 2 回書かない）、`values` はペンの並び
	 * どおりの値（`null` = 欠測）、`nowMs` は表示の時計（`serverClockNow`）。
	 */
	sync(
		config: TrendConfig,
		rawNowMs: number,
		snapshot: object,
		values: readonly (number | null)[]
	): void {
		const nowMs = Math.floor(rawNowMs);
		this.configure(config, nowMs);
		if (snapshot === this.#appliedSnapshot) return;
		this.#appliedSnapshot = snapshot;
		this.append(nowMs, values);
	}

	/** 現在値を 1 回分書き足す（`values` はペンの並びどおり、`nowMs` はサーバーの時計）。 */
	append(rawNowMs: number, values: readonly (number | null)[]): void {
		if (this.key === null) return;
		const nowMs = Math.floor(rawNowMs); // 整数のミリ秒にそろえる（configure と同じ）。
		const samples: Record<string, LiveSample> = {};
		this.penIds.forEach((id, i) => {
			const v = values[i];
			samples[id] = { tMs: nowMs, value: v === undefined ? null : v };
		});
		const next = liveAppendAll(this.live, samples, { retainMs: liveRetainMs(this.#windowMs) });
		if (next === this.live) return;
		this.live = next;
		this.#refreshBoundary();
		this.#maybeLoad();
	}

	/** 途切れを知らせる（ポーリングの失敗・収集の停止・タブが隠れた）。 */
	breakLive(): void {
		const next = liveBreakAll(this.live);
		if (next === this.live) return;
		this.live = next;
		this.#refreshBoundary();
	}

	/** 拡大・縮小・パンの結果（`TrendChart` の `onRangeChange`）。 */
	rangeChange(next: TimeRange): void {
		if (this.follow === null || this.nowMs === null) return;
		const follow = rangeChangeFollow(this.follow, next, this.nowMs);
		if (follow === this.follow) return;
		this.follow = follow;
		this.#maybeLoad();
	}

	/** 「最新へ」: 時間窓の幅で追従に戻す。 */
	followLatest(): void {
		if (this.follow === null || this.nowMs === null) return;
		this.follow = startFollow(this.#windowMs, this.nowMs);
		this.#maybeLoad();
	}

	/** 止めて捨てる（画面を離れた・トレンドでないグループになった）。 */
	reset(): void {
		this.#cancel();
		this.#appliedSnapshot = null;
		this.#requested = null;
		this.key = null;
		this.groupId = null;
		this.penIds = [];
		this.live = {};
		this.history = null;
		this.boundary = {};
		this.follow = null;
		this.nowMs = null;
		this.sampleIntervalMs = {};
		this.historyState = 'idle';
		this.unknownTagIds = [];
		this.simulationTagIds = [];
	}

	#rebuild(config: TrendConfig, key: string, nowMs: number): void {
		this.#cancel();
		const generation = this.#generation;
		this.#appliedSnapshot = null;
		this.#requested = null;
		this.#graceDone = false;
		this.#windowMs = config.windowMs;
		this.key = key;
		this.groupId = config.groupId;
		this.penIds = config.penTagIds.map((tagId, i) => trendPenId(i, tagId));
		this.live = {};
		this.history = null;
		this.boundary = {};
		this.follow = startFollow(config.windowMs, nowMs);
		this.unknownTagIds = [];
		this.simulationTagIds = [];
		this.historyState = 'loading';

		const current = () => generation === this.#generation;
		const extras = new WeakMap<object, HistoryExtras>();
		this.#loader = createRangeLoader({
			fetch: createHistoryFetch(this.#fetch, extras),
			debounceMs: this.#debounceMs,
			onResult: (data) => {
				if (!current()) return;
				const extra = extras.get(data.series);
				const tagIds = config.penTagIds;
				this.history = data;
				this.unknownTagIds = (extra?.unknownTagIds ?? []).filter((id) => tagIds.includes(id));
				this.simulationTagIds = extra?.simulationTagIds ?? [];
				this.historyState = 'ready';
				this.#refreshBoundary();
			},
			onError: () => {
				if (!current()) return;
				this.history = null;
				this.historyState = 'unavailable';
				this.#refreshBoundary();
			}
		});
		this.#boundaryLoader = createBoundaryLoader({
			fetch: skipTinyRanges(createHistoryFetch(this.#fetch, new WeakMap())),
			debounceMs: this.#debounceMs,
			onResult: (values) => {
				if (!current()) return;
				this.boundary = { ...this.boundary, ...values };
			}
		});
		this.#graceTimer = setTimeout(() => {
			this.#graceTimer = null;
			if (!current()) return;
			this.#graceDone = true;
			this.#maybeLoad();
		}, this.#graceMs);
	}

	#cancel(): void {
		this.#generation += 1;
		if (this.#graceTimer !== null) clearTimeout(this.#graceTimer);
		if (this.#retryTimer !== null) clearTimeout(this.#retryTimer);
		this.#graceTimer = null;
		this.#retryTimer = null;
		this.#graceDone = false;
		this.#loader?.dispose();
		this.#boundaryLoader?.dispose();
		this.#loader = null;
		this.#boundaryLoader = null;
	}

	/** 境界の計画を当て直す（データが変わるたび。同じ計画なら部品は読み直さない）。 */
	#refreshBoundary(): void {
		if (this.#boundaryLoader === null) return;
		const src = { history: this.history, live: this.live, penIds: this.penIds };
		const pruned = pruneBoundary(this.boundary, src);
		if (pruned !== this.boundary) this.boundary = pruned;
		this.#boundaryLoader.request(boundaryPlan(src, pruned));
	}

	/** 履歴を読み直す必要があれば要求する（`needsHistoryReload`）。 */
	#maybeLoad(): void {
		const view = this.view;
		if (!this.#graceDone || this.#loader === null || view === null) return;
		const binMs = binMsOf(view, this.bins);
		const liveFroms = this.penIds.map((id) => this.live[id]?.liveFrom ?? null);
		if (
			!needsHistoryReload({
				requested: this.#requested,
				view,
				binMs,
				following: this.following,
				liveFroms
			})
		) {
			return;
		}
		// 途切れの後に書き直し始めた直後（liveFrom が要求した範囲より後で新しい）は、flush を
		// 待ってから読む（1 回目は構成を当てたときの待ちで済んでいる）。
		const now = this.nowMs;
		const requestedEnd = this.#requested?.range.endMs ?? Infinity;
		const newest = Math.max(
			...liveFroms.map((f) => (f !== null && f > requestedEnd ? f : -Infinity))
		);
		if (now !== null && newest > now - this.#graceMs) {
			if (this.#retryTimer === null) {
				const generation = this.#generation;
				this.#retryTimer = setTimeout(
					() => {
						this.#retryTimer = null;
						if (generation === this.#generation) this.#maybeLoad();
					},
					newest + this.#graceMs - now
				);
			}
			return;
		}
		this.#requested = { range: view, binMs };
		this.historyState = 'loading';
		this.#loader.request({ range: view, penIds: this.penIds, bins: this.bins });
	}
}
