/**
 * データの受け渡しの契約: 取得した範囲と、ペンごとの `{ bins, binMs }` を必ず組で持つ。
 *
 * 区間の幅 `binMs` は API が返した実際の値（収集周期まで広がることがあり、ペンごとに違いうる）
 * を使い、要求の区間の数から推定しない。
 * 区間は `[tMs, tMs + binMs)` だが、`bins` は `tMs` の昇順で、各区間は次の区間の始まりで打ち切る
 * （収集周期が途中で変わり `binMs` より狭い間隔の点が並ぶことがあるため。`binEndMs`）。取得範囲 `range` も一緒に持つので、表示範囲が
 * 動いて読み直す間や失敗した後に、取得していない時刻へ線やカーソルの値を出さない
 * （`drawableSeries`・`cursorReadout` は取得範囲と表示範囲の重なりだけを使う）。
 */

import { visibleBins } from './geometry';
import type { Bin } from './pens';
import type { TimeRange } from './range';
import { samplesForDraw } from './samples';

/** ペン 1 本のデータ。`binMs` はその系列の実際の区間の幅（ミリ秒）。 */
export interface TrendSeries {
	bins: readonly Bin[];
	binMs: number;
	/**
	 * 系列の種類。未指定は 'bins'（区間の系列）。'samples' は観測点の系列（#1274）で、`bins` の各要素が観測 1 つ
	 * （`tMs` は観測の時刻そのもの、値は `min`）、`binMs` は記録周期。点を実時刻に置き、間引かず、
	 * カーソルは近い観測に吸着する（`core/samples.ts`）。
	 */
	kind?: 'bins' | 'samples';
}

/** 取得した範囲と、ペン id → 系列。 */
export interface TrendData {
	/** 取得した範囲。この外の時刻のデータは無いものとして扱う。 */
	range: TimeRange;
	series: Readonly<Record<string, TrendSeries | undefined>>;
}

/** 2 つの範囲 `[start, end)` の重なり。重ならなければ null。 */
export function intersectRange(a: TimeRange, b: TimeRange): TimeRange | null {
	const startMs = Math.max(a.startMs, b.startMs);
	const endMs = Math.min(a.endMs, b.endMs);
	return startMs < endMs ? { startMs, endMs } : null;
}

export interface DrawableSeries {
	/** 取得範囲と表示範囲の重なり。データが無い・重ならないときは null。 */
	effective: TimeRange | null;
	/**
	 * 有効範囲と重なる区間だけに絞った系列。'samples' は有効範囲の中の観測に、線を端までつなぐための
	 * 両側の外の観測を 1 つずつ足す（縦軸・空の判定は `samplesInRange` で外を除く）。`kind` は保つ。
	 */
	series: Record<string, TrendSeries>;
}

/**
 * 描いてよい区間。有効範囲 = 取得範囲 ∩ 表示範囲。各ペンの区間を、そのペンの `binMs` で
 * 有効範囲と重なるものだけに絞る（有効範囲が null なら空）。縦軸・空の判定・線・カーソルで共用する。
 */
export function drawableSeries(data: TrendData | null, view: TimeRange): DrawableSeries {
	const series: Record<string, TrendSeries> = {};
	if (!data) return { effective: null, series };
	const effective = intersectRange(data.range, view);
	for (const [id, s] of Object.entries(data.series)) {
		if (!s) continue;
		if (s.kind === 'samples') {
			series[id] = {
				binMs: s.binMs,
				kind: 'samples',
				bins: effective ? samplesForDraw(s.bins, effective) : []
			};
			continue;
		}
		series[id] = {
			binMs: s.binMs,
			bins: effective ? visibleBins(s.bins, s.binMs, effective) : []
		};
	}
	return { effective, series };
}
