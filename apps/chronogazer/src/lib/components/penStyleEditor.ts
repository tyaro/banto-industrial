/**
 * ペンの属性の編集欄「トレンドの表示」（`PenStyleEditor.svelte`）の判断を、DOM に依存しない
 * 純関数に出したもの（#1207・#1231 PR 8b2b、docs/chronogazer/trend-design.md §5・§10 の 8b2b）。
 * 表示グループのページ（`groups/groupsPageLogic.ts`）と自由トレンドの画面
 * （`free-trends/+page.svelte`）が同じ規則を使う。
 *
 * 設計の要点:
 * - ペンの属性は `PenStyle`（すべて `null` = 既定）。数の欄（縦軸の最小・最大、管理線の 4 つ）は
 *   **入力の文字**（`texts`）を別に持てる。"-" や "1e" のような途中の文字を黙って `null` にせず、
 *   数として読めない文字は {@link penStyleProblems} が保存を止める。
 * - 画面で止めるのはサーバーに送れない形・サーバーが断る形のうち、欄の中だけで判る誤り
 *   （数として読めない・縦軸の最小 >= 最大・管理線の上書きどうしの順序・名前と単位の長さ）。
 *   どちらもサーバーと同じ規則（`chronogazer_core` の検証）。
 */
import type { PenStyle } from '#lib/banto/displayGroupsAdmin.js';

/** 入力の文字で持つペンの数の欄。 */
export type PenNumberField = 'yMin' | 'yMax' | 'limitHh' | 'limitH' | 'limitL' | 'limitLl';
export const PEN_NUMBER_FIELDS: readonly PenNumberField[] = [
	'yMin',
	'yMax',
	'limitHh',
	'limitH',
	'limitL',
	'limitLl'
];

/** 数の欄の入力の文字（キーが無い = 触っていない = 欄の値をそのまま使う）。 */
export type PenNumberTexts = Partial<Record<PenNumberField, string>>;

/** ペンの属性と、数の欄の入力の文字。 */
export type PenStyleWithTexts = PenStyle & { texts?: PenNumberTexts };

/** 凡例の名前の上限（code point 数。Rust の `MAX_PEN_LABEL_CHARS` と同じ）。 */
export const MAX_PEN_LABEL_CHARS = 64;
/** 単位の上限（code point 数。Rust の `MAX_PEN_UNIT_CHARS` と同じ）。 */
export const MAX_PEN_UNIT_CHARS = 16;

/** 縦軸の入力の文字を数にする。空 = `null`（既定）、数として読めない = `'invalid'`。 */
export function parseAxisText(text: string): number | null | 'invalid' {
	const t = text.trim();
	if (t === '') return null;
	const n = Number(t);
	return Number.isFinite(n) ? n : 'invalid';
}

/** 数の欄の入力の文字（無ければ値から作る）。 */
export function axisText(pen: PenStyleWithTexts, which: PenNumberField): string {
	const text = pen.texts?.[which];
	if (text !== undefined) return text;
	const value = pen[which];
	return value === null || value === undefined ? '' : String(value);
}

function codePoints(text: string): number {
	return Array.from(text).length;
}

/**
 * ペンの属性だけを取り出す（保存の本文・未保存の判定・往復で同じ形にする）。凡例の名前と単位は
 * 前後の空白を落とし、空は `null`。数の欄は入力の文字があればそれを読む（読めなければ `null`。
 * 読めない文字は {@link penStyleProblems} が先に止める）。
 */
export function styleOf(pen: PenStyleWithTexts): PenStyle {
	const axisOf = (which: PenNumberField): number | null => {
		const text = pen.texts?.[which];
		if (text === undefined) return pen[which] ?? null;
		const parsed = parseAxisText(text);
		return parsed === 'invalid' ? null : parsed;
	};
	const trimmed = (text: string | null | undefined): string | null => {
		const t = (text ?? '').trim();
		return t === '' ? null : t;
	};
	return {
		label: trimmed(pen.label),
		unit: trimmed(pen.unit),
		decimals: pen.decimals ?? null,
		lineStyle: pen.lineStyle ?? null,
		lineWidth: pen.lineWidth ?? null,
		marker: pen.marker ?? null,
		markerSize: pen.markerSize ?? null,
		axis: pen.axis ?? null,
		yMin: axisOf('yMin'),
		yMax: axisOf('yMax'),
		limitLines: pen.limitLines ?? null,
		limitHh: axisOf('limitHh'),
		limitH: axisOf('limitH'),
		limitL: axisOf('limitL'),
		limitLl: axisOf('limitLl'),
		bitStyle: pen.bitStyle ?? null,
		bitBandHeight: pen.bitBandHeight ?? null
	};
}

/**
 * 保存の前に画面で止めるペンの属性の誤り（`n` は 1 始まりのペンの番号）。順序は凡例の名前 →
 * 単位 → 縦軸 → 管理線。
 */
export function penStyleProblems(pen: PenStyleWithTexts, n: number): string[] {
	const problems: string[] = [];
	const style = styleOf(pen);
	if (style.label !== null && codePoints(style.label) > MAX_PEN_LABEL_CHARS) {
		problems.push(`ペン ${n} の凡例の名前は ${MAX_PEN_LABEL_CHARS} 文字までです`);
	}
	if (style.unit !== null && codePoints(style.unit) > MAX_PEN_UNIT_CHARS) {
		problems.push(`ペン ${n} の単位は ${MAX_PEN_UNIT_CHARS} 文字までです`);
	}
	const read = (field: PenNumberField): number | null | 'invalid' => {
		const text = pen.texts?.[field];
		return text === undefined ? style[field] : parseAxisText(text);
	};
	const lo = read('yMin');
	const hi = read('yMax');
	if (lo === 'invalid') problems.push(`ペン ${n} の縦軸の最小が数ではありません`);
	if (hi === 'invalid') problems.push(`ペン ${n} の縦軸の最大が数ではありません`);
	if (typeof lo === 'number' && typeof hi === 'number' && lo >= hi) {
		problems.push(`ペン ${n} の縦軸は、最小を最大より小さくしてください`);
	}
	// 管理線の上書き（設定したものだけ。LL <= L <= H <= HH。サーバーと同じ）。
	const limitFields = [
		['limitLl', 'LL'],
		['limitL', 'L'],
		['limitH', 'H'],
		['limitHh', 'HH']
	] as const;
	const set: { label: string; value: number }[] = [];
	for (const [field, label] of limitFields) {
		const value = read(field);
		if (value === 'invalid') problems.push(`ペン ${n} の管理線の ${label} が数ではありません`);
		else if (value !== null) set.push({ label, value });
	}
	for (let i = 1; i < set.length; i += 1) {
		if (set[i - 1].value > set[i].value) {
			problems.push(
				`ペン ${n} の管理線は、LL ≦ L ≦ H ≦ HH の順にしてください（${set[i - 1].label} が ${set[i].label} より大きい）`
			);
			break;
		}
	}
	return problems;
}

/**
 * タグのしきい値（#532: 記録計の側のタグごとの設定。`/api/tag-thresholds` が返す形の
 * 一部。タグ定義の属性ではない）。
 */
export interface TagThresholds {
	thresholdLl?: number | null;
	thresholdL?: number | null;
	thresholdH?: number | null;
	thresholdHh?: number | null;
}

/** しきい値を読めなかったときのペンの表示。 */
export const THRESHOLDS_UNREADABLE = '読み込めませんでした';

/**
 * 管理線の入力の placeholder（純関数）。タグ側のしきい値を目安に出す。
 *
 * - タグがまだ無い（未登録の Hub のタグ）→ 「タグの設定なし」
 * - しきい値を読めなかった → 読めなかったことを出す（設定なしに潰さない）
 * - まだ読んでいない → 空
 * - 値があれば「タグ: <値>」、無ければ「タグの設定なし」
 */
export function limitPlaceholder(
	tagId: number | null,
	thresholds: ReadonlyMap<number, TagThresholds> | null,
	thresholdsFailed: boolean,
	key: 'thresholdHh' | 'thresholdH' | 'thresholdL' | 'thresholdLl'
): string {
	if (tagId === null) return 'タグの設定なし';
	if (thresholds === null) {
		return thresholdsFailed ? `タグのしきい値: ${THRESHOLDS_UNREADABLE}` : '';
	}
	const value = thresholds.get(tagId)?.[key];
	return typeof value === 'number' ? `タグ: ${value}` : 'タグの設定なし';
}

/** 帯の太さの選択肢。保存されている値が並びに無ければ、その値も出す。 */
export const BIT_BAND_HEIGHT_VALUES: readonly number[] = [8, 12, 16, 20, 24, 28, 32];
export function bitBandHeightOptions(current: number | null): readonly number[] {
	return current === null || BIT_BAND_HEIGHT_VALUES.includes(current)
		? BIT_BAND_HEIGHT_VALUES
		: [...BIT_BAND_HEIGHT_VALUES, current].sort((a, b) => a - b);
}
