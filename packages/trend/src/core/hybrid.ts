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
 *   欠測（null）。サンプルが 1 つも無い格子は出さない（下の規則で欠測の区間が入って線が切れる）。
 * - 幅 `liveBinMs` は、履歴の系列の `binMs`（無ければ利用側が渡す `binMs`）を、サンプル間隔の 1.5 倍
 *   以上になるまで整数倍に広げたもの。サンプル間隔ちょうどの幅だと、間隔のゆらぎで値の無い格子が
 *   できて線が細切れになるため（間隔が幅未満なら、幅 ≥ 1.5 × 間隔のどの区間にも値が入る）。
 *   サンプル間隔は利用側が渡す（収集周期・ポーリングの間隔）。渡さなければバッファの直近の間隔の
 *   中央値で見積もる。
 * - 出力の系列の `binMs` は、リングバッファがあるペンでは `liveBinMs`（履歴の幅以上）、無いペンでは
 *   履歴の `binMs`。描画とカーソルは区間の終わりを `binEndMs`（`min(tMs + binMs, 次の tMs)`）で決める。
 * - 出力の `range` は 1 本: 始まりは履歴の取得範囲の始まりと各ペンの `liveFrom` の早い方、終わりは
 *   履歴の取得範囲の終わり・`nowMs`・最新のサンプルの直後の遅い方（`nowMs` はリングバッファを持つ
 *   ペンがあるときだけ使う。履歴だけなら取得範囲のまま）。履歴が無い（読み出し中・失敗）ときは
 *   `liveFrom` からだけ描く。履歴の取得範囲と `liveFrom` の間に隙間があれば、下の規則で欠測の
 *   区間が入って線が切れる。
 *
 * 規則（本当の終わり。全部のペンの出力に通す）: 出力の区間ごとに、値を本当に持つ範囲の終わりを持つ。
 * 本当の終わりが次の区間の始まり（最後の区間なら出力の `range.endMs`）より前なら、本当の終わりに欠測の
 * 区間 `{ tMs: 本当の終わり, min: null, max: null }` を入れる（`withTrueEnds`）。こうすると `binEndMs` は
 * どの区間でも本当の終わりを超えず、幅が広い・次の区間が遠い・範囲が他のペンで延びたときも、値の無い
 * 時刻に線やカーソルの値を出さない。本当の終わりは:
 * - 履歴の区間: `min(binEndMs(tMs, 履歴の binMs, 元の次の tMs), 履歴の取得範囲の終わり)`。途中の穴・
 *   取得範囲の終わり・ライブとの隙間・ライブの無いペンで範囲が延びた場合が、どれもこれで切れる。
 *   取得範囲の終わり以後に始まる区間は使わない。
 * - 境界の区間（読み直し、またはまだで欠測）: `liveFrom`。
 * - ライブの区間: その格子の終わり（`floor(tMs / w) * w + w`）。先頭の途中区間（`tMs = liveFrom`）も
 *   その格子の終わりで、次の値のある格子までは欠測。
 * - ライブの最後の区間も格子の終わり。新しい格子にまだサンプルが届いていない間は、その格子（`nowMs`
 *   まで）が欠測になる。線の点は区間の中央なので描画は変わらず、カーソルの値だけが「—」になる
 *   （値の無い時刻なので正しい。次のサンプルで埋まる）。
 * 欠測の区間（元から値の無い区間）の後には入れない（もう切れている）。つなぎ目は、読み直した区間の
 * 本当の終わり = `liveFrom` = 最初のライブの区間の始まりなので欠測は入らず、両側に値があれば線は
 * つながる。
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

/** 区間と、その区間が本当に値を持つ範囲の終わり（排他）。 */
export interface BinSpan {
	bin: Bin;
	endMs: number;
}

const hasValue = (b: Bin) => fin(b.min) || fin(b.max);

/**
 * 区間の列（`tMs` の昇順）に、本当の終わりで欠測の区間を挟む。本当の終わりが次の区間の始まり
 * （最後の区間なら `rangeEndMs`）より前で、区間の始まりより後のときだけ入れる。値の無い区間の後には
 * 入れない。
 */
export function withTrueEnds(spans: readonly BinSpan[], rangeEndMs: number): Bin[] {
	const out: Bin[] = [];
	for (let i = 0; i < spans.length; i++) {
		const { bin, endMs } = spans[i];
		out.push(bin);
		const next = i + 1 < spans.length ? spans[i + 1].bin.tMs : rangeEndMs;
		if (hasValue(bin) && fin(endMs) && endMs > bin.tMs && endMs < next)
			out.push({ tMs: endMs, min: null, max: null });
	}
	return out;
}

/** 履歴の区間とその本当の終わり（`min(binEndMs, 取得範囲の終わり)`）。取得範囲の終わり以後は使わない。 */
function historySpans(series: TrendSeries, rangeEndMs: number): BinSpan[] {
	const out: BinSpan[] = [];
	const { bins, binMs } = series;
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		if (!fin(b.tMs)) continue;
		if (b.tMs >= rangeEndMs) break;
		out.push({ bin: b, endMs: Math.min(binEndMs(b.tMs, binMs, bins[i + 1]?.tMs), rangeEndMs) });
	}
	return out;
}

/**
 * 履歴の系列を `liveFrom` で分ける。`before` は本当の終わりが `liveFrom` 以下の区間（`historySpans`）、
 * `straddle` は `liveFrom` をまたぐ区間の始まり。
 */
function splitHistory(
	series: TrendSeries,
	rangeEndMs: number,
	liveFrom: number
): { before: BinSpan[]; straddle: number | null } {
	const before: BinSpan[] = [];
	for (const sp of historySpans(series, rangeEndMs)) {
		if (sp.bin.tMs >= liveFrom) break;
		if (sp.endMs > liveFrom) return { before, straddle: sp.bin.tMs };
		before.push(sp);
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
	const spans: Record<string, { spans: BinSpan[]; binMs: number }> = {};
	let startMs = history ? history.range.startMs : Infinity;
	let endMs = history ? history.range.endMs : -Infinity;
	let anyLive = false;

	for (const penId of penIds) {
		const hs = history?.series[penId];
		const lp = activeLive(live, penId);
		if (!lp) {
			if (hs && history)
				spans[penId] = { spans: historySpans(hs, history.range.endMs), binMs: hs.binMs };
			continue;
		}
		anyLive = true;
		startMs = Math.min(startMs, lp.liveFrom);
		endMs = Math.max(endMs, lp.samples[lp.samples.length - 1].tMs + 1);
		const base = hs && fin(hs.binMs) && hs.binMs > 0 ? hs.binMs : input.binMs;
		const interval = input.sampleIntervalMs?.[penId] ?? estimateIntervalMs(lp.samples);
		const w = liveBinMs(base, interval);
		const out: BinSpan[] = [];
		if (hs && history) {
			const { before, straddle } = splitHistory(hs, history.range.endMs, lp.liveFrom);
			out.push(...before);
			if (straddle !== null) {
				const got = boundary?.[boundaryKey(penId, straddle, lp.liveFrom)];
				const bin = got ? { ...got, tMs: straddle } : { tMs: straddle, min: null, max: null };
				out.push({ bin, endMs: lp.liveFrom });
			}
		}
		for (const b of liveBins(lp.samples, lp.liveFrom, w))
			out.push({ bin: b, endMs: Math.floor(b.tMs / w) * w + w });
		spans[penId] = { spans: out, binMs: w };
	}

	if (!history && !anyLive) return null;
	if (anyLive && fin(input.nowMs)) endMs = Math.max(endMs, input.nowMs);
	if (!(startMs < endMs)) return null;
	const series: Record<string, TrendSeries> = {};
	for (const [penId, s] of Object.entries(spans))
		series[penId] = { bins: withTrueEnds(s.spans, endMs), binMs: s.binMs };
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
