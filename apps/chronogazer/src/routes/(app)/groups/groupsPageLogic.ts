/**
 * `groups/+page.svelte`（グループ設定、#393）の判断を純関数に出したもの。
 * `tags/tagsPageLogic.ts` と同じ理由（Svelte 5 rune を使うページは、この
 * リポジトリの最小 vitest 構成では直接ロードできない）で、状態を持たない
 * 判断だけをここに置き、依存ゼロでテストする。
 *
 * 画面の編集中の値（下書き = `GroupDraft`）は、保存する形
 * （`DisplayGroupInput`）とは分けて持つ。ペンの行は「タグをまだ選んでいない」
 * （`tagId: null`）状態を取れるが、保存の形には出せないので、保存の前に
 * `draftProblems` で止める。**それ以外の検証はサーバー側の 1 関数だけ**が
 * 行う（`chronogazer_core::display_groups::validate_display_group`、
 * recorder-requirements.md §3.7.4）- 画面に同じ規則を書き写さず、上限や
 * 選択肢は「選べるものだけ出す」ことで守る。
 */
import type {
	DisplayGroup,
	DisplayGroupInput,
	DisplayKind
} from '#lib/banto/displayGroupsAdmin.js';

/** グループ数の上限（§3.2）。画面は作成ボタンを止めるだけで、拒否はサーバー。 */
export const MAX_DISPLAY_GROUPS = 16;
/** 1 グループのペンの上限（§2）。 */
export const MAX_PENS = 8;
/** banto チャートの系列色の枠の数（`@banto/charts` の `MAX_CHART_SERIES`）。 */
export const COLOR_SLOTS = 8;
/** トレンドの既定時間窓の既定値（秒、R1-D「既定窓 10 分」）。 */
export const DEFAULT_TIME_WINDOW_SEC = 600;

export const KIND_OPTIONS: readonly { value: DisplayKind; label: string }[] = [
	{ value: 'trend', label: 'トレンド' },
	{ value: 'digital', label: 'デジタル' },
	{ value: 'bar', label: 'バー' },
	{ value: 'gauge', label: '計器' }
];

/** Rust の `TREND_TIME_WINDOWS_SEC` と同じ選択肢（これ以外はサーバーが拒否する）。 */
export const TIME_WINDOW_OPTIONS: readonly { value: number; label: string }[] = [
	{ value: 60, label: '1 分' },
	{ value: 300, label: '5 分' },
	{ value: 600, label: '10 分' },
	{ value: 1800, label: '30 分' },
	{ value: 3600, label: '1 時間' }
];

export function kindLabel(kind: DisplayKind): string {
	return KIND_OPTIONS.find((option) => option.value === kind)?.label ?? kind;
}

export interface PenDraft {
	/** `null` = まだタグを選んでいない行。 */
	tagId: number | null;
	colorSlot: number | null;
}

export interface GroupDraft {
	name: string;
	kind: DisplayKind;
	/** トレンドのときだけ保存に使う（他の種別では送らない）。 */
	timeWindowSec: number;
	pens: PenDraft[];
}

export function emptyDraft(): GroupDraft {
	return { name: '', kind: 'trend', timeWindowSec: DEFAULT_TIME_WINDOW_SEC, pens: [] };
}

export function draftFromGroup(group: DisplayGroup): GroupDraft {
	return {
		name: group.name,
		kind: group.kind,
		timeWindowSec: group.attributes.timeWindowSec ?? DEFAULT_TIME_WINDOW_SEC,
		pens: group.pens.map((pen) => ({ tagId: pen.tagId, colorSlot: pen.colorSlot }))
	};
}

/**
 * 下書きを保存の形にする。タグ未選択の行が残っているときは呼ばない
 * （`draftProblems` が先に止める）。`expectedRevision` は更新のときだけ。
 */
export function draftToInput(draft: GroupDraft, expectedRevision?: number): DisplayGroupInput {
	const input: DisplayGroupInput = {
		name: draft.name,
		kind: draft.kind,
		attributes: draft.kind === 'trend' ? { timeWindowSec: draft.timeWindowSec } : {},
		pens: draft.pens.map((pen) => ({ tagId: pen.tagId as number, colorSlot: pen.colorSlot }))
	};
	if (expectedRevision !== undefined) input.expectedRevision = expectedRevision;
	return input;
}

/** 未保存の判定用の正規形（トレンド以外では時間窓を比べない）。 */
function draftKey(draft: GroupDraft): string {
	return JSON.stringify({
		name: draft.name,
		kind: draft.kind,
		timeWindowSec: draft.kind === 'trend' ? draft.timeWindowSec : null,
		pens: draft.pens.map((pen) => [pen.tagId, pen.colorSlot])
	});
}

/** 読み込み時（または保存時）の値と違う入力があるか。 */
export function isDraftDirty(draft: GroupDraft, baseline: GroupDraft): boolean {
	return draftKey(draft) !== draftKey(baseline);
}

/** 保存の前に画面で止める誤り（サーバーに送れない形のものだけ）。 */
export function draftProblems(draft: GroupDraft): string[] {
	const problems: string[] = [];
	draft.pens.forEach((pen, index) => {
		if (pen.tagId === null) problems.push(`ペン ${index + 1} のタグを選んでください`);
	});
	return problems;
}

export function canAddPen(draft: GroupDraft): boolean {
	return draft.pens.length < MAX_PENS;
}

export function addPen(draft: GroupDraft): GroupDraft {
	if (!canAddPen(draft)) return draft;
	return { ...draft, pens: [...draft.pens, { tagId: null, colorSlot: null }] };
}

export function removePen(draft: GroupDraft, index: number): GroupDraft {
	return { ...draft, pens: draft.pens.filter((_, i) => i !== index) };
}

/** ペンの順番を 1 つ動かす（端なら何もしない）。 */
export function movePen(draft: GroupDraft, index: number, delta: -1 | 1): GroupDraft {
	const target = index + delta;
	if (target < 0 || target >= draft.pens.length) return draft;
	const pens = [...draft.pens];
	[pens[index], pens[target]] = [pens[target], pens[index]];
	return { ...draft, pens };
}

/** 既定の色の枠（ペンの位置 + 1、`@banto/charts` の `seriesColorVar(index)` と同じ）。 */
export function defaultColorSlot(index: number): number {
	return Math.min(Math.max(index, 0) + 1, COLOR_SLOTS);
}

export function effectiveColorSlot(pen: { colorSlot: number | null }, index: number): number {
	return pen.colorSlot ?? defaultColorSlot(index);
}

/** 色の枠の CSS 変数（テーマに追従する）。 */
export function colorSlotVar(slot: number): string {
	return `var(--banto-chart-${slot})`;
}

/** このペンの行で選べないタグ（同じグループの他のペンが使っている）。 */
export function tagsUsedByOtherPens(draft: GroupDraft, index: number): Set<number> {
	const used = new Set<number>();
	draft.pens.forEach((pen, i) => {
		if (i !== index && pen.tagId !== null) used.add(pen.tagId);
	});
	return used;
}

/** タグのしきい値（タグ定義の属性。`/api/tags` が返す形の一部）。 */
export interface TagThresholds {
	thresholdLl?: number | null;
	thresholdL?: number | null;
	thresholdH?: number | null;
	thresholdHh?: number | null;
}

/**
 * しきい値の読み取り専用の表示（LL / L / H / HH の順）。1 つも無ければ
 * 「しきい値なし」。編集はタグ設定（タグ定義の経路）で行う。
 */
export function thresholdSummary(tag: TagThresholds | undefined): string {
	if (!tag) return '—';
	const parts: [string, number | null | undefined][] = [
		['LL', tag.thresholdLl],
		['L', tag.thresholdL],
		['H', tag.thresholdH],
		['HH', tag.thresholdHh]
	];
	if (parts.every(([, value]) => value === null || value === undefined)) return 'しきい値なし';
	return parts
		.map(([label, value]) => `${label} ${value === null || value === undefined ? '—' : value}`)
		.join(' / ');
}

/**
 * 一覧の中で 1 つ動かした後の ID の並び（`reorderDisplayGroups` に渡す）。
 * 端・見つからないときは `null`（送らない）。
 */
export function movedGroupIds(
	groups: readonly { id: number }[],
	id: number,
	delta: -1 | 1
): number[] | null {
	const index = groups.findIndex((group) => group.id === id);
	const target = index + delta;
	if (index < 0 || target < 0 || target >= groups.length) return null;
	const ids = groups.map((group) => group.id);
	[ids[index], ids[target]] = [ids[target], ids[index]];
	return ids;
}

export interface FieldErrorLike {
	field: string;
	message: string;
}

/** サーバーの `field_errors` を、画面のどこに出すかで振り分けたもの。 */
export interface SortedFieldErrors {
	name: string[];
	kind: string[];
	timeWindowSec: string[];
	/** ペンの数など、ペンの一覧全体への誤り。 */
	pens: string[];
	/** ペンの行ごと（キーは行の番号、0 始まり）。 */
	penRows: Record<number, string[]>;
	/** 版の食い違い（`expectedRevision`）。 */
	revision: string[];
	/** どの欄にも出せないもの（トーストに出す。黙って捨てない）。 */
	other: string[];
}

const PEN_FIELD = /^pens\[(\d+)\]\.(tagId|colorSlot)$/;

export function sortFieldErrors(fieldErrors: readonly FieldErrorLike[]): SortedFieldErrors {
	const sorted: SortedFieldErrors = {
		name: [],
		kind: [],
		timeWindowSec: [],
		pens: [],
		penRows: {},
		revision: [],
		other: []
	};
	for (const fe of fieldErrors) {
		const pen = PEN_FIELD.exec(fe.field);
		if (pen) {
			const row = Number(pen[1]);
			(sorted.penRows[row] ??= []).push(fe.message);
		} else if (fe.field === 'name') sorted.name.push(fe.message);
		else if (fe.field === 'kind') sorted.kind.push(fe.message);
		else if (fe.field === 'attributes.timeWindowSec') sorted.timeWindowSec.push(fe.message);
		else if (fe.field === 'pens') sorted.pens.push(fe.message);
		else if (fe.field === 'expectedRevision') sorted.revision.push(fe.message);
		else sorted.other.push(fe.message);
	}
	return sorted;
}

export function emptyFieldErrors(): SortedFieldErrors {
	return sortFieldErrors([]);
}
