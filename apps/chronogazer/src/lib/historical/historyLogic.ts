/**
 * ヒストリカル画面の純粋なロジック（#1207 PR2・#1231 PR 2b）: 期間の計算・検証・URL の
 * 読み書き・`bins` の決め方・履歴の応答を共通のトレンドの部品（`@banto-industrial/trend`）の
 * 形にする処理。画面（`routes/(app)/historical`）は、これを呼んで描くだけにする。
 *
 * - **期間は 31 日まで**（2026-11-19 オーナー決定。`HISTORY_MAX_WINDOW_MS`、
 *   サーバーの検証は `to - from <= 31 日`）。描くタグは表示グループを選んで決め、その場で
 *   足し引きしない。
 * - 描画は `TrendChart`（時刻の軸・ペンごとの縦軸・区間の MIN/MAX の包絡）。区間の幅は
 *   API が返した実際の `binMs`（サーバーが収集周期まで広げる）を系列と組で渡す。欠測
 *   （`null`）は線を切る。
 * - 期間は 2 つの形を持つ: 画面の URL と履歴の口は両端を含む `HistoryRange { fromMs, toMs }`、
 *   部品は半開区間 `TimeRange [startMs, endMs)`。変換は `toTimeRange`・`toHistoryRange` だけで行う。
 */
import { isProviderError } from '@banto/admin-core';
import {
	binCount,
	dataExtent,
	drawableSeries,
	type TimeRange,
	type TrendData,
	type TrendFetch,
	type TrendSeries
} from '@banto-industrial/trend';
import {
	HISTORY_MAX_BINS,
	HISTORY_MAX_POINTS,
	HISTORY_MAX_TAGS,
	HISTORY_MAX_WINDOW_MS,
	type CollectHistory,
	type CollectHistoryParams,
	type Readout
} from '../banto/collectAdmin';
import { tagIdOfTrendPenId } from '../monitor/trendPens';

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

/** 拡大・縮小・パンが止まってから URL を合わせるまでの時間（ms。履歴は積まない）。 */
export const HISTORY_URL_SYNC_MS = 300;

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

// --- 期間の 2 つの形 ------------------------------------------------------------

/**
 * `HistoryRange`（両端を含む `[fromMs, toMs]`。URL・履歴の口・CSV 出力）→ 部品の
 * `TimeRange`（半開区間 `[startMs, endMs)`）。ミリ秒の整数なので、両端を含む `toMs` は
 * 半開区間の `toMs + 1` と同じ時刻の集まり。幅は 1 ms 増える（31 日ちょうどの期間は
 * `MAX_RANGE_MS` を 1 ms 超えるが、部品は範囲を受けるだけで、拡大・縮小のときに 31 日へ丸める）。
 */
export function toTimeRange(range: HistoryRange): TimeRange {
	return { startMs: range.fromMs, endMs: range.toMs + 1 };
}

/** `toTimeRange` の逆。部品の範囲（拡大・縮小・パンの結果）を URL・履歴の口の形にする。 */
export function toHistoryRange(range: TimeRange): HistoryRange {
	return { fromMs: range.startMs, toMs: range.endMs - 1 };
}

/** 2 つの期間が同じか。 */
export function sameHistoryRange(a: HistoryRange, b: HistoryRange): boolean {
	return a.fromMs === b.fromMs && a.toMs === b.toMs;
}

// --- bins --------------------------------------------------------------------

/**
 * 履歴の `bins`（純関数）。区間の数は描画域の幅から（`binCount`: 1 px に 1 区間、2000 まで）。
 * ただし履歴の口の予算（系列 1 本 `HISTORY_MAX_BINS`・本数 × bins ≤ `HISTORY_MAX_POINTS`）を
 * 越えない。リサイズのたびに読み直さないよう、50 以上は 25 刻みに切り下げる。
 */
export function historyBins(plotWidthPx: number, tagCount: number): number {
	let bins = binCount(plotWidthPx);
	if (bins >= 50) bins = Math.floor(bins / 25) * 25;
	return Math.max(
		1,
		Math.min(bins, HISTORY_MAX_BINS, Math.floor(HISTORY_MAX_POINTS / Math.max(1, tagCount)))
	);
}

/** 履歴の要求（純関数）。タグが無い・描画幅がまだ無い・期間が外れているなら `null`。 */
export function historyRequestFor(
	range: HistoryRange,
	penTagIds: readonly number[],
	plotWidthPx: number
): CollectHistoryParams | null {
	const tagIds = [...new Set(penTagIds)].slice(0, HISTORY_MAX_PENS);
	if (tagIds.length === 0 || !(plotWidthPx > 0) || !checkRange(range).ok) return null;
	return {
		tagIds,
		fromMs: range.fromMs,
		toMs: range.toMs,
		bins: historyBins(plotWidthPx, tagIds.length)
	};
}

// --- ペンと読み出し ------------------------------------------------------------------

/** 収集が使えず読めなかった（`Readout` が `ready` でない）。画面は「読み出せませんでした」を出す。 */
export class HistoryUnavailableError extends Error {
	constructor() {
		super(HISTORY_UNAVAILABLE_MESSAGE);
		this.name = 'HistoryUnavailableError';
	}
}

/** 読み出しの付帯情報（部品の契約に入らないもの）。 */
export interface HistoryExtras {
	/** レジストリに居ないタグ（削除された）。 */
	unknownTagIds: number[];
	/** 系列が `simulation`（記録されない接続）のタグ。監視のトレンドが注記に使う。 */
	simulationTagIds: number[];
}

/**
 * 部品の `TrendFetch` を作る。`read` は `getCollectHistory`（試験では差し替える）。
 *
 * - ペンの ID からタグ ID を読み、重複を除いて 1 回で読む。系列はペンごとに
 *   `{ bins: 点（tMs の昇順）, binMs: API の実際の幅 }` にする（同じタグのペンは同じ系列を共有）。
 * - `Readout` が `ready` でなければ `HistoryUnavailableError` を投げる（空に潰さない）。
 * - `unknownTagIds` は返すオブジェクトを鍵にして `extras` に置く（ローダーの `onResult` は
 *   このオブジェクトそのものを `data.series` で渡すので、画面はそこから引ける）。
 */
export function createHistoryFetch(
	read: (params: CollectHistoryParams, signal: AbortSignal) => Promise<Readout<CollectHistory>>,
	extras: WeakMap<object, HistoryExtras>
): TrendFetch {
	return async ({ range, penIds, bins, signal }) => {
		const tagOf = new Map<string, number>();
		for (const id of penIds) {
			const tagId = tagIdOfTrendPenId(id);
			if (tagId !== null) tagOf.set(id, tagId);
		}
		const hr = toHistoryRange(range);
		const tagIds = [...new Set(tagOf.values())];
		const readout = await read({ tagIds, fromMs: hr.fromMs, toMs: hr.toMs, bins }, signal);
		if (readout.state !== 'ready') throw new HistoryUnavailableError();
		const byTag = new Map<number, TrendSeries>();
		for (const s of readout.data.series) {
			byTag.set(s.tagId, {
				bins: [...s.points].sort((a, b) => a.tMs - b.tMs),
				binMs: s.binMs
			});
		}
		const out: Record<string, TrendSeries> = {};
		for (const [id, tagId] of tagOf) {
			const s = byTag.get(tagId);
			if (s) out[id] = s;
		}
		extras.set(out, {
			unknownTagIds: [...readout.data.unknownTagIds],
			simulationTagIds: readout.data.series.filter((s) => s.simulation).map((s) => s.tagId)
		});
		return out;
	};
}

// --- 表示 --------------------------------------------------------------------

/** 期間の表示（「2026/11/19 09:00 〜 2026/11/19 10:00」）。 */
export function rangeLabel(range: HistoryRange): string {
	const text = (ms: number): string => {
		const at = new Date(ms);
		return `${pad(at.getFullYear(), 4)}/${pad(at.getMonth() + 1)}/${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
	};
	return `${text(range.fromMs)} 〜 ${text(range.toMs)}`;
}

/** 表示中（取得範囲 ∩ 表示範囲）の値の範囲（全ペン）。値が無ければ `null`。 */
export function historyExtent(
	data: TrendData | null,
	view: TimeRange
): { min: number; max: number } | null {
	let min = Infinity;
	let max = -Infinity;
	for (const s of Object.values(drawableSeries(data, view).series)) {
		const e = dataExtent(s.bins);
		if (e === null) continue;
		min = Math.min(min, e[0]);
		max = Math.max(max, e[1]);
	}
	return min <= max ? { min, max } : null;
}

/** 画面外の説明文（各ペンの縦軸の範囲は部品の説明文が読む）。 */
export function historyDescription(input: {
	range: HistoryRange;
	penLabels: readonly string[];
	extent: { min: number; max: number } | null;
	bandPenName: string | null;
}): string {
	const parts = [
		`${rangeLabel(input.range)} のトレンド（各区間の最小と最大）。ペン: ${input.penLabels.join('、')}。`
	];
	parts.push(
		input.extent === null
			? 'この期間に表示できる値はありません。'
			: `縦軸はペンごとの範囲で、表示中の値の範囲は ${input.extent.min}〜${input.extent.max}。`
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
	'期間は 31 日まで、ペンは 16 本までです。条件を見直してください。';

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
