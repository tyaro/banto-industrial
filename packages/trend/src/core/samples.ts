/**
 * 観測点の系列（`TrendSeries.kind === 'samples'`、#1274）の計算。描画の層から切り離した計算だけ。
 *
 * 決め:
 * - `bins` の各要素は観測 1 つ。`tMs` は観測の時刻そのもの、値は `min`（無ければ `max`）。両方 null・非有限は
 *   「記録した欠測」。`tMs` の昇順（契約は 'bins' と同じ）。`binMs` は記録周期。
 * - 次の観測まで `binMs × 2` を超えて空いたら線を切る（`SAMPLE_GAP_FACTOR`）。値の無い観測でも線を切る。
 *   `binMs` が正の有限でないときは空きでは切らない。
 * - 線はアナログなら観測点どうしの直線、bit の段差（`step`）は前の値を次の観測まで保つ段差。点は実時刻に置く。
 * - マーカーは実時刻に置き、間引かない。丸めた座標が直前の点とまったく同じ点だけ省く。
 * - カーソルは時刻に最も近い観測に、許容の幅（ms）以内なら吸着する（`sampleAt`。候補は有効範囲の中の観測だけ）。
 * - 線は有効範囲の x の幅で切る（`effectiveClip`。外の観測は補間のために残すだけ）。
 * - 範囲の絞り込みは、範囲の中の観測に加えて両側の外の観測を 1 つずつ残す（線を描画域の端までつなぐため。
 *   縦軸・空の判定には外の点を含めない: `samplesInRange`）。
 * - 帯の塗りは観測から次の観測まで（`binMs × 2` と有効範囲の端で打ち切る。最後の観測は `binMs` 分）。
 */

import type { Bin } from './pens';

type Scale = (v: number) => number;
interface Range {
	startMs: number;
	endMs: number;
}

/** 線を切る空き = 記録周期のこの倍数を超えたとき。 */
export const SAMPLE_GAP_FACTOR = 2;

const fin = (v: number | null | undefined): v is number =>
	typeof v === 'number' && Number.isFinite(v);
const r2 = (v: number) => Math.round(v * 100) / 100;

/** 観測の値。`min`、無ければ `max`。有限の値が無ければ null。 */
export function sampleValue(b: Bin): number | null {
	return fin(b.min) ? b.min : fin(b.max) ? b.max : null;
}

/** 有効範囲の中の観測に、両側の外の観測を 1 つずつ足したもの（`tMs` が非有限のものは除く）。 */
export function samplesForDraw(bins: readonly Bin[], range: Range): Bin[] {
	const out: Bin[] = [];
	let before: Bin | null = null;
	let after: Bin | null = null;
	for (const b of bins) {
		if (!fin(b.tMs)) continue;
		if (b.tMs < range.startMs) before = b;
		else if (b.tMs < range.endMs) out.push(b);
		else {
			after = b;
			break;
		}
	}
	return [...(before ? [before] : []), ...out, ...(after ? [after] : [])];
}

/**
 * 有効範囲 `[startMs, endMs)` の中の観測だけ（縦軸・空の判定・カーソル用）。
 * 値は `sampleValue` の 1 値に揃える（`min = max = v`、欠測は両方 null）。縦軸・空の判定・カーソルが描画と同じ値を見る。
 */
export function samplesInRange(bins: readonly Bin[], range: Range): Bin[] {
	return bins
		.filter((b) => fin(b.tMs) && b.tMs >= range.startMs && b.tMs < range.endMs)
		.map((b) => {
			const v = sampleValue(b);
			return { tMs: b.tMs, min: v, max: v };
		});
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** 凡例の観測の時刻。`YYYY/MM/DD HH:mm:ss`（ローカル時刻）、ミリ秒が 0 でなければ `.mmm` まで。 */
export function formatObservationTime(tMs: number): string {
	const d = new Date(tMs);
	const base = `${d.getFullYear()}/${pad2(d.getMonth() + 1)}/${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
	const ms = d.getMilliseconds();
	return ms === 0 ? base : `${base}.${String(ms).padStart(3, '0')}`;
}

/**
 * 'samples' の線・管理線の重ね描きを切る x の幅。有効範囲 `[startMs, endMs)` を x に直し、描画域 `[0, plotWidth]` に収める。
 * 線のために残した範囲の外の観測が、取得範囲の外に線を引かないようにする。有効範囲が無い・幅が無いときは null。
 */
export function effectiveClip(
	effective: Range | null,
	x: Scale,
	plotWidth: number
): { x: number; width: number } | null {
	if (!effective) return null;
	const x0 = Math.max(0, x(effective.startMs));
	const x1 = Math.min(plotWidth, x(effective.endMs));
	if (!Number.isFinite(x0) || !Number.isFinite(x1) || x1 <= x0) return null;
	return { x: x0, width: x1 - x0 };
}

/** 空きが線を切るほど大きいか。 */
function isGap(prevT: number, t: number, binMs: number): boolean {
	return fin(binMs) && binMs > 0 && t - prevT > binMs * SAMPLE_GAP_FACTOR;
}

/**
 * 観測点の線の path の `d`。点が無ければ空文字。`step` は前の値を次の観測まで保つ段差。
 * 1 点だけの切れ端は同じ点への `L` を足す（`stroke-linecap: round` で点として見える）。
 */
export function samplesPath(
	bins: readonly Bin[],
	binMs: number,
	x: Scale,
	y: Scale,
	options?: { step?: boolean }
): string {
	const parts: string[] = [];
	let segLen = 0;
	let segStart = '';
	let prevT: number | null = null;
	let prevY = 0;
	const end = () => {
		if (segLen === 1) parts.push(`L${segStart}`);
		segLen = 0;
		prevT = null;
	};
	for (const b of bins) {
		const v = sampleValue(b);
		const px = fin(b.tMs) ? x(b.tMs) : NaN;
		const py = v === null ? NaN : y(v);
		if (!Number.isFinite(px) || !Number.isFinite(py)) {
			end();
			continue;
		}
		if (prevT !== null && isGap(prevT, b.tMs, binMs)) end();
		const pt = `${r2(px)},${r2(py)}`;
		if (segLen === 0) {
			parts.push(`M${pt}`);
			segStart = pt;
		} else {
			if (options?.step) parts.push(`L${r2(px)},${r2(prevY)}`);
			parts.push(`L${pt}`);
		}
		segLen++;
		prevT = b.tMs;
		prevY = py;
	}
	end();
	return parts.join('');
}

/**
 * 観測点のマーカーの位置。有効範囲の中の観測だけ、実時刻の x に置く。間引かない
 * （丸めた座標が直前の点とまったく同じ点だけ省く）。描画域の外の点（点の半分の大きさを足した範囲）は省く。
 */
export function samplesMarkerPoints(
	bins: readonly Bin[],
	x: Scale,
	y: Scale,
	range: Range,
	size: number,
	plot?: { width: number; height: number }
): { x: number; y: number }[] {
	const h = size / 2;
	const out: { x: number; y: number }[] = [];
	for (const b of bins) {
		const v = sampleValue(b);
		if (v === null || !fin(b.tMs) || b.tMs < range.startMs || b.tMs >= range.endMs) continue;
		const px = r2(x(b.tMs));
		const py = r2(y(v));
		if (!Number.isFinite(px) || !Number.isFinite(py)) continue;
		if (plot && !(px >= -h && px <= plot.width + h && py >= -h && py <= plot.height + h)) continue;
		const last = out[out.length - 1];
		if (last && last.x === px && last.y === py) continue;
		out.push({ x: px, y: py });
	}
	return out;
}

/** `tMs` に最も近い観測（同じ距離なら左）。それが欠測、または距離が `toleranceMs` を超えるなら null。 */
export function sampleAt(bins: readonly Bin[], tMs: number, toleranceMs: number): Bin | null {
	if (!fin(tMs) || !fin(toleranceMs) || toleranceMs < 0 || bins.length === 0) return null;
	// tMs 以下の最後の観測の添字（無ければ -1）。
	let lo = 0;
	let hi = bins.length - 1;
	let found = -1;
	while (lo <= hi) {
		const mid = (lo + hi) >> 1;
		if (bins[mid].tMs <= tMs) {
			found = mid;
			lo = mid + 1;
		} else {
			hi = mid - 1;
		}
	}
	// 候補は両隣の観測だけ（tMs が有限のもの）。欠測を飛ばして先の観測に吸着しない。
	let left: Bin | null = null;
	for (let i = found; i >= 0; i--) {
		if (fin(bins[i].tMs)) {
			left = bins[i];
			break;
		}
	}
	let right: Bin | null = null;
	for (let i = found + 1; i < bins.length; i++) {
		if (fin(bins[i].tMs)) {
			right = bins[i];
			break;
		}
	}
	const dl = left ? tMs - left.tMs : Infinity;
	const dr = right ? right.tMs - tMs : Infinity;
	const best = dl <= dr ? left : right;
	if (!best || Math.min(dl, dr) > toleranceMs) return null;
	return sampleValue(best) !== null ? best : null;
}

/** 観測 1 つが値を持つ区間（有効範囲で切ったもの）。 */
export interface SampleInterval {
	s: number;
	e: number;
	value: number;
}

/**
 * 観測ごとの「値のある区間」。観測から次の観測まで（`binMs × 2` と有効範囲の端で打ち切る。次が無い観測は `binMs`）。
 * 値の無い観測は区間を持たない。`binMs` が正の有限でないときは次の観測まで（次が無ければ空）。
 */
export function sampleIntervals(
	bins: readonly Bin[],
	binMs: number,
	range: Range
): SampleInterval[] {
	const binOk = fin(binMs) && binMs > 0;
	const out: SampleInterval[] = [];
	for (let i = 0; i < bins.length; i++) {
		const b = bins[i];
		const v = sampleValue(b);
		if (v === null || !fin(b.tMs)) continue;
		let next: number | undefined;
		for (let j = i + 1; j < bins.length; j++) {
			if (fin(bins[j].tMs)) {
				next = bins[j].tMs;
				break;
			}
		}
		let e = next ?? (binOk ? b.tMs + binMs : b.tMs);
		if (binOk) e = Math.min(e, b.tMs + binMs * SAMPLE_GAP_FACTOR);
		const s = Math.max(b.tMs, range.startMs);
		e = Math.min(e, range.endMs);
		if (e > s) out.push({ s, e, value: v });
	}
	return out;
}
