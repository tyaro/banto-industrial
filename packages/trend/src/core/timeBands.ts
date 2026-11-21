/**
 * 時刻の区間の帯（`timeBands`）と時刻の印（`timeMarks`）の位置の計算。描画の層（SVG・後の
 * Canvas）から切り離した計算だけ（設計書 §10 の 10、#1273）。区間の状態（欠測の理由・
 * 部分的な区間など）は `Bin` に足さず、利用側が `penId` 付きの帯で渡す。
 *
 * 決め:
 * - 区間は `[fromMs, toMs)`。表示範囲（`range`）で切る（取得範囲では切らない）。`fromMs >= toMs`・
 *   非有限・表示範囲と重ならない帯は出さない。幅が 1px 未満になった帯は 1px に広げる。
 * - `penId` 無しの帯は描画域の全高（アナログの域と bit の帯の域の両方）に塗る（`background`。
 *   高さは描画側が描画域の高さにする）。
 * - `penId` 付きの帯: そのペンが bit の帯なら、そのペンの帯の高さの中（`bitOverlays`）。
 *   アナログのペンなら、アナログの域の上端に、ペンごとの細い列（高さ 4px・間 1px）を、
 *   「penId 付きの帯を持つアナログのペン」だけ、ペンの並びの順に上から積む（`lanes`）。
 *   `penId` がペンの組に無い帯は出さない。列の合計がアナログの域の半分を超えるときは、列の高さと
 *   間を同じ割合で縮めて半分に収める（`analogHeight` が 0 以下・非有限なら列は出さない）。
 * - 印は縦の線（描画域の全高）。表示範囲 `[startMs, endMs)` の外は出さない。色は `colorVar`、
 *   無ければ `penId` のペンの色、どちらも無ければ `--banto-text-muted`。名前は、前に名前を出した
 *   印から 48px 未満なら省く（線と `<title>` は残す）。時刻の順に判定する。
 */

import { isBitBand, penColor, type TrendPen } from './pens';
import type { BitBandSlot } from './bitBands';

/** 時刻の帯 1 つ。区間は `[fromMs, toMs)`。 */
export interface TrendTimeBand {
	fromMs: number;
	toMs: number;
	/** 塗りの色（CSS の値）。 */
	colorVar: string;
	/** 帯の名前（`<title>`）。 */
	label: string;
	/** あればそのペンの帯・列に描く。無ければ描画域の全高。 */
	penId?: string | null;
}

/** 時刻の印 1 つ。 */
export interface TrendTimeMark {
	tMs: number;
	label: string;
	penId?: string | null;
	colorVar?: string | null;
}

/** 描画域の中の帯の矩形（px。x・y は描画域の左上から）。 */
export interface TimeBandRect {
	x: number;
	y: number;
	width: number;
	height: number;
	colorVar: string;
	label: string;
}

export interface TimeBandLayout {
	/** 全高の帯（`y`・`height` は使わない。描画域の全高に塗る）。 */
	background: Omit<TimeBandRect, 'y' | 'height'>[];
	/** bit の帯の中に重ねる帯。 */
	bitOverlays: TimeBandRect[];
	/** アナログの域の上端の列の帯。 */
	lanes: TimeBandRect[];
}

export interface TimeMarkLine {
	x: number;
	label: string;
	colorVar: string;
	showLabel: boolean;
}

export const TIME_LANE_HEIGHT = 4;
export const TIME_LANE_GAP = 1;
export const TIME_MARK_LABEL_MIN_GAP = 48;
export const TIME_MARK_DEFAULT_COLOR = 'var(--banto-text-muted)';

const fin = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);

interface RangeLike {
	startMs: number;
	endMs: number;
}

function validRange(range: RangeLike, plotWidth: number): boolean {
	return (
		fin(range.startMs) &&
		fin(range.endMs) &&
		range.endMs > range.startMs &&
		fin(plotWidth) &&
		plotWidth > 0
	);
}

export function timeBandLayout(
	bands: readonly TrendTimeBand[] | null | undefined,
	pens: readonly TrendPen[],
	range: RangeLike,
	plotWidth: number,
	bitSlots: readonly BitBandSlot[],
	analogHeight: number
): TimeBandLayout {
	const out: TimeBandLayout = { background: [], bitOverlays: [], lanes: [] };
	if (!bands || bands.length === 0 || !validRange(range, plotWidth)) return out;
	const span = range.endMs - range.startMs;
	const clip = (b: TrendTimeBand): { x: number; width: number } | null => {
		if (!fin(b.fromMs) || !fin(b.toMs) || !(b.fromMs < b.toMs)) return null;
		const a = Math.max(b.fromMs, range.startMs);
		const z = Math.min(b.toMs, range.endMs);
		if (!(z > a)) return null;
		const x = ((a - range.startMs) / span) * plotWidth;
		const w = ((z - a) / span) * plotWidth;
		return { x, width: Math.max(1, w) };
	};
	// penId 付きの帯を持つアナログのペンの列（ペンの並びの順）。
	const hasBand = new Set<string>();
	for (const b of bands) if (b.penId && clip(b)) hasBand.add(b.penId);
	const laneOf = new Map<string, number>();
	for (const p of pens) {
		if (hasBand.has(p.id) && !isBitBand(p)) laneOf.set(p.id, laneOf.size);
	}
	const n = laneOf.size;
	const rawLanes = n * TIME_LANE_HEIGHT + Math.max(0, n - 1) * TIME_LANE_GAP;
	const laneBudget = fin(analogHeight) && analogHeight > 0 ? analogHeight * 0.5 : 0;
	const k = rawLanes > laneBudget ? laneBudget / rawLanes : 1;
	for (const b of bands) {
		const c = clip(b);
		if (!c) continue;
		const base = { x: c.x, width: c.width, colorVar: b.colorVar, label: b.label };
		if (!b.penId) {
			out.background.push(base);
			continue;
		}
		const index = pens.findIndex((p) => p.id === b.penId);
		if (index < 0) continue;
		if (isBitBand(pens[index])) {
			const slot = bitSlots.find((s) => s.penIndex === index);
			if (slot) out.bitOverlays.push({ ...base, y: slot.y, height: slot.height });
		} else {
			if (k <= 0) continue;
			const lane = laneOf.get(b.penId) ?? 0;
			out.lanes.push({
				...base,
				y: lane * (TIME_LANE_HEIGHT + TIME_LANE_GAP) * k,
				height: TIME_LANE_HEIGHT * k
			});
		}
	}
	return out;
}

export function timeMarkLayout(
	marks: readonly TrendTimeMark[] | null | undefined,
	pens: readonly TrendPen[],
	range: RangeLike,
	plotWidth: number
): TimeMarkLine[] {
	if (!marks || marks.length === 0 || !validRange(range, plotWidth)) return [];
	const span = range.endMs - range.startMs;
	const items = marks
		.filter((m) => fin(m.tMs) && m.tMs >= range.startMs && m.tMs < range.endMs)
		.slice()
		.sort((a, b) => a.tMs - b.tMs);
	const out: TimeMarkLine[] = [];
	let lastLabelX = -Infinity;
	for (const m of items) {
		const x = ((m.tMs - range.startMs) / span) * plotWidth;
		const pen = m.penId ? pens.find((p) => p.id === m.penId) : undefined;
		const colorVar = m.colorVar?.trim()
			? m.colorVar
			: pen
				? penColor(pen)
				: TIME_MARK_DEFAULT_COLOR;
		const showLabel = x - lastLabelX >= TIME_MARK_LABEL_MIN_GAP;
		if (showLabel) lastLabelX = x;
		out.push({ x, label: m.label, colorVar, showLabel });
	}
	return out;
}

/** 要約に足す短い文（0 件なら空）。件数は渡された件数。 */
export function timeSummaryText(bandCount: number, markCount: number): string {
	const parts: string[] = [];
	if (bandCount > 0) parts.push(`時刻の帯 ${bandCount}`);
	if (markCount > 0) parts.push(`印 ${markCount}`);
	return parts.length > 0 ? `${parts.join('・')}。` : '';
}
