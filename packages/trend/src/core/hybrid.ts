/**
 * ハイブリッドのつなぎ合わせ（設計書 §8）: 履歴の区間（過去）とリングバッファ（直近、`live.ts`）を、
 * ペンごとの `liveFrom` で 1 本の `TrendData` にする。
 *
 * つなぎ方（ペンごと）:
 * - `liveFrom` より前は履歴の区間、`liveFrom` 以後はリングバッファ。`liveFrom` 以後の履歴の区間は
 *   使わない（線を 1 本に。履歴の最後の区間を境にしない）。`liveFrom` が null（始まりを待っている）
 *   なら履歴だけ。
 * - 履歴の区間が `liveFrom` をまたぐ（`tMs < liveFrom < 区間の終わり`。終わりは `binEndMs` と取得範囲の
 *   終わりの小さい方）ときは、その区間を履歴のまま使わず、`[区間の始まり, liveFrom)` を読み直した値
 *   （`boundaryPlan` で計画し、`boundaryValuesFrom` で覚える）で置き換える。**読み直しがまだなら、
 *   その区間は欠測（min・max とも null）にして線を切る**。履歴のまま残すと `liveFrom` 以後の値を
 *   二重に数え（§8 の (4)）、落とすと線が区間 1 つぶん飛ぶうえ縦軸もずれる。読み直しは 200 ms ほどで
 *   届くので、一瞬の切れ目のほうが害が小さい。
 * - リングバッファの値は、エポックに揃えた幅 `liveBinMs` の格子で最小・最大にまとめる。最初の区間だけ
 *   `tMs = liveFrom` で始める（格子の途中から）。`null` のサンプルは値に数えず、値が 1 つも無い区間は
 *   欠測（null）。サンプルが 1 つも無い格子は出さない（`envelopePath` の「区間の列の穴」で線が切れる）。
 * - 幅 `liveBinMs` は、履歴の系列の `binMs`（無ければ利用側が渡す `binMs`）を、サンプル間隔の 1.5 倍
 *   以上になるまで整数倍に広げたもの。サンプル間隔ちょうどの幅だと、間隔のゆらぎで値の無い格子が
 *   できて線が細切れになるため（間隔が幅未満なら、幅 ≥ 1.5 × 間隔のどの区間にも値が入る）。
 *   サンプル間隔は利用側が渡す（収集周期・ポーリングの間隔）。渡さなければバッファの直近の間隔の
 *   中央値で見積もる。
 * - 出力の系列の `binMs` は、リングバッファがあるペンでは `liveBinMs`（履歴の幅以上）。履歴の区間は
 *   次の区間の始まりで打ち切られる（`binEndMs`）ので、狭い間隔の履歴の区間もそのまま並べてよい。
 *   つなぎ目（読み直した区間 → `liveFrom` の区間）の間隔は幅より狭いので、両側に値があれば線はつながる。
 * - 出力の `range` は 1 本: 始まりは履歴の取得範囲の始まりと各ペンの `liveFrom` の早い方、終わりは
 *   履歴の取得範囲の終わり・`nowMs`・最新のサンプルの直後の遅い方（`nowMs` はリングバッファを持つ
 *   ペンがあるときだけ使う。履歴だけなら取得範囲のまま）。履歴が無い（読み出し中・失敗）ときは
 *   `liveFrom` からだけ描く。履歴の取得範囲と `liveFrom` の間に隙間があれば、そこには区間が無く
 *   （`envelopePath` の「区間の列の穴」で）線が切れる。
 */

import { binEndMs } from './geometry';
import type { TrendData, TrendSeries } from './data';
import type { LiveBuffers, LivePen, LiveSample } from './live';
import { isAbortError, type TrendFetch } from './loader';
import type { Bin } from './pens';
import type { TimeRange } from './range';

const fin = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

/** つなぎ合わせの材料。 */
export interface HybridSources {
	/** 履歴（読み出し中・失敗なら null）。 */
	history: TrendData | null;
	/** ペンごとのリングバッファ。 */
	live: LiveBuffers;
	/** 出力するペン。 */
	penIds: readonly string[];
}

/** 境界の区間の読み直しの結果。キーは `boundaryKey`。 */
export type BoundaryValues = Readonly<Record<string, Bin | undefined>>;

export interface HybridInput extends HybridSources {
	/** 履歴の系列が無いペンの区間の幅（表示範囲 ÷ 区間の数。`binMs()`）。 */
	binMs: number;
	/** 境界の区間の読み直しの結果。 */
	boundary?: BoundaryValues;
	/** ペンごとのサンプル間隔（収集周期など）。無ければバッファから見積もる。 */
	sampleIntervalMs?: Readonly<Record<string, number | undefined>>;
	/** 今（出力の範囲の右端の候補）。 */
	nowMs?: number;
}

/** 境界の区間 `[startMs, liveFrom)` を 1 区間で読む要求。同じ範囲のペンをまとめる。 */
export interface BoundaryRequest {
	range: TimeRange;
	penIds: string[];
}

/** 読み直しの結果を覚えるキー（ペン・区間の始まり・liveFrom）。 */
export function boundaryKey(penId: string, startMs: number, liveFrom: number): string {
	return `${startMs}|${liveFrom}|${penId}`;
}

/** 直近の間隔の中央値（最大 32 個）。2 つ未満なら undefined。 */
export function estimateIntervalMs(samples: readonly LiveSample[]): number | undefined {
	const n = samples.length;
	if (n < 2) return undefined;
	const diffs: number[] = [];
	for (let i = Math.max(1, n - 32); i < n; i++) diffs.push(samples[i].tMs - samples[i - 1].tMs);
	diffs.sort((a, b) => a - b);
	const m = diffs[Math.floor(diffs.length / 2)];
	return m > 0 ? m : undefined;
}

/**
 * リングバッファをまとめる幅。`baseBinMs` をサンプル間隔の 1.5 倍以上になるまで整数倍に広げる。
 * 間隔が無効なら `baseBinMs` のまま。`baseBinMs` が無効なら間隔の 1.5 倍（それも無ければ 1000）。
 */
export function liveBinMs(baseBinMs: number, sampleIntervalMs?: number): number {
	const iv = fin(sampleIntervalMs) && sampleIntervalMs > 0 ? sampleIntervalMs : null;
	if (!(fin(baseBinMs) && baseBinMs > 0)) return iv !== null ? iv * 1.5 : 1000;
	if (iv === null) return baseBinMs;
	return baseBinMs * Math.max(1, Math.ceil((iv * 1.5) / baseBinMs - 1e-9));
}

/**
 * サンプルを幅 `binMs` の格子（エポックに揃える）で最小・最大にまとめる。`liveFrom` より前の
 * サンプルは捨て、最初の区間は `liveFrom` で始める。値の無い区間は欠測、サンプルの無い格子は出さない。
 */
export function liveBins(samples: readonly LiveSample[], liveFrom: number, binMs: number): Bin[] {
	const out: Bin[] = [];
	if (!(fin(binMs) && binMs > 0)) return out;
	let cur: Bin | null = null;
	for (const s of samples) {
		if (!fin(s.tMs) || s.tMs < liveFrom) continue;
		const start = Math.max(liveFrom, Math.floor(s.tMs / binMs) * binMs);
		if (!cur || cur.tMs !== start) {
			cur = { tMs: start, min: null, max: null };
			out.push(cur);
		}
		const v = s.value;
		if (fin(v)) {
			cur.min = cur.min === null ? v : Math.min(cur.min, v);
			cur.max = cur.max === null ? v : Math.max(cur.max, v);
		}
	}
	return out;
}

/** 履歴の系列を `liveFrom` で分ける。`before` は終わりが `liveFrom` 以下の区間、`straddle` はまたぐ区間の始まり。 */
function splitHistory(
	series: TrendSeries,
	rangeEndMs: number,
	liveFrom: number
): { before: Bin[]; straddle: number | null } {
	const before: Bin[] = [];
	const { bins, binMs } = series;
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		if (!fin(b.tMs)) continue;
		if (b.tMs >= liveFrom) break;
		const end = Math.min(binEndMs(b.tMs, binMs, bins[i + 1]?.tMs), rangeEndMs);
		if (end <= liveFrom) {
			before.push(b);
			continue;
		}
		return { before, straddle: b.tMs };
	}
	return { before, straddle: null };
}

function activeLive(live: LiveBuffers, penId: string): (LivePen & { liveFrom: number }) | null {
	const p = live[penId];
	return p && fin(p.liveFrom) && p.samples.length > 0
		? (p as LivePen & { liveFrom: number })
		: null;
}

/** 境界の区間の一覧（ペンごと 0〜1 個）。 */
function boundaries(src: HybridSources): { penId: string; startMs: number; liveFrom: number }[] {
	const out: { penId: string; startMs: number; liveFrom: number }[] = [];
	const h = src.history;
	if (!h) return out;
	for (const penId of src.penIds) {
		const lp = activeLive(src.live, penId);
		const s = h.series[penId];
		if (!lp || !s) continue;
		const { straddle } = splitHistory(s, h.range.endMs, lp.liveFrom);
		if (straddle !== null) out.push({ penId, startMs: straddle, liveFrom: lp.liveFrom });
	}
	return out;
}

/**
 * 読み直すべき境界の区間。同じ `[startMs, liveFrom)` のペンを 1 つの要求にまとめる。
 * `known` にキーがある（`undefined` でない）ものは除く。
 */
export function boundaryPlan(src: HybridSources, known?: BoundaryValues): BoundaryRequest[] {
	const groups = new Map<string, BoundaryRequest>();
	for (const b of boundaries(src)) {
		if (known?.[boundaryKey(b.penId, b.startMs, b.liveFrom)] !== undefined) continue;
		const g = `${b.startMs}|${b.liveFrom}`;
		let req = groups.get(g);
		if (!req) {
			req = { range: { startMs: b.startMs, endMs: b.liveFrom }, penIds: [] };
			groups.set(g, req);
		}
		req.penIds.push(b.penId);
	}
	return [...groups.values()];
}

/** いま要る境界のキーだけを残す（`liveFrom` が進んだ・履歴を読み直した後の古いキーを捨てる）。 */
export function pruneBoundary(values: BoundaryValues, src: HybridSources): BoundaryValues {
	const need = new Set(boundaries(src).map((b) => boundaryKey(b.penId, b.startMs, b.liveFrom)));
	const keys = Object.keys(values);
	if (keys.every((k) => need.has(k))) return values;
	const out: Record<string, Bin | undefined> = {};
	for (const k of keys) if (need.has(k)) out[k] = values[k];
	return out;
}

/**
 * 読み直しの結果を覚える形にする。要求の範囲に始まる区間（`tMs` が `[startMs, endMs)`）の最小・最大を
 * 1 つにまとめ、`tMs = startMs` の区間にする。値が無い・系列が無いペンは欠測（null）。
 */
export function boundaryValuesFrom(
	req: BoundaryRequest,
	result: Readonly<Record<string, TrendSeries | undefined>>
): Record<string, Bin> {
	const { startMs, endMs } = req.range;
	const out: Record<string, Bin> = {};
	for (const penId of req.penIds) {
		let min: number | null = null;
		let max: number | null = null;
		for (const b of result[penId]?.bins ?? []) {
			if (!fin(b.tMs) || b.tMs < startMs || b.tMs >= endMs) continue;
			for (const v of [b.min, b.max]) {
				if (!fin(v)) continue;
				min = min === null ? v : Math.min(min, v);
				max = max === null ? v : Math.max(max, v);
			}
		}
		out[boundaryKey(penId, startMs, endMs)] = { tMs: startMs, min, max };
	}
	return out;
}

/** 履歴とリングバッファを 1 本の `TrendData` にする。描けるものが無ければ null。 */
export function hybridData(input: HybridInput): TrendData | null {
	const { history, live, penIds, boundary } = input;
	const series: Record<string, TrendSeries> = {};
	let startMs = history ? history.range.startMs : Infinity;
	let endMs = history ? history.range.endMs : -Infinity;
	let anyLive = false;

	for (const penId of penIds) {
		const hs = history?.series[penId];
		const lp = activeLive(live, penId);
		if (!lp) {
			if (hs) series[penId] = hs;
			continue;
		}
		anyLive = true;
		startMs = Math.min(startMs, lp.liveFrom);
		endMs = Math.max(endMs, lp.samples[lp.samples.length - 1].tMs + 1);
		const base = hs && fin(hs.binMs) && hs.binMs > 0 ? hs.binMs : input.binMs;
		const interval = input.sampleIntervalMs?.[penId] ?? estimateIntervalMs(lp.samples);
		const w = liveBinMs(base, interval);
		const bins: Bin[] = [];
		if (hs && history) {
			const { before, straddle } = splitHistory(hs, history.range.endMs, lp.liveFrom);
			bins.push(...before);
			if (straddle !== null) {
				const got = boundary?.[boundaryKey(penId, straddle, lp.liveFrom)];
				bins.push(got ? { ...got, tMs: straddle } : { tMs: straddle, min: null, max: null });
			}
		}
		bins.push(...liveBins(lp.samples, lp.liveFrom, w));
		series[penId] = { bins, binMs: w };
	}

	if (!history && !anyLive) return null;
	if (anyLive && fin(input.nowMs)) endMs = Math.max(endMs, input.nowMs);
	if (!(startMs < endMs)) return null;
	return { range: { startMs, endMs }, series };
}

export interface BoundaryLoaderOptions {
	fetch: TrendFetch;
	/** 新しい要求が来てから読み始めるまで（既定 200 ms）。 */
	debounceMs?: number;
	/** 1 要求ぶんの結果（`boundaryValuesFrom` の形）。利用側は `boundary` に足す。 */
	onResult: (values: Record<string, Bin>, req: BoundaryRequest) => void;
	onError?: (error: unknown, req: BoundaryRequest) => void;
}

export interface BoundaryLoader {
	/** いまの計画（`boundaryPlan` の結果）を渡す。毎回渡してよい。 */
	request(plan: readonly BoundaryRequest[]): void;
	dispose(): void;
}

/**
 * 境界の区間の読み直しの段取り。`createRangeLoader` と同じ作法（debounce・abort・古い結果を捨てる・
 * AbortError を渡さない）で、要求ごとに独立に読む:
 * - 計画は毎回渡してよい。読み出し中・待っている要求と同じものは読み直さない。
 * - 計画から消えた要求は abort し、遅れて返っても結果を渡さない。
 * - 新しい要求が加わったら、`debounceMs` 待ってからまとめて読み始める（区間の数は 1）。
 * - 成功した要求は忘れる（利用側が結果を `known` に足すので、計画から消える）。失敗した要求は、
 *   計画から消えるまで読み直さない（毎秒の計画の更新で失敗を繰り返して叩かない。`liveFrom` が
 *   進む・履歴を読み直すと要求が変わり、そのときに読む。それまでその区間は欠測で線が切れる）。
 */
export function createBoundaryLoader(options: BoundaryLoaderOptions): BoundaryLoader {
	const { fetch, onResult, onError } = options;
	const debounceMs = fin(options.debounceMs) && options.debounceMs >= 0 ? options.debounceMs : 200;
	interface Entry {
		req: BoundaryRequest;
		controller: AbortController;
		started: boolean;
	}
	const active = new Map<string, Entry>();
	let timer: ReturnType<typeof setTimeout> | null = null;
	let disposed = false;
	const sigOf = (r: BoundaryRequest) =>
		JSON.stringify([r.range.startMs, r.range.endMs, [...r.penIds].sort()]);

	const start = (sig: string, e: Entry) => {
		e.started = true;
		const current = () => !disposed && active.get(sig) === e && !e.controller.signal.aborted;
		let p: Promise<Record<string, TrendSeries>>;
		try {
			p = fetch({ range: e.req.range, penIds: e.req.penIds, bins: 1, signal: e.controller.signal });
		} catch (err) {
			p = Promise.reject(err);
		}
		p.then(
			(result) => {
				if (!current()) return;
				active.delete(sig);
				onResult(boundaryValuesFrom(e.req, result), e.req);
			},
			(err) => {
				if (current() && !isAbortError(err)) onError?.(err, e.req);
			}
		);
	};

	return {
		request(plan) {
			if (disposed) return;
			const next = new Map(plan.map((r) => [sigOf(r), r] as const));
			for (const [sig, e] of active) {
				if (next.has(sig)) continue;
				e.controller.abort();
				active.delete(sig);
			}
			let added = false;
			for (const [sig, req] of next) {
				if (active.has(sig)) continue;
				active.set(sig, { req, controller: new AbortController(), started: false });
				added = true;
			}
			if (!added) return;
			if (timer !== null) clearTimeout(timer);
			timer = setTimeout(() => {
				timer = null;
				for (const [sig, e] of active) if (!e.started) start(sig, e);
			}, debounceMs);
		},
		dispose() {
			disposed = true;
			if (timer !== null) clearTimeout(timer);
			timer = null;
			for (const e of active.values()) e.controller.abort();
			active.clear();
		}
	};
}
