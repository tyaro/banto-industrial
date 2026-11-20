/**
 * `groups/+page.svelte`（表示グループ、#393）の判断を純関数に出したもの。
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
	DisplayKind,
	PenInput
} from '#lib/banto/displayGroupsAdmin.js';
import type { HubTagRow } from '#lib/banto/tagRegistryAdmin.js';
import {
	DEFAULT_HUB_GROUP_PERIOD_MS,
	registrationBlocker,
	type RegistrationTarget
} from '#lib/banto/hubRegistration.js';
import { REVISION_CONFLICT_FIELD } from '#lib/banto/revisionConflict.js';
import { penColorVar } from '@banto-industrial/trend';

/** グループ数の上限（trend-design.md §6。旧 16 → 36）。画面は作成ボタンを止めるだけで、拒否はサーバー。 */
export const MAX_DISPLAY_GROUPS = 36;
/** 1 グループのペンの上限（trend-design.md §6。旧 8 → 16）。 */
export const MAX_PENS = 16;
/** 束の名前の最大文字数（`chronogazer_core::display_groups::MAX_BUNDLE_CHARS`）。 */
export const MAX_BUNDLE_CHARS = 50;
/**
 * 色の枠の数。1〜8 は banto チャートの系列色（`--banto-chart-N`）、9〜16 は industrial の
 * テーマの `--bi-chart-N`（trend-design.md §9 Q4）。
 */
export const COLOR_SLOTS = 16;
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

/** Hub のタグの登録先を新しいグループにしたときの、名前の既定の提案（空にしない）。 */
export const DEFAULT_HUB_GROUP_NAME = 'Hub のタグ';

/**
 * ペン 1 行の下書き。**`tagId` と `hubTag` はどちらか一つ**（両方 `null` = まだタグを
 * 選んでいない行）。`hubTag` は Hub のカタログの行で、保存で記録項目に登録されて
 * ペンに割り当たる（#1179 PR5・PR6b）。
 */
export interface PenDraft {
	/** 登録済みの記録項目。 */
	tagId: number | null;
	/** まだ記録項目になっていない Hub のタグ（保存で登録）。 */
	hubTag: HubTagRow | null;
	/** `hubTag` の出どころ（Hub の接続先名・収集グループ名）。表示だけで、保存には使わない。 */
	hubSource?: { connection: string; group: string } | null;
	colorSlot: number | null;
}

export interface GroupDraft {
	name: string;
	/** 束の名前（任意。空は束なし）。 */
	bundle: string;
	kind: DisplayKind;
	/** トレンドのときだけ保存に使う（他の種別では送らない）。 */
	timeWindowSec: number;
	pens: PenDraft[];
	/**
	 * `hubTag` のペンの登録先。`hubTag` のペンを足すときに決まる（`null` = まだ無い）。
	 * 未保存の判定・保存の形に入るのは、`hubTag` のペンがあるときだけ。
	 */
	hubTarget: RegistrationTarget | null;
}

export function emptyDraft(): GroupDraft {
	return {
		name: '',
		bundle: '',
		kind: 'trend',
		timeWindowSec: DEFAULT_TIME_WINDOW_SEC,
		pens: [],
		hubTarget: null
	};
}

/** 登録先の既定: Hub のグループがあれば最初のもの、無ければ新しいグループ（名前は「Hub のタグ」）。 */
export function defaultHubTarget(hubGroups: readonly { id: number }[]): RegistrationTarget {
	return hubGroups.length > 0
		? { kind: 'existing', collectionGroupId: hubGroups[0].id }
		: { kind: 'new', name: DEFAULT_HUB_GROUP_NAME, periodMs: DEFAULT_HUB_GROUP_PERIOD_MS };
}

export function hasHubPens(draft: Pick<GroupDraft, 'pens'>): boolean {
	return draft.pens.some((pen) => pen.hubTag !== null);
}

/** ペンの重複判定のキー（`tag:<id>` / `hub:<外部名>`）。タグ未選択は `null`。 */
export function penKey(pen: Pick<PenDraft, 'tagId' | 'hubTag'>): string | null {
	if (pen.tagId !== null) return `tag:${pen.tagId}`;
	if (pen.hubTag !== null) return `hub:${pen.hubTag.externalName}`;
	return null;
}

export function draftFromGroup(group: DisplayGroup): GroupDraft {
	return {
		name: group.name,
		bundle: group.bundle ?? '',
		kind: group.kind,
		timeWindowSec: group.attributes.timeWindowSec ?? DEFAULT_TIME_WINDOW_SEC,
		pens: group.pens.map((pen) => ({ tagId: pen.tagId, hubTag: null, colorSlot: pen.colorSlot })),
		hubTarget: null
	};
}

/**
 * 下書きを保存の形にする。タグ未選択の行が残っているときは呼ばない
 * （`draftProblems` が先に止める）。`expectedRevision` は更新のときだけ。
 */
export function draftToInput(draft: GroupDraft, expectedRevision?: number): DisplayGroupInput {
	const input: DisplayGroupInput = {
		name: draft.name,
		bundle: draft.bundle.trim() === '' ? null : draft.bundle.trim(),
		kind: draft.kind,
		attributes: draft.kind === 'trend' ? { timeWindowSec: draft.timeWindowSec } : {},
		pens: draft.pens.map((pen): PenInput =>
			pen.hubTag !== null
				? { hubTag: pen.hubTag, colorSlot: pen.colorSlot }
				: { tagId: pen.tagId as number, colorSlot: pen.colorSlot }
		)
	};
	if (hasHubPens(draft) && draft.hubTarget !== null) {
		const target = draft.hubTarget;
		input.hubTagGroup =
			target.kind === 'existing'
				? { collectionGroupId: target.collectionGroupId as number }
				: { newGroup: { name: target.name.trim(), periodMs: target.periodMs } };
	}
	if (expectedRevision !== undefined) input.expectedRevision = expectedRevision;
	return input;
}

/** 未保存の判定用の正規形（トレンド以外では時間窓を比べない）。 */
function draftKey(draft: GroupDraft): string {
	return JSON.stringify({
		name: draft.name,
		bundle: draft.bundle.trim(),
		kind: draft.kind,
		timeWindowSec: draft.kind === 'trend' ? draft.timeWindowSec : null,
		pens: draft.pens.map((pen) => [penKey(pen), pen.colorSlot]),
		// 登録先は `hubTag` のペンがあるときだけ未保存の判定に入る。
		hubTarget: hasHubPens(draft) ? draft.hubTarget : null
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
		if (pen.tagId === null && pen.hubTag === null) {
			problems.push(`ペン ${index + 1} のタグを選んでください`);
		}
	});
	if (hasHubPens(draft)) {
		const blocker = registrationBlocker(
			1,
			draft.hubTarget ?? { kind: 'existing', collectionGroupId: null }
		);
		if (blocker !== null) problems.push(`Hub のタグの登録先: ${blocker}`);
	}
	return problems;
}

export function canAddPen(draft: GroupDraft): boolean {
	return draft.pens.length < MAX_PENS;
}

export function addPen(draft: GroupDraft): GroupDraft {
	if (!canAddPen(draft)) return draft;
	return { ...draft, pens: [...draft.pens, { tagId: null, hubTag: null, colorSlot: null }] };
}

/** あと何本ペンを足せるか。 */
export function remainingPens(draft: Pick<GroupDraft, 'pens'>): number {
	return Math.max(MAX_PENS - draft.pens.length, 0);
}

/**
 * タグ選択で選んだペンを末尾に足す。残りを超えるときは**何も足さない**（黙って一部だけ
 * 足さない。止める理由は画面が出す）。`hubTag` のペンを初めて足すとき、登録先が未決なら
 * `defaultTarget` にする。
 */
export function appendPens(
	draft: GroupDraft,
	picks: readonly PenDraft[],
	defaultTarget: RegistrationTarget
): GroupDraft {
	if (picks.length === 0 || picks.length > remainingPens(draft)) return draft;
	const needsTarget = picks.some((pen) => pen.hubTag !== null);
	return {
		...draft,
		pens: [...draft.pens, ...picks],
		hubTarget: needsTarget && draft.hubTarget === null ? defaultTarget : draft.hubTarget
	};
}

/** 1 本のペンのタグを置き換える（色は保つ）。 */
export function replacePenTag(
	draft: GroupDraft,
	index: number,
	pick: PenDraft,
	defaultTarget: RegistrationTarget
): GroupDraft {
	if (index < 0 || index >= draft.pens.length) return draft;
	const pens = draft.pens.map((pen, i) =>
		i === index ? { ...pick, colorSlot: pen.colorSlot } : pen
	);
	return {
		...draft,
		pens,
		hubTarget: pick.hubTag !== null && draft.hubTarget === null ? defaultTarget : draft.hubTarget
	};
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
	return penColorVar(slot);
}

/**
 * このペンの行で選べないタグ（同じグループの他のペンが使っている）。キーは
 * {@link penKey}（`tag:<id>` / `hub:<外部名>`）。`index` を省略すると全ペン。
 */
export function tagsUsedByOtherPens(draft: GroupDraft, index: number | null = null): Set<string> {
	const used = new Set<string>();
	draft.pens.forEach((pen, i) => {
		const key = penKey(pen);
		if (i !== index && key !== null) used.add(key);
	});
	return used;
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

/**
 * しきい値の読み取り専用の表示（LL / L / H / HH の順）。1 つも無ければ
 * 「しきい値なし」。編集は「記録」ページの「しきい値（記録計の設定）」で行う
 * （保存は記録計の側の設定の経路。§3.7.6）。
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

/** しきい値を読めなかったときのペンの表示。 */
export const THRESHOLDS_UNREADABLE = '読み込めませんでした';

/**
 * ペン 1 本のしきい値の表示（純関数、#532）。
 *
 * - タグ未選択・タグが見つからない → 「—」
 * - しきい値の一覧をまだ読めていない → 読み込み中は「—」、読めなかったら
 *   [`THRESHOLDS_UNREADABLE`]（「しきい値なし」と混ぜない - 読めなかったことを
 *   設定なしに潰さない）
 * - 一覧に無いタグは設定なし（既定）→ 「しきい値なし」
 */
export function penThresholdText(
	tagId: number | null,
	tagKnown: boolean,
	thresholds: ReadonlyMap<number, TagThresholds> | null,
	thresholdsFailed: boolean
): string {
	if (tagId === null || !tagKnown) return '—';
	if (thresholds === null) return thresholdsFailed ? THRESHOLDS_UNREADABLE : '—';
	return thresholdSummary(thresholds.get(tagId) ?? {});
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
	bundle: string[];
	kind: string[];
	timeWindowSec: string[];
	/** ペンの数など、ペンの一覧全体への誤り。 */
	pens: string[];
	/** `hubTag` のペンの登録先（`hubTagGroup.*`）への誤り。登録先の欄に出す。 */
	hubTagGroup: string[];
	/** ペンの行ごと（キーは行の番号、0 始まり）。 */
	penRows: Record<number, string[]>;
	/** 版の食い違い（`expectedRevision`）。 */
	revision: string[];
	/** どの欄にも出せないもの（トーストに出す。黙って捨てない）。 */
	other: string[];
}

const PEN_FIELD = /^pens\[(\d+)\]\.(tagId|colorSlot|hubTag(\..+)?)$/;
const HUB_TAG_GROUP_FIELD = /^hubTagGroup(\..+)?$/;

export function sortFieldErrors(fieldErrors: readonly FieldErrorLike[]): SortedFieldErrors {
	const sorted: SortedFieldErrors = {
		name: [],
		bundle: [],
		kind: [],
		timeWindowSec: [],
		pens: [],
		hubTagGroup: [],
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
		else if (fe.field === 'bundle') sorted.bundle.push(fe.message);
		else if (fe.field === 'kind') sorted.kind.push(fe.message);
		else if (fe.field === 'attributes.timeWindowSec') sorted.timeWindowSec.push(fe.message);
		else if (fe.field === 'pens') sorted.pens.push(fe.message);
		else if (HUB_TAG_GROUP_FIELD.test(fe.field)) sorted.hubTagGroup.push(fe.message);
		else if (fe.field === REVISION_CONFLICT_FIELD) sorted.revision.push(fe.message);
		else sorted.other.push(fe.message);
	}
	return sorted;
}

export function emptyFieldErrors(): SortedFieldErrors {
	return sortFieldErrors([]);
}

/**
 * 欄のエラーがあるタブ。中央は「設定」（名前・種別・時間窓）と「ペン」（ペン全体・各行）の
 * 2 タブなので、保存の失敗が今見ていないタブにあっても気づけるよう、どちらにあるかを返す。
 */
export function fieldErrorTabs(errors: SortedFieldErrors): { settings: boolean; pens: boolean } {
	return {
		settings:
			errors.name.length > 0 ||
			errors.bundle.length > 0 ||
			errors.kind.length > 0 ||
			errors.timeWindowSec.length > 0,
		pens:
			errors.pens.length > 0 ||
			errors.hubTagGroup.length > 0 ||
			Object.values(errors.penRows).some((rows) => rows.length > 0)
	};
}

// --- 版の食い違いのあとの「最新の内容を読み込む」（#393 オーナーレビュー P2-2・P2-5） ---

/**
 * 「最新の内容を読み込む」の結果:
 * - `open`: 読めた。`group` を開き直す（入力は破棄）。`groups` は一覧の最新。
 * - `deleted`: 読めたが、そのグループはもう無い。
 * - `failed`: 読めなかった。**古い行で開き直さず**、入力を残したまま理由を出す
 *   （P2-5: 以前は一覧の再読み込みの失敗を握り、最後に読めていた古い行で
 *   開き直して入力を捨てていた）。
 * - `stale`: 待っている間に選択・編集の世代が変わった。**何もしない**（P2-2:
 *   遅れて届いた応答が、別のグループで編集中の下書きを捨てないように）。
 */
export type LatestReload =
	| { kind: 'open'; group: DisplayGroup; groups: DisplayGroup[] }
	| { kind: 'deleted'; groups: DisplayGroup[] }
	| { kind: 'failed'; message: string }
	| { kind: 'stale' };

export interface LatestReloadOptions {
	/** 開き直すグループの ID。 */
	id: number;
	/** 一覧の取得（`listDisplayGroups`）。 */
	load: () => Promise<DisplayGroup[]>;
	/** 応答が届いた時点で、読み込みを始めたときの選択・編集がまだ続いているか。 */
	isCurrent: () => boolean;
	describeError: (err: unknown) => string;
}

export async function reloadLatestGroup(options: LatestReloadOptions): Promise<LatestReload> {
	let groups: DisplayGroup[];
	try {
		groups = await options.load();
	} catch (err) {
		if (!options.isCurrent()) return { kind: 'stale' };
		return { kind: 'failed', message: options.describeError(err) };
	}
	if (!options.isCurrent()) return { kind: 'stale' };
	const group = groups.find((entry) => entry.id === options.id);
	return group ? { kind: 'open', group, groups } : { kind: 'deleted', groups };
}
