/**
 * 模擬のタグの履歴（設計 docs/demo-pages-design.md §3.3・§6 の 5）。{@link demoTagValue} と同じ
 * 純関数の値を、期間を `bins` 個に区切った区間ごとに標本して、最小・最大にする。状態を持たず、
 * 同じ入力なら必ず同じ結果（履歴を時刻で引き直せる）。
 *
 * 本物（`apps/chronogazer/core/src/collect/history.rs`）に合わせた点:
 * - 検証は `validate_history_request`（タグ 1〜16 本・期間は `fromMs < toMs` かつ 31 日以内・
 *   `bins` は 1〜2000・本数 × `bins` ≤ 32000）。誤りの `field` も文言も同じ（`tagIds`・`toMs`・`bins`）。
 * - 返す点は系列ごとに `bins` 以下。点は `{ tMs, min, max }` だけ（平均は無い）。`tMs` は区間の始まり。
 *   値の無い区間（文字列のタグ・収集の対象外）は `min`・`max` とも `null`（線を切る）。
 * - 期間は両端を含み、区間の幅は `ceil((toMs - fromMs + 1) / bins)`。
 *
 * 本物と違う点: 記録の無い未来（`nowMs` より後）は点を出さない。シミュレーション接続の値は
 * 本物では記録されず `null` だが、デモは収集を動かさないので模擬の値を出す。
 */
import { demoTagValue, type DemoTagValueInput } from './tagValues';

/** 履歴 1 回で読めるタグの本数・期間・区間の数・応答全体の点数の上限（本物の `HISTORY_MAX_*`）。 */
export const DEMO_HISTORY_MAX_TAGS = 16;
export const DEMO_HISTORY_MAX_WINDOW_MS = 31 * 24 * 3_600_000;
export const DEMO_HISTORY_MAX_BINS = 2000;
export const DEMO_HISTORY_MAX_POINTS = 32000;

/** 1 区間で標本する点の上限。 */
const MAX_SAMPLES_PER_BIN = 24;

export interface DemoHistoryFieldError {
	field: string;
	message: string;
}

export interface DemoHistoryPoint {
	tMs: number;
	min: number | null;
	max: number | null;
}

export interface DemoHistoryTag extends DemoTagValueInput {
	/** 収集の対象か（タグ・グループ・接続の全部が有効）。外れていれば全区間が欠測。 */
	collected: boolean;
}

/** 履歴の要求の検証。誤りが無ければ空（本物と同じ文言・同じ `field`）。 */
export function validateDemoHistoryRequest(
	tagIds: readonly number[],
	fromMs: number,
	toMs: number,
	bins: number
): DemoHistoryFieldError[] {
	const errors: DemoHistoryFieldError[] = [];
	const unique = new Set(tagIds).size;
	if (unique === 0) {
		errors.push({ field: 'tagIds', message: 'タグを 1 つ以上指定してください' });
	} else if (unique > DEMO_HISTORY_MAX_TAGS) {
		errors.push({
			field: 'tagIds',
			message: `タグは ${DEMO_HISTORY_MAX_TAGS} 個までです（${unique} 個）`
		});
	}
	const width = toMs - fromMs;
	if (!Number.isFinite(width) || width <= 0) {
		errors.push({ field: 'toMs', message: '終わりの時刻は始まりの時刻より後にしてください' });
	} else if (width > DEMO_HISTORY_MAX_WINDOW_MS) {
		errors.push({
			field: 'toMs',
			message: `期間は ${DEMO_HISTORY_MAX_WINDOW_MS / 1000} 秒以内にしてください`
		});
	}
	if (!Number.isInteger(bins) || bins < 1 || bins > DEMO_HISTORY_MAX_BINS) {
		errors.push({ field: 'bins', message: `bins は 1〜${DEMO_HISTORY_MAX_BINS} にしてください` });
	} else if (
		unique > 0 &&
		unique <= DEMO_HISTORY_MAX_TAGS &&
		unique * bins > DEMO_HISTORY_MAX_POINTS
	) {
		errors.push({
			field: 'bins',
			message: `タグの本数 × bins は ${DEMO_HISTORY_MAX_POINTS} 以下にしてください（${unique} × ${bins}）`
		});
	}
	return errors;
}

/**
 * タグ 1 本の履歴。区間の幅は `ceil((toMs - fromMs + 1) / bins)`、点は区間の始まりの時刻で
 * `bins` 個以下。`nowMs` より後に始まる区間は点を出さず、`nowMs` をまたぐ区間は `nowMs` までを標本する。
 * 検証済みの要求を渡すこと。
 */
export function demoTagHistory(
	tag: DemoHistoryTag,
	fromMs: number,
	toMs: number,
	bins: number,
	nowMs: number
): { binMs: number; points: DemoHistoryPoint[] } {
	const binMs = Math.ceil((toMs - fromMs + 1) / bins);
	const points: DemoHistoryPoint[] = [];
	for (let i = 0; i < bins; i++) {
		const start = fromMs + i * binMs;
		if (start > toMs || start > nowMs) break;
		const end = Math.min(start + binMs - 1, toMs, nowMs);
		points.push({ tMs: start, ...binRange(tag, start, end) });
	}
	return { binMs, points };
}

function binRange(
	tag: DemoHistoryTag,
	start: number,
	end: number
): { min: number | null; max: number | null } {
	if (!tag.collected) return { min: null, max: null };
	const samples = Math.min(MAX_SAMPLES_PER_BIN, end - start + 1);
	let min: number | null = null;
	let max: number | null = null;
	for (let j = 0; j < samples; j++) {
		const t = samples === 1 ? start : start + Math.round(((end - start) * j) / (samples - 1));
		const v = demoTagValue(tag, t);
		if (v === null) return { min: null, max: null };
		if (min === null || v < min) min = v;
		if (max === null || v > max) max = v;
	}
	return { min, max };
}
