/**
 * リアルタイムトレンド（R1-D の D-3b、recorder-requirements.md §3.3 の「リアル
 * タイム」）の判断をまとめた純関数（`trendLogic.test.ts` が表で固定する）。
 * バッファの持ち主（`trendFeed.svelte.ts`）とパネル（`TrendPanel.svelte`）は
 * ここを呼ぶだけで、判断を書き写さない（docs/implementation-checklist.md §5）。
 *
 * ## 時間の格子（なぜ固定の刻みに載せるか）
 *
 * banto の `LineChart` は x を**順序付きのカテゴリ**として等間隔に並べる（実時間の
 * 目盛ではない）。行の時刻がばらばらだと、詰まった区間と空いた区間が同じ幅で描かれて
 * 時間が歪む。そこで行を**固定の刻み（`stepMs`）の格子**に載せ、窓の行数を一定
 * （`windowMs / stepMs`）に保つ。値の無い刻みは `null` の行として残し、
 * `gaps: 'break'` で線を切る（null を 0 と区別する、#414 段階2）。
 *
 * 刻みは [`chooseStepMs`]: 候補（60 秒を割り切る値。窓の選択肢はどれも 60 秒の倍数
 * なので行数が整数になる）のうち、次を満たす最小のもの。
 *
 * - **ポーリング周期の 2 倍以上**。1 刻みに 1 回しか読まないと、周期の揺れ（応答を
 *   待ってから次を予約するので、実際の間隔は周期より少し長い）で刻みを飛ばし、
 *   読めているのに線が切れる。2 倍なら 1 刻みに 1 回以上は必ず読む。
 * - **行数が描画幅（ピクセル）以下**。`LineChart` は幅を超える行を間引くので、
 *   行数を幅以下にしておけば 1 行 = 1 刻みが崩れない。
 * - **履歴の上限**（`HISTORY_MAX_BINS`、本数 × 行数 ≤ `HISTORY_MAX_POINTS`）。初期窓を
 *   1 回の要求で格子と同じ数の区間で読むため。
 *
 * 時刻は**サーバーの時計**（現在値の `ptimeMs`、履歴の `tMs`）で揃える。端末の時計を
 * 使うと、LAN の別の端末で時計がずれているとき履歴と現在値が食い違う。表示の時計は
 * [`observeServerClock`] / [`serverClockNow`]: 受け取った `ptimeMs` の最大を基準にし、
 * その後は**端末の単調な経過時間**（`performance.now()`）で進める。
 *
 * - `ptimeMs` だけを時計にすると、表示中のタグがすべて収集周期 60 秒のとき、ポーリングが
 *   成功し続けても `ptimeMs` は 60 秒間止まったままで、同じ刻みを書き直し続け、次の
 *   サンプルで間の刻みが `null` になって**毎分線が切れる**（PR #543 のオーナーレビュー P2）。
 *   経過時間で進めれば、同じ good のサンプルが続く間も今の刻みに書く（下の「収集周期の
 *   長いタグも、ポーリングのたびに今の値を書く」、履歴の `max(binMs, 周期)` と同じ扱い）。
 * - 基準はより新しい `ptimeMs` が来たときだけ前へ付け替える（戻さない）。
 * - **サーバーの時刻を 1 つも受け取っていない間は時計が無い**（`null`）。そのあいだ格子を
 *   作らず、履歴も要求しない。端末の時計で格子を作ると、端末の時計が進んでいるとき後から
 *   来たサーバーの時刻が「過去」に見え、`appendLive` が捨ててしまう（PR #543 のレビュー P2）。
 * - 読み取りの失敗（ポーリングが成功しない）・`bad`・`stale` は今までどおり線を切る:
 *   失敗の間は書き足さないので刻みは `null`、`bad` / `stale` の値は `null`。
 *
 * ## 履歴（初期窓）と現在値の合わせ方
 *
 * - **現在値**: ポーリングで受け取るたびに、その時刻の刻みの行へペンごとの値を書く
 *   （同じ刻みに 2 回来たら後の値。品質 good で有限の値だけが数値で、それ以外は
 *   `null`）。収集周期の長いタグも、ポーリングのたびに「今の値」を書くので線は
 *   つながる（記録計の「表示の刻みごとに今の値を打つ」と同じ）。
 * - **履歴**（D-3a の `getCollectHistory`）: 点は間引いた区間の最小・最大。**1 本の
 *   線で描き、値は最小と最大の中点**にする（[`historyPointValue`]）。
 *   - 判断: 包絡を 2 本の線（最小・最大）で描くと 8 ペンで 16 系列になり、banto の
 *     系列色（8 枠）を使い切る・凡例が倍になる。最大だけを描くと上に偏り、現在値
 *     （その時点の 1 サンプル）と並べたときに段差が出る。中点は偏りが無く、区間の
 *     幅は格子の 1 刻み（≒ 1 ピクセル）なので、1 刻みの中の山だけが平らになる。
 *     初期窓の多くの刻みは 1〜数サンプルしか含まず、そのとき最小 = 最大で値は正確。
 *     山を落とさない包絡の表示は R2 のヒストリカルで扱う。
 *   - 点は区間の始まり `tMs` から `holdMs`（区間の幅 `binMs` とそのタグの収集周期の
 *     大きい方）の間の刻みに書く。収集周期が刻みより長いタグを素通しで読んだとき、
 *     点と点の間の刻みが `null` になって線が細切れになるのを防ぐ（1 サンプルは次の
 *     サンプルまで有効、という記録計の扱い）。
 *   - **現在値を書き始めた刻み（`liveFromT`）より前にだけ書く**。現在値の行は
 *     上書きしない。履歴は書き手の flush（約 1 秒）の後に要求する
 *     （`trendFeed.svelte.ts` の `HISTORY_FLUSH_GRACE_MS`）ので、境目に刻みの抜けは
 *     できない。
 *
 * ## しきい値の帯（Q5）
 *
 * 帯は**選んだ 1 ペンだけ**（記録計の側の設定、#532）。上側は H..HH が注意、HH..+∞ が
 * 危険、下側は LL..L が注意、-∞..LL が危険（[`thresholdBands`]）。設定の無いペンは
 * 帯を出さない。既定で選ぶのは、しきい値のある最初のペン。
 */
import type { OpenThresholdBand } from '@banto/charts';
import {
	HISTORY_MAX_BINS,
	HISTORY_MAX_POINTS,
	type CollectHistory,
	type CollectHistoryParams,
	type CurrentSampleView,
	type HistoryPoint
} from '../banto/collectAdmin';
import type { DisplayGroup } from '../banto/displayGroupsAdmin';
import type { TagWithThresholds, ThresholdFields } from '../banto/tagThresholdsAdmin';
import { formatValue } from './monitorLogic';

// --- 時間窓 ------------------------------------------------------------------

/**
 * 選べる時間窓（秒）。Rust の `TREND_TIME_WINDOWS_SEC`、グループ設定画面の
 * `TIME_WINDOW_OPTIONS` と同じ（`trendLogic.test.ts` が突き合わせる）。
 */
export const TREND_WINDOWS_SEC: readonly number[] = [60, 300, 600, 1800, 3600];
/** グループに既定の窓が無い・不正なときの窓（R1-D「既定窓 10 分」）。 */
export const DEFAULT_TREND_WINDOW_SEC = 600;

export function isTrendWindowSec(value: unknown): value is number {
	return typeof value === 'number' && TREND_WINDOWS_SEC.includes(value);
}

/** 時間窓の表示名（「10 分」「1 時間」）。 */
export function trendWindowLabel(sec: number): string {
	if (sec % 3600 === 0) return `${sec / 3600} 時間`;
	if (sec % 60 === 0) return `${sec / 60} 分`;
	return `${sec} 秒`;
}

/** 端末ごとの時間窓（Q4）の localStorage のキー。値は `{ "<グループ ID>": 秒 }`。 */
export const TREND_WINDOW_STORAGE_KEY = 'chronogazer.monitor.trendWindowSec';

function readWindowMap(storage: Pick<Storage, 'getItem'>): Record<string, unknown> {
	const raw = storage.getItem(TREND_WINDOW_STORAGE_KEY);
	if (raw === null) return {};
	const parsed: unknown = JSON.parse(raw);
	return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
		? (parsed as Record<string, unknown>)
		: {};
}

/** この端末で選んだ窓（秒）。無い・壊れている・選択肢に無いときは `null`。 */
export function loadTrendWindowOverride(
	storage: Pick<Storage, 'getItem'> | undefined,
	groupId: number
): number | null {
	if (!storage) return null;
	try {
		const value = readWindowMap(storage)[String(groupId)];
		return isTrendWindowSec(value) ? value : null;
	} catch {
		return null;
	}
}

/**
 * この端末の窓を覚える（ベストエフォート）。グループの既定と同じ窓を選んだら覚えを
 * 消す（以後はグループの既定に従う）。**グループの定義には書かない**（Q4）。
 */
export function saveTrendWindowOverride(
	storage: Pick<Storage, 'getItem' | 'setItem'> | undefined,
	groupId: number,
	sec: number,
	groupDefaultSec: number
): void {
	if (!storage || !isTrendWindowSec(sec)) return;
	try {
		let map: Record<string, unknown>;
		try {
			map = readWindowMap(storage);
		} catch {
			map = {};
		}
		const next = { ...map };
		if (sec === groupDefaultSec) delete next[String(groupId)];
		else next[String(groupId)] = sec;
		storage.setItem(TREND_WINDOW_STORAGE_KEY, JSON.stringify(next));
	} catch {
		// 端末の便利機能なので、失敗しても表示は続ける。
	}
}

/** グループの既定の窓（属性が無い・不正なら [`DEFAULT_TREND_WINDOW_SEC`]）。 */
export function groupDefaultWindowSec(attributes: { timeWindowSec?: number }): number {
	return isTrendWindowSec(attributes.timeWindowSec)
		? attributes.timeWindowSec
		: DEFAULT_TREND_WINDOW_SEC;
}

/** 使う窓: 端末の覚え → グループの既定。 */
export function resolveTrendWindowSec(
	attributes: { timeWindowSec?: number },
	override: number | null
): number {
	return override ?? groupDefaultWindowSec(attributes);
}

// --- 格子の刻み --------------------------------------------------------------

/** 刻みの候補（ms）。どれも 60 秒を割り切る。 */
export const TREND_STEP_CANDIDATES_MS: readonly number[] = [
	250, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000
];

/** `LineChart` の既定の左右の余白（48 + 16 px）。行数の上限を描画域の幅で数える。 */
export const TREND_PLOT_MARGIN_PX = 64;

export interface StepInput {
	windowMs: number;
	/** パネルの幅（px）。余白は [`TREND_PLOT_MARGIN_PX`] を引いて数える。 */
	widthPx: number;
	/** 現在値のポーリング周期（ms）。 */
	pollPeriodMs: number;
	/** 履歴で読むタグの本数（重複を除く）。 */
	tagCount: number;
}

/** 格子の行数の上限（描画域の幅・履歴の上限）。 */
export function maxTrendRows(widthPx: number, tagCount: number): number {
	const plot = Math.floor(widthPx - TREND_PLOT_MARGIN_PX);
	const budget = Math.floor(HISTORY_MAX_POINTS / Math.max(1, tagCount));
	return Math.max(1, Math.min(plot, HISTORY_MAX_BINS, budget));
}

/**
 * 刻みを決める（純関数）。ポーリング周期の 2 倍以上で、行数が上限以下になる最小の
 * 候補。どれも満たさなければ最大の候補（60 秒。1 時間窓で 60 行）。
 */
export function chooseStepMs(input: StepInput): number {
	const maxRows = maxTrendRows(input.widthPx, input.tagCount);
	const minStep = input.pollPeriodMs * 2;
	for (const step of TREND_STEP_CANDIDATES_MS) {
		if (step < minStep) continue;
		if (input.windowMs / step <= maxRows) return step;
	}
	return TREND_STEP_CANDIDATES_MS[TREND_STEP_CANDIDATES_MS.length - 1];
}

/** 時刻をその刻みの始まりに丸める。 */
export function alignToStep(tMs: number, stepMs: number): number {
	return Math.floor(tMs / stepMs) * stepMs;
}

// --- バッファ ----------------------------------------------------------------

/** 格子の 1 行（刻みの始まりの時刻と、ペンの並びどおりの値）。 */
export interface TrendRow {
	t: number;
	/** ペンの並びどおり。`null` = 値が無い（線を切る。0 ではない）。 */
	values: (number | null)[];
}

/**
 * トレンドの格子。`rows` は時刻の昇順で、隙間の無い格子（`rows[k+1].t - rows[k].t
 * === stepMs`）、行数は常に `windowMs / stepMs`。
 */
export interface TrendBuffer {
	stepMs: number;
	windowMs: number;
	penCount: number;
	rows: TrendRow[];
	/** 現在値を書き始めた刻み（`null` = まだ書いていない）。履歴はこれより前にだけ書く。 */
	liveFromT: number | null;
}

function nullRow(t: number, penCount: number): TrendRow {
	return { t, values: Array.from({ length: penCount }, () => null) };
}

/** 窓の行数。 */
export function trendRowCount(windowMs: number, stepMs: number): number {
	return Math.max(1, Math.round(windowMs / stepMs));
}

/** 空の格子（全部 `null`）。最後の行が `nowMs` の刻み。 */
export function emptyTrendBuffer(
	stepMs: number,
	windowMs: number,
	penCount: number,
	nowMs: number
): TrendBuffer {
	const n = trendRowCount(windowMs, stepMs);
	const last = alignToStep(nowMs, stepMs);
	const rows: TrendRow[] = [];
	for (let k = 0; k < n; k++) rows.push(nullRow(last - (n - 1 - k) * stepMs, penCount));
	return { stepMs, windowMs, penCount, rows, liveFromT: null };
}

/**
 * 格子を `nowMs` の刻みまで進める（純関数）。足した刻みは `null`（その間に値を受け
 * 取っていない = 線を切る）で、窓より古い行は捨てる。時計が戻った（`nowMs` が最後の
 * 行より前）ときは何もしない。窓より長く空いたら全部 `null` の格子に作り直す。
 */
export function advanceTrend(buffer: TrendBuffer, nowMs: number): TrendBuffer {
	const { stepMs, windowMs, penCount, rows } = buffer;
	const target = alignToStep(nowMs, stepMs);
	const last = rows[rows.length - 1]?.t ?? Number.NEGATIVE_INFINITY;
	if (target <= last) return buffer;
	if (target - last >= windowMs) {
		return { ...emptyTrendBuffer(stepMs, windowMs, penCount, nowMs), liveFromT: buffer.liveFromT };
	}
	const added: TrendRow[] = [];
	for (let t = last + stepMs; t <= target; t += stepMs) added.push(nullRow(t, penCount));
	const n = trendRowCount(windowMs, stepMs);
	const combined = [...rows, ...added];
	return { ...buffer, rows: combined.slice(combined.length - n) };
}

/**
 * 現在値を 1 回分書く（純関数）。`nowMs` の刻みまで進めてから、その行のペンの値を
 * 置き換える（同じ刻みに 2 回来たら後の値）。`values` はペンの並びどおり。
 */
export function appendLive(
	buffer: TrendBuffer,
	nowMs: number,
	values: readonly (number | null)[]
): TrendBuffer {
	const advanced = advanceTrend(buffer, nowMs);
	const rows = advanced.rows;
	const lastIndex = rows.length - 1;
	const lastRow = rows[lastIndex];
	if (!lastRow || alignToStep(nowMs, advanced.stepMs) !== lastRow.t) return advanced;
	const nextValues = lastRow.values.map((prev, i) => {
		const v = values[i];
		return v === undefined ? prev : v !== null && Number.isFinite(v) ? v : null;
	});
	const nextRows = rows.slice();
	nextRows[lastIndex] = { t: lastRow.t, values: nextValues };
	return {
		...advanced,
		rows: nextRows,
		liveFromT: advanced.liveFromT ?? lastRow.t
	};
}

/**
 * 履歴の点 1 つの値（純関数）: 最小と最大の中点（判断はモジュールの doc）。片方だけ
 * 有限ならその値、どちらも無ければ `null`。
 */
export function historyPointValue(point: Pick<HistoryPoint, 'min' | 'max'>): number | null {
	const lo = point.min !== null && Number.isFinite(point.min) ? point.min : null;
	const hi = point.max !== null && Number.isFinite(point.max) ? point.max : null;
	if (lo === null) return hi;
	if (hi === null) return lo;
	return (lo + hi) / 2;
}

/**
 * 履歴を格子に合わせる（純関数）。ペンごとにタグの系列を引き、点の
 * `[tMs, tMs + holdMs)` に重なる刻みに値を書く（`holdMs` は `binMs` と
 * `periodMsOf(tagId)` の大きい方）。点は時刻順に書くので、同じ刻みに 2 点が重なれば
 * 後の点（現在値の「後の値」と同じ）。`liveFromT` 以後の行は触らない。系列の無い
 * ペン（不明なタグ）は `null` のまま。
 */
export function mergeHistory(
	buffer: TrendBuffer,
	penTagIds: readonly number[],
	history: Pick<CollectHistory, 'series'>,
	periodMsOf: (tagId: number) => number | null = () => null
): TrendBuffer {
	const { rows, stepMs } = buffer;
	if (rows.length === 0) return buffer;
	const first = rows[0].t;
	const limit = buffer.liveFromT ?? rows[rows.length - 1].t + stepMs;
	const next = rows.map((row) => ({ t: row.t, values: row.values.slice() }));
	penTagIds.forEach((tagId, pen) => {
		const series = history.series.find((s) => s.tagId === tagId);
		if (!series) return;
		const period = periodMsOf(tagId) ?? 0;
		const holdMs = Math.max(series.binMs, period, 1);
		const points = [...series.points].sort((a, b) => a.tMs - b.tMs);
		for (const point of points) {
			const value = historyPointValue(point);
			const end = point.tMs + holdMs; // 排他
			// 重なる刻み: row.t < end かつ row.t + stepMs > point.tMs。
			let k = Math.max(0, Math.floor((point.tMs - first) / stepMs));
			for (; k < next.length; k++) {
				const t = next[k].t;
				if (t >= end || t >= limit) break;
				if (t + stepMs <= point.tMs) continue;
				next[k].values[pen] = value;
			}
		}
	});
	return { ...buffer, rows: next };
}

/** 初期窓の要求（純関数）。期間は格子の全体（両端を含む）、区間の数は行数。 */
export function historyRequest(
	buffer: TrendBuffer,
	penTagIds: readonly number[]
): CollectHistoryParams | null {
	const tagIds = [...new Set(penTagIds)];
	if (tagIds.length === 0 || buffer.rows.length === 0) return null;
	const fromMs = buffer.rows[0].t;
	const toMs = buffer.rows[buffer.rows.length - 1].t + buffer.stepMs - 1;
	return { tagIds, fromMs, toMs, bins: buffer.rows.length };
}

/** 現在値の `ptimeMs`（収集が読みに行った時刻、サーバーの時計）の最大。無ければ `null`。 */
export function maxPtimeMs(
	values: Readonly<Record<string, Pick<CurrentSampleView, 'ptimeMs'>>>
): number | null {
	let max: number | null = null;
	for (const sample of Object.values(values)) {
		const t = sample.ptimeMs;
		if (t !== null && Number.isFinite(t) && (max === null || t > max)) max = t;
	}
	return max;
}

/**
 * 表示の時計（サーバーの時刻の基準と、それを受け取ったときの端末の単調な時刻）。
 * `null` = まだサーバーの時刻を受け取っていない。
 */
export type ServerClock = { serverMs: number; localMs: number } | null;

/** 時計の今（純関数）: 基準 + 端末の経過時間。時計が無ければ `null`。 */
export function serverClockNow(clock: ServerClock, localNowMs: number): number | null {
	return clock === null ? null : clock.serverMs + Math.max(0, localNowMs - clock.localMs);
}

/**
 * 現在値を受け取ったときに時計を更新する（純関数）。`ptimeMs` の最大が今の見積もりより
 * 新しければ基準をそこへ付け替え、そうでなければそのまま（後ろへは戻さない）。
 * `ptimeMs` が 1 つも無ければ時計はそのまま（無ければ `null` のまま）。
 */
export function observeServerClock(
	clock: ServerClock,
	values: Readonly<Record<string, Pick<CurrentSampleView, 'ptimeMs'>>>,
	localNowMs: number
): ServerClock {
	const latest = maxPtimeMs(values);
	if (latest === null) return clock;
	const now = serverClockNow(clock, localNowMs);
	if (now !== null && latest <= now) return clock;
	return { serverMs: latest, localMs: localNowMs };
}

/** 値の範囲（全ペン・全行の最小と最大）。値が 1 つも無ければ `null`。 */
export function valueExtent(rows: readonly TrendRow[]): { min: number; max: number } | null {
	let min = Infinity;
	let max = -Infinity;
	for (const row of rows) {
		for (const v of row.values) {
			if (v === null || !Number.isFinite(v)) continue;
			if (v < min) min = v;
			if (v > max) max = v;
		}
	}
	return Number.isFinite(min) ? { min, max } : null;
}

// --- しきい値の帯 ------------------------------------------------------------

export function hasThresholds(t: ThresholdFields | undefined | null): boolean {
	return (
		!!t &&
		(t.thresholdHh !== null ||
			t.thresholdH !== null ||
			t.thresholdL !== null ||
			t.thresholdLl !== null)
	);
}

export const TREND_WARNING_COLOR = 'var(--banto-warning)';
export const TREND_DANGER_COLOR = 'var(--banto-danger)';

/**
 * しきい値の帯（純関数）。判定（`thresholdLevel`）と同じ境目: 上側は HH 以上が危険・
 * H 以上 HH 未満が注意、下側は LL 以下が危険・LL より上 L 以下が注意。端の無い側は
 * `null`（banto の `OpenThresholdBand` が描画域の端まで伸ばす）。同じ値のしきい値
 * （`H === HH` など。正しい設定）は幅 0 の注意の帯を出さない。設定が無ければ空。
 */
export function thresholdBands(t: ThresholdFields | undefined | null): OpenThresholdBand[] {
	if (!t) return [];
	const { thresholdHh: hh, thresholdH: h, thresholdL: l, thresholdLl: ll } = t;
	const bands: OpenThresholdBand[] = [];
	if (hh !== null) bands.push({ from: hh, to: null, label: 'HH', colorVar: TREND_DANGER_COLOR });
	if (h !== null && (hh === null || h < hh)) {
		bands.push({ from: h, to: hh, label: 'H', colorVar: TREND_WARNING_COLOR });
	}
	if (l !== null && (ll === null || l > ll)) {
		bands.push({ from: ll, to: l, label: 'L', colorVar: TREND_WARNING_COLOR });
	}
	if (ll !== null) bands.push({ from: null, to: ll, label: 'LL', colorVar: TREND_DANGER_COLOR });
	return bands;
}

/** 帯を出すペン: 選んだペン（グループに居れば）→ しきい値のある最初のペン → なし。 */
export function resolveBandPen(
	pens: readonly { tagId: number; thresholds: ThresholdFields | null }[],
	selectedTagId: number | null
): number | null {
	if (selectedTagId !== null && pens.some((p) => p.tagId === selectedTagId)) return selectedTagId;
	return pens.find((p) => hasThresholds(p.thresholds))?.tagId ?? null;
}

// --- パネルの表示 -------------------------------------------------------------

/** パネルに渡すペン 1 本（判断は済ませてある）。 */
export interface TrendPenInfo {
	tagId: number;
	name: string;
	unit: string | null;
	/** 小数桁（タグを読めていなければ `null`）。 */
	decimals: number | null;
	colorSlot: number;
	thresholds: ThresholdFields | null;
}

/**
 * グループのペンをパネルの形にする（純関数）。タグを読めていなければ「タグ ID n」で
 * 単位・小数桁・しきい値なし。しきい値は設定のあるタグだけ（`tagsForDisplay` が
 * 読めなかったときに空にしたものをそのまま使う）。
 */
export function trendPenInfos(
	group: Pick<DisplayGroup, 'pens'>,
	tags: readonly TagWithThresholds[]
): TrendPenInfo[] {
	return group.pens.map((pen, index) => {
		const tag = tags.find((t) => t.id === pen.tagId);
		return {
			tagId: pen.tagId,
			name: tag?.name ?? `タグ ID ${pen.tagId}`,
			unit: tag?.unit ? tag.unit : null,
			decimals: tag ? tag.decimals : null,
			colorSlot: pen.colorSlot ?? index + 1,
			thresholds:
				tag && hasThresholds(tag)
					? {
							thresholdHh: tag.thresholdHh,
							thresholdH: tag.thresholdH,
							thresholdL: tag.thresholdL,
							thresholdLl: tag.thresholdLl
						}
					: null
		};
	});
}

/** 凡例の文字（名前 + 単位。Q7: 単位は凡例に）。 */
export function penLegendLabel(pen: Pick<TrendPenInfo, 'name' | 'unit'>): string {
	return pen.unit ? `${pen.name}（${pen.unit}）` : pen.name;
}

/** 時刻の目盛（端末のロケール、時:分:秒）。 */
export function trendTimeLabel(epochMs: number): string {
	const at = new Date(epochMs);
	if (Number.isNaN(at.getTime())) return String(epochMs);
	return at.toLocaleTimeString(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
}

function formatNumber(n: number, decimals: number | null): string {
	return decimals === null ? String(n) : formatValue(n, decimals);
}

/** 帯 1 つの説明（「H 80〜90 注意」など）。 */
function bandText(band: OpenThresholdBand, decimals: number | null): string {
	const tone = band.colorVar === TREND_DANGER_COLOR ? '危険' : '注意';
	const from = band.from === null ? null : formatNumber(band.from, decimals);
	const to = band.to === null ? null : formatNumber(band.to, decimals);
	const range = from === null ? `${to} 以下` : to === null ? `${from} 以上` : `${from}〜${to}`;
	return `${band.label} ${range} ${tone}`;
}

/**
 * 画面外の説明文（純関数、D-2 の `meterDescription` と同じ役目）。図は `role="img"`
 * で中が読まれないので、ペン・単位・窓・値の範囲・帯を文で伝える。
 */
export function trendDescription(input: {
	windowSec: number;
	pens: readonly TrendPenInfo[];
	rows: readonly TrendRow[];
	bandTagId: number | null;
}): string {
	const parts: string[] = [];
	parts.push(
		`直近 ${trendWindowLabel(input.windowSec)}のトレンド。ペン: ${input.pens.map(penLegendLabel).join('、')}。`
	);
	const extent = valueExtent(input.rows);
	const decimals = Math.max(0, ...input.pens.map((p) => p.decimals ?? 0));
	parts.push(
		extent === null
			? '表示できる値はまだありません。'
			: `縦軸は全ペン共通の自動スケールで、表示中の値の範囲は ${formatNumber(extent.min, decimals)}〜${formatNumber(extent.max, decimals)}。`
	);
	const bandPen = input.pens.find((p) => p.tagId === input.bandTagId) ?? null;
	if (bandPen === null) {
		parts.push('しきい値の帯: なし。');
	} else {
		const bands = thresholdBands(bandPen.thresholds);
		parts.push(
			bands.length === 0
				? `しきい値の帯: ${bandPen.name}（しきい値の設定なし）。`
				: `しきい値の帯: ${bandPen.name}（${bands.map((b) => bandText(b, bandPen.decimals)).join('、')}）。`
		);
	}
	return parts.join('');
}

// --- 注記 --------------------------------------------------------------------

/** 履歴（初期窓）の取得の状態。 */
export type TrendHistoryState = 'idle' | 'loading' | 'ready' | 'unavailable';

/** シミュレーション接続のペンに添える一文（D-3b の指示どおり）。 */
export const SIMULATION_NOTE =
	'シミュレーション接続の値は記録されないため、再読み込みで履歴は消えます';

export const HISTORY_UNAVAILABLE_NOTE =
	'直近の履歴を読み込めませんでした。表示を開いてからの値だけを描いています。';

/**
 * パネルに添える注記（純関数）。出す順に並べる。
 *
 * - 履歴を読めなかった（`unavailable`・失敗・時間切れ）: 現在値だけで描いていると言う
 *   （「履歴が 0 件」に潰さない）。
 * - 履歴に居ないタグ（消された等）: 名前を挙げる（1 本の誤りで全体を止めない）。
 * - シミュレーション接続のペン: 値が記録されないこと。
 */
export function trendNotices(input: {
	historyState: TrendHistoryState;
	unknownNames: readonly string[];
	simulationNames: readonly string[];
}): string[] {
	const notices: string[] = [];
	if (input.historyState === 'unavailable') notices.push(HISTORY_UNAVAILABLE_NOTE);
	if (input.unknownNames.length > 0) {
		notices.push(
			`次のタグは登録が見つからないため履歴がありません: ${input.unknownNames.join('、')}`
		);
	}
	if (input.simulationNames.length > 0) {
		notices.push(`${SIMULATION_NOTE}（${input.simulationNames.join('、')}）。`);
	}
	return notices;
}
