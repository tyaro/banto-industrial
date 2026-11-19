/**
 * #1198 PR2: 「表示グループ」ページのペン一覧とタグ選択に並べる、現在値と品質の
 * 表示の判断（純関数。`valueColumn.test.ts` が表で固定する）。
 *
 * 値は監視画面と同じ `GET /api/collect/values`（`valuesPoller.svelte.ts`）の表を
 * `tag:<id>` で引くだけで、書式・品質の文言は `penView`（`monitorLogic.ts`）と
 * `qualityLabel` をそのまま使う（新しい言い回しを増やさない）。
 *
 * - 登録済みのタグ（`tagId`）だけ引く。ChronoGazer に未登録の Hub のタグ（`hubTag`）は
 *   値を取りに行かず「未登録」の印を出す。
 * - 表にキーが無い（収集を始めた後に足した・まだ一度も読めていない）ときは「—」だけ。
 *   「未収集」などの状態表示は PR3 で行う。
 * - 取得できない理由（収集が動いていない・読めない）は行ごとに出さず、
 *   [`valuesNote`] で一か所に出す。
 */
import type { Tag } from '../banto/tagRegistryAdmin';
import { penView, NO_VALUE, valuesStaleNote, type PenState } from './monitorLogic';
import type { CurrentSampleView } from '../banto/collectAdmin';
import type { ValuesState } from './valuesPoller.svelte';

/** 「未登録」の印（値の列は「—」）。 */
export const UNREGISTERED_VALUE_LABEL = '未登録';

/** 収集が動いていないときの一文（列の近くに一度だけ出す）。 */
export const VALUES_NOT_RUNNING_NOTE = '収集が動いていないため、現在値は表示できません。';

/** 現在値のセル 1 つ分（値の文字列と品質の文言）。 */
export interface ValueCell {
	/** 値（単位つき）。出せなければ「—」。 */
	text: string;
	/** 品質の文言（`qualityLabel` 由来か「未登録」）。無ければ `null`。 */
	quality: string | null;
	/** 色分け用。`unregistered` = Hub にだけあるタグ。`none` = 品質を出さない。 */
	state: PenState | 'unregistered' | 'none';
}

const NO_THRESHOLDS = {
	thresholdLl: null,
	thresholdL: null,
	thresholdH: null,
	thresholdHh: null
};

const EMPTY_CELL: ValueCell = { text: NO_VALUE, quality: null, state: 'none' };

export type ValueTarget =
	/** 登録済みのタグ。 */
	| { kind: 'tag'; tagId: number }
	/** ChronoGazer に未登録の Hub のタグ。 */
	| { kind: 'hub' }
	/** タグ未選択・登録できない行。 */
	| { kind: 'none' };

/**
 * 現在値のセルを作る（純関数）。`values` が `null`（まだ・動いていない）なら「—」。
 * 行ごとの計算は Map の引き当てと `penView` 1 回だけ。
 */
export function valueCell(
	target: ValueTarget,
	tagsById: ReadonlyMap<
		number,
		Pick<Tag, 'name' | 'unit' | 'decimals'> & Partial<Pick<Tag, 'dataType'>>
	>,
	values: Readonly<Record<string, CurrentSampleView>> | null
): ValueCell {
	if (target.kind === 'hub')
		return { text: NO_VALUE, quality: UNREGISTERED_VALUE_LABEL, state: 'unregistered' };
	if (target.kind === 'none' || values === null) return EMPTY_CELL;
	const sample = values[`tag:${target.tagId}`];
	if (sample === undefined) return EMPTY_CELL;
	const found = tagsById.get(target.tagId);
	// このページはしきい値を判定しないので、しきい値は空で渡す（値の書式・単位だけ使う）。
	const tag = found && { ...found, ...NO_THRESHOLDS };
	const view = penView({ tagId: target.tagId, colorSlot: null }, 0, sample, tag);
	const text =
		view.state === 'good' && view.unit !== null && !view.isBit
			? `${view.display} ${view.unit}`
			: view.display;
	return { text, quality: view.stateLabel, state: view.state };
}

/**
 * 値の列に添える理由（一か所に出す）。出す必要が無ければ `null`。
 *
 * - `active` が偽（ポーリングしていない）・読み込み中で失敗も無い = `null`。
 * - 収集が動いていない = 一文。
 * - 連続失敗（`stale`）= 監視画面と同じ `valuesStaleNote`（最後に取得できた時刻つき）。
 */
export function valuesNote(
	active: boolean,
	state: Pick<ValuesState, 'phase' | 'lastOkAt'>,
	stale: boolean,
	timeLabel: (ms: number) => string
): string | null {
	if (!active) return null;
	if (stale) return valuesStaleNote(state.lastOkAt, timeLabel);
	if (state.phase === 'notRunning') return VALUES_NOT_RUNNING_NOTE;
	return null;
}
