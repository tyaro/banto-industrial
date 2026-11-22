/**
 * 模擬のタグの値（設計 docs/demo-pages-design.md §3.3）。タグ ID と時刻だけから決まる純関数で、
 * 状態を持たない（同じ入力なら必ず同じ値。履歴を時刻で引き直せる）。
 *
 * - 数値: タグ ID ごとの中心・振れ幅・周期のゆっくりした正弦波に、同じ秒なら同じ小さなゆらぎ。
 *   整数の型は丸める。値は更新の周期（`periodMs`）の区切りで変わる。
 * - bit: 周期（2〜5 秒の間でタグごとに違う）で 0・1 を切り替える。
 * - 文字列: 値の欄は数値なので `null`（本物の Hub の `/api/v1/stream` も数値だけを流す）。
 */

export interface DemoTagValueInput {
	id: number;
	/** `bit`・`i16`・`u16`・`i32`・`u32`・`f32`・`i64`・`u64`・`f64`・`string`。 */
	dataType: string;
	/** 小数の桁（既定 2。整数の型には使わない）。 */
	decimals?: number;
	/** 更新の周期 ms（収集グループの周期。下限 500、既定 1000）。 */
	periodMs?: number;
}

/** 更新の周期の下限（設計 §3.3）。 */
export const DEMO_MIN_PERIOD_MS = 500;

const INTEGER_TYPES = new Set(['i16', 'u16', 'i32', 'u32', 'i64', 'u64']);

/** 整数のハッシュ（xorshift 系）。[0, 1) の値にして返す。 */
export function demoHash01(a: number, b: number): number {
	let h = (Math.imul(a | 0, 0x9e3779b1) ^ Math.imul(b | 0, 0x85ebca6b)) >>> 0;
	h = Math.imul(h ^ (h >>> 15), 0x2c1b3c6d) >>> 0;
	h = Math.imul(h ^ (h >>> 12), 0x297a2d39) >>> 0;
	h = (h ^ (h >>> 15)) >>> 0;
	return h / 0x100000000;
}

/** 周期の下限を当てた、実際の更新の周期。 */
export function effectivePeriodMs(periodMs: number | undefined): number {
	const p = periodMs ?? 1000;
	return Number.isFinite(p) ? Math.max(DEMO_MIN_PERIOD_MS, Math.floor(p)) : 1000;
}

/** 時刻 `tMs` を含む更新の区切りの始まり（値の時刻 `t` に使う）。 */
export function demoSampleTime(tMs: number, periodMs: number | undefined): number {
	const p = effectivePeriodMs(periodMs);
	return Math.floor(tMs / p) * p;
}

/** 値が数値で出るか（文字列は `null`）。 */
export function demoValueIsNumeric(dataType: string): boolean {
	return dataType !== 'string';
}

/** `tMs` の時点のタグの値。文字列は `null`。 */
export function demoTagValue(input: DemoTagValueInput, tMs: number): number | null {
	if (!demoValueIsNumeric(input.dataType)) return null;
	const sampleMs = demoSampleTime(tMs, input.periodMs);
	if (input.dataType === 'bit') {
		const flipMs = 2000 + (Math.abs(input.id) % 4) * 1000;
		const offset = Math.floor(demoHash01(input.id, 7) * 1000);
		return Math.floor((sampleMs + offset) / flipMs) % 2;
	}
	const center = 40 + (Math.abs(input.id) % 7) * 10;
	const amplitude = 8 + (Math.abs(input.id) % 5) * 4;
	const periodS = 45 + (Math.abs(input.id) % 11) * 6;
	const phase = demoHash01(input.id, 13) * Math.PI * 2;
	const wave = Math.sin((sampleMs / 1000 / periodS) * Math.PI * 2 + phase);
	const jitter = (demoHash01(input.id, Math.floor(sampleMs / 1000)) - 0.5) * 0.1 * amplitude;
	const raw = center + amplitude * wave + jitter;
	if (INTEGER_TYPES.has(input.dataType)) return Math.round(raw);
	const decimals = Math.min(6, Math.max(0, input.decimals ?? 2));
	return Number(raw.toFixed(decimals));
}
