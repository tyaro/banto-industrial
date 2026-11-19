/**
 * 共通のトレンドの部品（`@banto-industrial/trend`）のペンの作り方。監視画面（リアルタイム）と
 * ヒストリカル画面が同じペンの ID・`TrendPen` を使う（取得した履歴・格子の系列を、ペンの
 * ID で部品へ渡すため）。
 */
import type { TrendPen } from '@banto-industrial/trend';
import { penLegendLabel, type TrendPenInfo } from './trendLogic';

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
 * 部品のペン（純関数）。色の枠はペンの `colorSlot`（`--banto-chart-<枠>`）。bit のタグは
 * 縦軸を 0〜1・小数 0 桁にする（値は数値の 0 / 1 で出る）。
 */
export function toTrendPens(pens: readonly TrendPenInfo[]): TrendPen[] {
	return pens.map((pen, index) => ({
		id: trendPenId(index, pen.tagId),
		label: penLegendLabel(pen),
		colorSlot: pen.colorSlot,
		decimals: pen.isBit ? 0 : pen.decimals,
		...(pen.isBit ? { yMin: 0, yMax: 1 } : {})
	}));
}
