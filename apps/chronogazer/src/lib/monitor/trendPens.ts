/**
 * 共通のトレンドの部品（`@banto-industrial/trend`）のペンの作り方。監視画面（リアルタイム）と
 * ヒストリカル画面が同じペンの ID・`TrendPen` を使う（取得した履歴・格子の系列を、ペンの
 * ID で部品へ渡すため）。
 */
import type { TrendLimit, TrendPen } from '@banto-industrial/trend';
import {
	TREND_DANGER_COLOR,
	TREND_WARNING_COLOR,
	type ResolvedLimits,
	type TrendPenInfo
} from './trendLogic';

/**
 * 部品のペンの ID。並びの位置とタグ ID から作る（同じタグを 2 回置いても衝突しない）。
 * ヒストリカルの読み出しの口（`createHistoryFetch`）はここからタグ ID を読み戻す。
 */
export function trendPenId(index: number, tagId: number): string {
	return `pen-${index}-tag-${tagId}`;
}

/** `trendPenId` からタグ ID を読む。形が違えば `null`。 */
export function tagIdOfTrendPenId(penId: string): number | null {
	const m = /^pen-\d+-tag-(\d+)$/.exec(penId);
	if (!m) return null;
	const n = Number(m[1]);
	return Number.isSafeInteger(n) ? n : null;
}

/**
 * 部品の管理線。並びは H → L → HH → LL（越えた区間の色は後ろが勝つので、注意を両方先、より危険な HH・LL を両方後ろに。上限と下限が重なる区間でも危険が勝つ）。
 * 値の無いものは入れない。
 */
export function toTrendLimits(limits: ResolvedLimits | null): TrendLimit[] {
	if (limits === null) return [];
	const out: TrendLimit[] = [];
	const add = (value: number | null, side: 'upper' | 'lower', label: string, colorVar: string) => {
		if (value !== null) out.push({ value, side, colorVar, label });
	};
	add(limits.h, 'upper', 'H', TREND_WARNING_COLOR);
	add(limits.l, 'lower', 'L', TREND_WARNING_COLOR);
	add(limits.hh, 'upper', 'HH', TREND_DANGER_COLOR);
	add(limits.ll, 'lower', 'LL', TREND_DANGER_COLOR);
	return out;
}

/**
 * 部品のペン（純関数）。色の枠はペンの `colorSlot`（`--banto-chart-<枠>`）。bit のタグは
 * 縦軸を 0〜1 に固定し、小数桁は指定しない（0・1 はそのまま、区間の中点は 0.5 と出る）。
 */
export function toTrendPens(pens: readonly TrendPenInfo[]): TrendPen[] {
	return pens.map((pen, index) => ({
		id: trendPenId(index, pen.tagId),
		// 単位は部品が凡例の範囲・カーソルの値・要約に足すので、名前だけを渡す（二重に出さない）。
		label: pen.name,
		colorSlot: pen.colorSlot,
		unit: pen.unit,
		decimals: pen.isBit ? null : pen.decimals,
		...(pen.axis !== null ? { axis: pen.axis } : {}),
		...(pen.lineStyle !== null ? { lineStyle: pen.lineStyle } : {}),
		...(pen.lineWidth !== null ? { lineWidth: pen.lineWidth } : {}),
		...(pen.marker !== null ? { marker: pen.marker } : {}),
		...(pen.markerSize !== null ? { markerSize: pen.markerSize } : {}),
		...(pen.limits !== null ? { limits: toTrendLimits(pen.limits) } : {}),
		...(pen.isBit
			? { yMin: 0, yMax: 1 }
			: {
					...(pen.yMin !== null ? { yMin: pen.yMin } : {}),
					...(pen.yMax !== null ? { yMax: pen.yMax } : {}),
					...(pen.engRange !== null ? { engRange: pen.engRange } : {})
				})
	}));
}
