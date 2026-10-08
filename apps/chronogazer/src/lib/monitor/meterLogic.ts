/**
 * バー・計器（R1-D の D-2）の判断をまとめた純関数（`meterLogic.test.ts` が表で
 * 固定する）。パネル（`BarPanel.svelte`・`GaugePanel.svelte`）はここで作った
 * [`MeterView`] を描くだけで、判断を書き写さない（docs/implementation-checklist.md
 * §5「判断は純関数に出して、状態の総当たりを表でテストする」）。値・品質・
 * しきい値の判定と文言は D-1 の `monitorLogic.ts`（`penView`・`thresholdLevel`）を
 * そのまま使い、ここでは足さない。
 *
 * ## レンジ（2026-10-08 オーナー決定 Q3、docs/r1-plan.md の R1-D）
 *
 * 1. タグの工学値レンジ（`engLo` / `engHi`。スケーリングの 4 項目は全部そろって
 *    いるか全部無いかのどちらか - `banto-tags` の `Scaling::from_parts`）。
 *    `engLo > engHi`（逆向きのスケーリング）も正しい設定なので、小さい方を下端に
 *    する。`engLo === engHi` はレンジにならないので次へ。
 * 2. 無ければしきい値の LL..HH（両方あって LL < HH のとき）。
 * 3. どちらも無ければ「レンジ未設定」。棒・計器は描かず、値の文字は出す
 *    （値そのものは読めているので隠さない）。タグ設定へのリンクを添える
 *    （閲覧公開ではリンクを出さない - パネル側で `tagsHref` が `null`）。
 *
 * タグの情報を読めていないときは「レンジ未設定」と言わない（設定が無いのか
 * 読めなかったのかを混ぜない。チェックリスト §5「エラーを空に潰さない」）。
 *
 * ## 色（Q5）
 *
 * しきい値の判定は D-1 の `thresholdLevel`（収集のしきい値イベントと同じ向き・
 * 優先順位）。色は HH/LL → danger、H/L → warning、範囲内・判定なし → primary。
 * パネルは**色と文字の両方**で出す（`PenView.levelLabel`）。
 *
 * ## 値が無いとき（#414 段階2）
 *
 * `PenView.value` が `null`（`bad` / `stale` / `invalid` / 未収集 / good で値なし）
 * なら棒を描かない（0 の位置に描かない）。計器は banto の `Gauge` に `null` を
 * 渡し、弧を描かずに「—」を出させる（banto v6.3.0）。理由の文言は D-1 と同じ。
 *
 * ## レンジ外
 *
 * スケーリングは工学値レンジに丸めない（`banto-tags` の `scaling.rs`）ので、値は
 * レンジの外に出うる。棒は端に丸めて描き、「レンジ上限超え / 下限未満」を文字で
 * 添える（端に張り付いた棒だけでは、ちょうど端の値と区別できない）。
 */
import { niceTicks, type GaugeThresholds } from '@banto/charts';
import type { Tag } from '../banto/tagRegistryAdmin';
import { formatValue, type PenView, type ThresholdLevel } from './monitorLogic';

// --- レンジ ----------------------------------------------------------------

type RangeFields = Pick<Tag, 'engLo' | 'engHi' | 'thresholdLl' | 'thresholdHh'>;

/** バー・計器のレンジ。 */
export type MeterRange =
	/** 決まった（`min < max`）。`source` はどこから取ったか。 */
	| { kind: 'ok'; min: number; max: number; source: 'eng' | 'thresholds' }
	/** タグに工学値レンジも LL..HH も無い。 */
	| { kind: 'unset' }
	/** タグの情報を読めていない（設定が無いのか分からない）。 */
	| { kind: 'noTag' };

export const RANGE_UNSET_MESSAGE = 'レンジ未設定（タグ設定で工学値レンジを入れてください）';
export const RANGE_NO_TAG_MESSAGE = 'タグの情報を読み込めないため、レンジを決められません';

function finite(n: number | null | undefined): n is number {
	return typeof n === 'number' && Number.isFinite(n);
}

/** レンジを決める（純関数、Q3）。工学値レンジ → LL..HH → 未設定。 */
export function resolveMeterRange(tag: RangeFields | undefined): MeterRange {
	if (!tag) return { kind: 'noTag' };
	const { engLo, engHi, thresholdLl: ll, thresholdHh: hh } = tag;
	if (finite(engLo) && finite(engHi) && engLo !== engHi) {
		return { kind: 'ok', min: Math.min(engLo, engHi), max: Math.max(engLo, engHi), source: 'eng' };
	}
	if (finite(ll) && finite(hh) && ll < hh) {
		return { kind: 'ok', min: ll, max: hh, source: 'thresholds' };
	}
	return { kind: 'unset' };
}

/** レンジが決まらないときの文言（決まっていれば `null`）。 */
export function rangeMessage(range: MeterRange): string | null {
	switch (range.kind) {
		case 'ok':
			return null;
		case 'unset':
			return RANGE_UNSET_MESSAGE;
		case 'noTag':
			return RANGE_NO_TAG_MESSAGE;
	}
}

// --- 色 --------------------------------------------------------------------

/** しきい値の色の段階。 */
export type LevelTone = 'danger' | 'warning' | 'normal';

/** しきい値の判定 → 色の段階（純関数）。HH/LL → danger、H/L → warning。 */
export function levelTone(level: ThresholdLevel): LevelTone {
	switch (level) {
		case 'HH':
		case 'LL':
			return 'danger';
		case 'H':
		case 'L':
			return 'warning';
		case 'normal':
		case 'none':
			return 'normal';
	}
}

/**
 * 色の段階 → テーマの CSS 変数。banto の `gaugeColorVar` と同じ変数を使う
 * （計器とバーで同じ色になる）。
 */
export function toneColorVar(tone: LevelTone): string {
	switch (tone) {
		case 'danger':
			return 'var(--banto-danger)';
		case 'warning':
			return 'var(--banto-warning)';
		case 'normal':
			return 'var(--banto-primary)';
	}
}

/**
 * double の 1 つ下の値（`x` より小さい最大の double）。`value <= nextDown(x)` は
 * `value < x` と同じ意味になる（banto の下側しきい値は `<=` しか書けないため）。
 */
function nextDown(x: number): number {
	if (!Number.isFinite(x)) return x;
	if (x === 0) return -Number.MIN_VALUE;
	const view = new DataView(new ArrayBuffer(8));
	view.setFloat64(0, x);
	const bits = view.getBigUint64(0);
	view.setBigUint64(0, x > 0 ? bits - 1n : bits + 1n);
	return view.getFloat64(0);
}

type ThresholdFields = Pick<Tag, 'thresholdH' | 'thresholdHh' | 'thresholdL' | 'thresholdLl'>;

/**
 * banto の `Gauge` に渡すしきい値（純関数）。H → `warning`、HH → `danger`、
 * L → `warningLow`、LL → `dangerLow`。
 *
 * banto の判定順は danger → dangerLow → warning → warningLow で、D-1 の
 * `thresholdLevel`（HH → H → LL → L。上側が下側より優先）と 1 点だけ食い違う:
 * `LL >= H`（タグの検証は `LL <= L <= H <= HH` の等号を許すので `LL === H` は
 * 起こる）で値が H 以上・LL 以下のとき、こちらは H（warning）、banto は danger。
 * そのときだけ `dangerLow` を「H 未満」（`nextDown(H)`）にして、計器の色を
 * 文字（`levelLabel`）と必ず一致させる（`meterLogic.test.ts` が banto の
 * `gaugeColorVar` と突き合わせて固定する）。
 */
export function gaugeThresholds(tag: ThresholdFields | undefined): GaugeThresholds {
	if (!tag) return {};
	const { thresholdH: h, thresholdHh: hh, thresholdL: l, thresholdLl: ll } = tag;
	const out: GaugeThresholds = {};
	if (finite(h)) out.warning = h;
	if (finite(hh)) out.danger = hh;
	if (finite(l)) out.warningLow = l;
	if (finite(ll)) out.dangerLow = finite(h) && ll >= h ? nextDown(h) : ll;
	return out;
}

// --- バーの形 ----------------------------------------------------------------

/** 棒の形（下端 = レンジの下限）。 */
export interface BarGeometry {
	/** 棒の高さの割合（0..1、レンジに丸めた後）。 */
	fill: number;
	/** レンジの外（`over` = 上限超え、`under` = 下限未満）。中なら `null`。 */
	out: 'over' | 'under' | null;
}

/**
 * 棒の形（純関数）。値が無い（`null`・非有限）なら `null` = 棒を描かない
 * （0 の位置に描かない）。レンジの外は端に丸め、`out` で知らせる。
 */
export function barGeometry(
	value: number | null,
	range: { min: number; max: number }
): BarGeometry | null {
	if (value === null || !Number.isFinite(value)) return null;
	const span = range.max - range.min;
	if (!(span > 0)) return null;
	if (value > range.max) return { fill: 1, out: 'over' };
	if (value < range.min) return { fill: 0, out: 'under' };
	return { fill: (value - range.min) / span, out: null };
}

export const OVER_RANGE_LABEL = 'レンジ上限超え';
export const UNDER_RANGE_LABEL = 'レンジ下限未満';

/** レンジ外の文言（中なら `null`）。 */
export function outOfRangeLabel(out: BarGeometry['out']): string | null {
	if (out === 'over') return OVER_RANGE_LABEL;
	if (out === 'under') return UNDER_RANGE_LABEL;
	return null;
}

/** 目盛 1 本（`position` は下端からの割合 0..1）。 */
export interface ScaleTick {
	value: number;
	position: number;
}

/**
 * 目盛（純関数）。レンジの両端と、その間のきりのよい値（banto の `niceTicks`）。
 * レンジの外の目盛は出さない。
 */
export function scaleTicks(range: { min: number; max: number }, count = 5): ScaleTick[] {
	const span = range.max - range.min;
	if (!(span > 0)) return [];
	const epsilon = span * 1e-9;
	const inner = niceTicks(range.min, range.max, count).filter(
		(v) => v > range.min + epsilon && v < range.max - epsilon
	);
	return [range.min, ...inner, range.max].map((value) => ({
		value,
		position: (value - range.min) / span
	}));
}

/** しきい値の印 1 本。 */
export interface ThresholdMark {
	level: 'HH' | 'H' | 'L' | 'LL';
	value: number;
	position: number;
	tone: LevelTone;
}

/** しきい値の印（純関数）。レンジの中（両端を含む）にあるものだけ、上から順に。 */
export function thresholdMarks(
	tag: ThresholdFields | undefined,
	range: { min: number; max: number }
): ThresholdMark[] {
	if (!tag) return [];
	const span = range.max - range.min;
	if (!(span > 0)) return [];
	const marks: ThresholdMark[] = [];
	for (const [level, value] of [
		['HH', tag.thresholdHh],
		['H', tag.thresholdH],
		['L', tag.thresholdL],
		['LL', tag.thresholdLl]
	] as const) {
		if (!finite(value) || value < range.min || value > range.max) continue;
		marks.push({
			level,
			value,
			position: (value - range.min) / span,
			tone: levelTone(level)
		});
	}
	return marks;
}

/**
 * 同じ位置に重なるしきい値の印をまとめたもの（バーの名前の欄に 1 つだけ出す）。
 * タグの検証は `LL <= L <= H <= HH` の**等号を許す**ので、`LL === L === H` の
 * ような設定は正しく、印を 1 本ずつ置くと名前が同じ場所に重なって読めない
 * （#535 のレビュー）。
 */
export interface ThresholdMarkGroup {
	/** まとめた段（下から: LL → L → H → HH）。 */
	levels: ThresholdMark['level'][];
	/** 名前の欄に出す文字（例: `LL/L/H`）。 */
	label: string;
	value: number;
	position: number;
	/** まとめた段のうち重い方（danger があれば danger）。 */
	tone: LevelTone;
}

const LEVEL_ORDER_BOTTOM_UP: readonly ThresholdMark['level'][] = ['LL', 'L', 'H', 'HH'];

/**
 * しきい値の印を値ごとにまとめる（純関数）。同じ値の印は 1 つにし、名前は
 * 下から（LL → L → H → HH）`/` でつなぐ。並びは上から（値の大きい順）。
 */
export function groupThresholdMarks(marks: readonly ThresholdMark[]): ThresholdMarkGroup[] {
	const groups: ThresholdMarkGroup[] = [];
	for (const mark of marks) {
		const found = groups.find((g) => g.value === mark.value);
		if (found) {
			found.levels.push(mark.level);
			if (mark.tone === 'danger') found.tone = 'danger';
		} else {
			groups.push({
				levels: [mark.level],
				label: '',
				value: mark.value,
				position: mark.position,
				tone: mark.tone
			});
		}
	}
	for (const g of groups) {
		g.levels.sort((a, b) => LEVEL_ORDER_BOTTOM_UP.indexOf(a) - LEVEL_ORDER_BOTTOM_UP.indexOf(b));
		g.label = g.levels.join('/');
	}
	return groups.sort((a, b) => b.value - a.value);
}

// --- ペン 1 本の表示（バー・計器） -------------------------------------------

/** バー・計器のペン 1 本の表示（パネルはこれを描くだけ）。 */
export interface MeterView extends PenView {
	range: MeterRange;
	/** レンジが決まらないときの文言（決まっていれば `null`）。 */
	rangeMessage: string | null;
	tone: LevelTone;
	/** 棒の形（レンジが決まっていて値があるときだけ）。 */
	bar: BarGeometry | null;
	/** レンジ外の文言（中・値なし・レンジ未定なら `null`）。 */
	outLabel: string | null;
	ticks: ScaleTick[];
	marks: ThresholdMark[];
	/** 名前の欄に出す印（同じ値の印をまとめたもの）。 */
	markGroups: ThresholdMarkGroup[];
	/**
	 * タグに設定されているしきい値（レンジの外も含む。上から HH → H → L → LL）。
	 * 支援技術向けの説明（`meterDescription`）に使う。
	 */
	thresholds: { level: ThresholdMark['level']; value: number }[];
	gaugeThresholds: GaugeThresholds;
	/** 目盛・計器の数値の小数桁（タグを読めていなければ `null` = そのまま出す）。 */
	decimals: number | null;
}

/**
 * バー・計器のペン 1 本の表示を作る（純関数）。値・品質・しきい値の判定は
 * D-1 の `penView` の結果（`pen`）をそのまま使う。
 */
export function meterView(
	pen: PenView,
	tag: (RangeFields & ThresholdFields & Pick<Tag, 'decimals'>) | undefined
): MeterView {
	const range = resolveMeterRange(tag);
	const bar = range.kind === 'ok' ? barGeometry(pen.value, range) : null;
	const marks = range.kind === 'ok' ? thresholdMarks(tag, range) : [];
	const thresholds: MeterView['thresholds'] = [];
	if (tag) {
		for (const [level, value] of [
			['HH', tag.thresholdHh],
			['H', tag.thresholdH],
			['L', tag.thresholdL],
			['LL', tag.thresholdLl]
		] as const) {
			if (finite(value)) thresholds.push({ level, value });
		}
	}
	return {
		...pen,
		range,
		rangeMessage: rangeMessage(range),
		tone: levelTone(pen.level),
		bar,
		outLabel: outOfRangeLabel(bar?.out ?? null),
		ticks: range.kind === 'ok' ? scaleTicks(range) : [],
		marks,
		markGroups: groupThresholdMarks(marks),
		thresholds,
		gaugeThresholds: gaugeThresholds(tag),
		decimals: tag ? tag.decimals : null
	};
}

/** グループの全ペンのバー・計器の表示（純関数）。 */
export function meterViews(pens: readonly PenView[], tags: readonly Tag[]): MeterView[] {
	return pens.map((pen) =>
		meterView(
			pen,
			tags.find((tag) => tag.id === pen.tagId)
		)
	);
}

// --- 支援技術向けの説明 ------------------------------------------------------

/**
 * バー・計器の目盛としきい値の説明（純関数、#535 のレビュー）。棒・目盛・印の
 * 図は `aria-hidden` にしてあり、そのままでは支援技術にレンジとしきい値が
 * 伝わらないので、同じ内容を文で添える（パネルが画面外の文字として置く）。
 * 例: 「レンジ 0〜100（工学値レンジ）。しきい値: HH 90、H 80、L 10、LL 5」。
 */
export function meterDescription(
	view: Pick<MeterView, 'range' | 'rangeMessage' | 'thresholds' | 'decimals' | 'unit'>
): string {
	const fmt = (n: number) =>
		(view.decimals === null ? String(n) : formatValue(n, view.decimals)) +
		(view.unit ? ` ${view.unit}` : '');
	const range =
		view.range.kind === 'ok'
			? `レンジ ${fmt(view.range.min)}〜${fmt(view.range.max)}（${
					view.range.source === 'eng' ? '工学値レンジ' : 'しきい値の LL〜HH'
				}）`
			: (view.rangeMessage ?? '');
	const thresholds =
		view.thresholds.length === 0
			? 'しきい値: なし'
			: `しきい値: ${view.thresholds.map((t) => `${t.level} ${fmt(t.value)}`).join('、')}`;
	return `${range}。${thresholds}`;
}
