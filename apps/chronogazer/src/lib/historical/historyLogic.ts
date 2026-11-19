/**
 * ヒストリカル画面の純粋なロジック（#1207 PR2）: 期間の計算・検証・URL の読み書き・
 * `bins` の決め方・履歴の応答を描画の格子にする処理。画面（`routes/(app)/historical`）
 * は、これを呼んで描くだけにする。
 *
 * - **期間は 31 日まで**（2026-11-19 オーナー決定。`HISTORY_MAX_WINDOW_MS`、
 *   サーバーの検証は `to - from <= 31 日`）。描くタグは表示グループを選んで決め、その場で
 *   足し引きしない。
 * - **`LineChart` の x は並びの添字**（時刻の目盛ではない）。系列ごとに点の時刻が揃わない
 *   ので、期間を等間隔の「区間」に割った格子を作り、点を区間に写す（`buildHistoryGrid`）。
 * - **包絡で描く**: 履歴の点は区間の最小・最大。1 区間を 2 行（最小 → 最大）にして 1 本の
 *   線でつなぐと、山も谷も落とさずに、ペンごとに 1 本の色で描ける。行数は描画幅以下に
 *   する（`LineChart` は行数が幅を超えると素朴に間引いて山を落とすため）。
 * - 欠測（`null`）は 0 にしない。線を切る（`gaps: 'break'`）。
 */
import { isProviderError } from '@banto/admin-core';
import {
	HISTORY_MAX_BINS,
	HISTORY_MAX_POINTS,
	HISTORY_MAX_TAGS,
	HISTORY_MAX_WINDOW_MS,
	type CollectHistory,
	type CollectHistoryParams
} from '../banto/collectAdmin';
import { maxTrendRows, valueExtent, type TrendRow } from '../monitor/trendLogic';

export interface HistoryRange {
	fromMs: number;
	toMs: number;
}

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** 期間の既定の候補（幅）。 */
export const HISTORY_PRESETS: readonly { label: string; widthMs: number }[] = [
	{ label: '直近 1 時間', widthMs: HOUR_MS },
	{ label: '直近 8 時間', widthMs: 8 * HOUR_MS },
	{ label: '直近 24 時間', widthMs: DAY_MS },
	{ label: '直近 7 日', widthMs: 7 * DAY_MS },
	{ label: '直近 31 日', widthMs: HISTORY_MAX_WINDOW_MS }
];

/** URL を読めないときの既定の幅（直近 1 時間）。 */
export const DEFAULT_HISTORY_WIDTH_MS = HOUR_MS;

/** 1 つの画面で描くペンの上限（履歴の口のタグ上限）。 */
export const HISTORY_MAX_PENS = HISTORY_MAX_TAGS;

/** 再読み込みを束ねる時間（連続した操作・リサイズ、ms）。 */
export const HISTORY_DEBOUNCE_MS = 300;

// --- 期間 --------------------------------------------------------------------

/** 幅が `widthMs` で、`nowMs` に終わる期間。 */
export function presetRange(widthMs: number, nowMs: number): HistoryRange {
	return { fromMs: nowMs - widthMs, toMs: nowMs };
}

/** 既定の期間（直近 1 時間）。 */
export function defaultRange(nowMs: number): HistoryRange {
	return presetRange(DEFAULT_HISTORY_WIDTH_MS, nowMs);
}

export type RangeCheck = { ok: true } | { ok: false; message: string };

export const RANGE_INVALID_MESSAGE = '開始と終了の日時を入力してください。';
export const RANGE_ORDER_MESSAGE = '開始は終了より前の日時にしてください。';
export const RANGE_TOO_WIDE_MESSAGE = '期間は 31 日以内にしてください。';

/** 期間を確かめる（純関数）。外れたら読みに行かず、この文言を出す。 */
export function checkRange(range: HistoryRange): RangeCheck {
	const { fromMs, toMs } = range;
	if (!Number.isSafeInteger(fromMs) || !Number.isSafeInteger(toMs) || fromMs < 0 || toMs < 0) {
		return { ok: false, message: RANGE_INVALID_MESSAGE };
	}
	if (fromMs >= toMs) return { ok: false, message: RANGE_ORDER_MESSAGE };
	if (toMs - fromMs > HISTORY_MAX_WINDOW_MS) return { ok: false, message: RANGE_TOO_WIDE_MESSAGE };
	return { ok: true };
}

/** 幅が候補に当たるとき、その候補の幅（押した印に使う）。 */
export function activePresetWidth(range: HistoryRange): number | null {
	const width = range.toMs - range.fromMs;
	return HISTORY_PRESETS.find((p) => p.widthMs === width)?.widthMs ?? null;
}

/**
 * 同じ幅で期間をずらす（純関数）。`next` は現在時刻を超えない（超えるなら終わりを
 * 現在にして幅を保つ）。`prev` は 0（1970 年）より前へ行かない。
 */
export function shiftRange(
	range: HistoryRange,
	direction: 'prev' | 'next',
	nowMs: number
): HistoryRange {
	const width = range.toMs - range.fromMs;
	if (direction === 'prev') {
		const fromMs = Math.max(0, range.fromMs - width);
		return { fromMs, toMs: fromMs + width };
	}
	const toMs = Math.min(range.toMs + width, Math.max(nowMs, range.toMs));
	return { fromMs: toMs - width, toMs };
}

/** 「次へ」を押せるか（終わりが現在より前のとき）。 */
export function canShiftNext(range: HistoryRange, nowMs: number): boolean {
	return range.toMs < nowMs;
}

// --- datetime-local ------------------------------------------------------------

function pad(n: number, digits = 2): string {
	return String(n).padStart(digits, '0');
}

/** ミリ秒 → `<input type="datetime-local">` の値（ローカル時刻、分まで）。 */
export function toLocalInputValue(ms: number): string {
	const at = new Date(ms);
	if (Number.isNaN(at.getTime())) return '';
	return `${pad(at.getFullYear(), 4)}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}T${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/** `<input type="datetime-local">` の値（ローカル時刻）→ ミリ秒。読めなければ `null`。 */
export function fromLocalInputValue(value: string): number | null {
	if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(value)) return null;
	const ms = new Date(value).getTime();
	return Number.isFinite(ms) ? ms : null;
}

// --- URL ---------------------------------------------------------------------

/** 数字だけの URL の値をミリ秒に読む。ほかは `null`。 */
export function parseMsParam(raw: string | null): number | null {
	if (raw === null || !/^\d{1,16}$/.test(raw)) return null;
	const n = Number(raw);
	return Number.isSafeInteger(n) ? n : null;
}

export interface ResolvedRange {
	range: HistoryRange;
	/** URL の値が足りない・おかしくて既定に戻した。 */
	fellBack: boolean;
}

/**
 * URL の `from`・`to` から期間を決める（純関数）。両方無ければ既定（これは「戻した」
 * に数えない）。どちらかが無い・数でない・上限を外れる（前後・31 日超）ときは既定に戻す。
 */
export function resolveRange(
	rawFrom: string | null,
	rawTo: string | null,
	nowMs: number
): ResolvedRange {
	if (rawFrom === null && rawTo === null) return { range: defaultRange(nowMs), fellBack: false };
	const fromMs = parseMsParam(rawFrom);
	const toMs = parseMsParam(rawTo);
	if (fromMs !== null && toMs !== null) {
		const range = { fromMs, toMs };
		if (checkRange(range).ok) return { range, fellBack: false };
	}
	return { range: defaultRange(nowMs), fellBack: true };
}

/** 画面の URL のクエリ文字列（`?group=…&from=…&to=…`）。 */
export function historyQuery(groupId: number, range: HistoryRange): string {
	return `?group=${groupId}&from=${range.fromMs}&to=${range.toMs}`;
}

// --- bins --------------------------------------------------------------------

/**
 * 履歴の `bins`（純関数）。1 区間は 2 行（最小・最大）になるので、行数の上限
 * （描画域の幅・2000・ペン数 × bins ≤ 8000。`maxTrendRows`）の半分。リサイズのたびに
 * 読み直さないよう、50 以上は 25 刻みに切り下げる。
 */
export function historyBins(widthPx: number, tagCount: number): number {
	const rows = maxTrendRows(widthPx, tagCount);
	let bins = Math.max(1, Math.floor(rows / 2));
	if (bins >= 50) bins = Math.floor(bins / 25) * 25;
	return Math.min(bins, HISTORY_MAX_BINS, Math.floor(HISTORY_MAX_POINTS / Math.max(1, tagCount)));
}

/** 履歴の要求（純関数）。タグが無い・描画幅がまだ無い・期間が外れているなら `null`。 */
export function historyRequestFor(
	range: HistoryRange,
	penTagIds: readonly number[],
	widthPx: number
): CollectHistoryParams | null {
	const tagIds = [...new Set(penTagIds)].slice(0, HISTORY_MAX_PENS);
	if (tagIds.length === 0 || !(widthPx > 0) || !checkRange(range).ok) return null;
	return {
		tagIds,
		fromMs: range.fromMs,
		toMs: range.toMs,
		bins: historyBins(widthPx, tagIds.length)
	};
}

// --- 格子 --------------------------------------------------------------------

export interface HistoryGrid {
	/** 1 区間の幅（ms）。 */
	stepMs: number;
	/** 区間の数。行数はこの 2 倍。 */
	bins: number;
	/** 行 `2k` = 区間 k の最小、`2k + 1` = 最大。`t` は区間の始まり・中ほど。 */
	rows: TrendRow[];
}

/**
 * 履歴の応答を格子にする（純関数）。期間（両端を含む）を `bins` 個の区間に割り、
 * 点 `[tMs, tMs + 有効幅)` が重なる区間に最小・最大を畳み込む（区間内の最小の最小、
 * 最大の最大）。系列の無いペン（不明なタグ）・欠測の区間は `null` のまま。
 *
 * **有効幅 = `max(binMs, 収集周期, 1)`**（記録計の「1 サンプルは次のサンプルまで有効」。
 * 監視画面の PR #543 の `mergeHistory` と同じ）。収集周期が格子の 1 区間より長いタグ
 * （周期 60 秒・1 時間窓で 1 区間 6 秒など）は、点と点の間の区間が空になり、線が点の並びに
 * 細切れになるため。周期は `periodMsOf(tagId)`（収集グループの `periodMs`、無ければ `null`）。
 * 周期による延長は次の点（サーバーが返した欠測の点を含む）の手前で止める。欠測の点
 * （min/max が `null`）自体は書かないので、本当の欠測では線が切れる。
 * 次の点がある限り幅は次の点で止め、`binMs` を下限にしない: 収集周期を途中で変えると
 * `binMs` は読んだ周期の最大になり、素通しの点（`binMs` より狭い間隔）が良い値を後続の
 * 欠測の点の先まで延ばして、記録の途切れを隠してしまうため。
 */
export function buildHistoryGrid(
	range: HistoryRange,
	bins: number,
	penTagIds: readonly number[],
	history: Pick<CollectHistory, 'series'>,
	periodMsOf: (tagId: number) => number | null = () => null
): HistoryGrid {
	const span = range.toMs - range.fromMs + 1;
	const count = Math.max(1, Math.floor(bins));
	const stepMs = Math.max(1, Math.ceil(span / count));
	const n = Math.max(1, Math.ceil(span / stepMs));
	const lo: (number | null)[][] = penTagIds.map(() => Array.from({ length: n }, () => null));
	const hi: (number | null)[][] = penTagIds.map(() => Array.from({ length: n }, () => null));
	penTagIds.forEach((tagId, pen) => {
		const series = history.series.find((s) => s.tagId === tagId);
		if (!series) return;
		const binWidth = Math.max(1, series.binMs);
		const hold = Math.max(binWidth, periodMsOf(tagId) ?? 0);
		const points = [...series.points].sort((a, b) => a.tMs - b.tMs);
		for (let i = 0; i < points.length; i++) {
			const point = points[i];
			const next = points[i + 1];
			// 次の点（欠測の点も）までは延ばすが、その先へは延ばさない。
			const width =
				next !== undefined && next.tMs > point.tMs
					? Math.max(1, Math.min(hold, next.tMs - point.tMs))
					: hold;
			if (
				point.min === null ||
				point.max === null ||
				!Number.isFinite(point.min) ||
				!Number.isFinite(point.max)
			) {
				continue;
			}
			const first = Math.max(0, Math.floor((point.tMs - range.fromMs) / stepMs));
			const last = Math.min(n - 1, Math.floor((point.tMs + width - 1 - range.fromMs) / stepMs));
			for (let k = first; k <= last; k++) {
				const curLo = lo[pen][k];
				const curHi = hi[pen][k];
				lo[pen][k] = curLo === null ? point.min : Math.min(curLo, point.min);
				hi[pen][k] = curHi === null ? point.max : Math.max(curHi, point.max);
			}
		}
	});
	const rows: TrendRow[] = [];
	for (let k = 0; k < n; k++) {
		const t = range.fromMs + k * stepMs;
		rows.push({ t, values: lo.map((col) => col[k]) });
		rows.push({ t: t + Math.floor(stepMs / 2), values: hi.map((col) => col[k]) });
	}
	return { stepMs, bins: n, rows };
}

// --- 表示 --------------------------------------------------------------------

/** 目盛・ツールチップの日時の書式。期間が 1 日を超えるなら日付も、1 時間以内なら秒まで。 */
export function historyTimeLabel(epochMs: number, spanMs: number): string {
	const at = new Date(epochMs);
	if (Number.isNaN(at.getTime())) return String(epochMs);
	const hm = `${pad(at.getHours())}:${pad(at.getMinutes())}`;
	if (spanMs > DAY_MS) return `${pad(at.getMonth() + 1)}/${pad(at.getDate())} ${hm}`;
	if (spanMs <= HOUR_MS) return `${hm}:${pad(at.getSeconds())}`;
	return hm;
}

/** 期間の表示（「2026/11/19 09:00 〜 2026/11/19 10:00」）。 */
export function rangeLabel(range: HistoryRange): string {
	const text = (ms: number): string => {
		const at = new Date(ms);
		return `${pad(at.getFullYear(), 4)}/${pad(at.getMonth() + 1)}/${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
	};
	return `${text(range.fromMs)} 〜 ${text(range.toMs)}`;
}

/** 画面外の説明文（図は `role="img"` で中が読まれないため）。 */
export function historyDescription(input: {
	range: HistoryRange;
	penLabels: readonly string[];
	rows: readonly TrendRow[];
	bandPenName: string | null;
}): string {
	const parts = [
		`${rangeLabel(input.range)} のトレンド（各区間の最小と最大）。ペン: ${input.penLabels.join('、')}。`
	];
	const extent = valueExtent(input.rows);
	parts.push(
		extent === null
			? 'この期間に表示できる値はありません。'
			: `縦軸は全ペン共通の自動スケールで、表示中の値の範囲は ${extent.min}〜${extent.max}。`
	);
	parts.push(
		input.bandPenName === null ? 'しきい値の帯: なし。' : `しきい値の帯: ${input.bandPenName}。`
	);
	return parts.join('');
}

// --- 注記・エラー ------------------------------------------------------------

export const HISTORY_UNAVAILABLE_MESSAGE =
	'データを読み出せませんでした。しばらくしてからもう一度試してください。';
export const HISTORY_EMPTY_MESSAGE = 'この期間に記録された値はありません。';
export const HISTORY_LIMIT_MESSAGE =
	'期間は 31 日まで、ペンは 8 本までです。条件を見直してください。';

/** 履歴の読み出しが投げたもの → 日本語の文言（422 も日本語にする）。 */
export function historyErrorMessage(err: unknown): string {
	if (isProviderError(err)) {
		if (err.body.kind === 'validation') return HISTORY_LIMIT_MESSAGE;
		return `履歴を読み出せませんでした（${err.message}）。`;
	}
	return `履歴を読み出せませんでした（${err instanceof Error ? err.message : String(err)}）。`;
}

/** 不明なタグ（削除された）の注記。無ければ `null`。 */
export function unknownTagsNote(
	unknownTagIds: readonly number[],
	nameOf: (tagId: number) => string
): string | null {
	if (unknownTagIds.length === 0) return null;
	return `削除されたタグがあります（${unknownTagIds.map(nameOf).join('、')}）。そのタグの履歴は表示されません。`;
}

/** 値が 1 つも無い（全部欠測）か。 */
export function isEmptyGrid(grid: Pick<HistoryGrid, 'rows'>): boolean {
	return valueExtent(grid.rows) === null;
}
