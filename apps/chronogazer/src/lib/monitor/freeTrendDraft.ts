/**
 * 自由トレンドの下書き・編集の純粋な判断（#1207・#1231 PR 8b2a、
 * docs/chronogazer/trend-design.md §3・§10 の 8b2a）。
 *
 * - 下書きは **sessionStorage**（タブを閉じるまで。再読み込み・画面の移動では残る。2026-11-20
 *   オーナー決定）に、対象ごと（既存は ID、新規は `new`）に 1 つ。ペンの属性（線種・線幅・
 *   マーカー・管理線・bit など）もそのまま持つ（編集欄は 8b2b。保存済みの属性を下書きと保存で
 *   落とさない）。
 * - 読み込みは**黙って捨てる**（壊れた JSON・形の違い・上限超え・型違いは `null`。例外を投げない）。
 *   ストレージのアクセスが投げても表示は続ける。
 */
import { MAX_FREE_TREND_NAME_CHARS, MAX_PENS_PER_FREE_TREND } from '../banto/freeTrendsAdmin';
import type { FreeTrend, FreeTrendInput, FreeTrendPenInput } from '../banto/freeTrendsAdmin';
import type { Pen } from '../banto/displayGroupsAdmin';
import { DEFAULT_TREND_WINDOW_SEC, isTrendWindowSec } from './trendLogic';

/** 下書きの対象。既存の自由トレンドは ID、新規は `'new'`。 */
export type DraftTarget = number | 'new';

/** 下書きの中身。`baseRevision` は編集を始めたときの保存の版（新規は `null`）。 */
export interface FreeTrendDraft {
	baseRevision: number | null;
	name: string;
	timeWindowSec: number;
	pens: FreeTrendPenInput[];
}

export const DRAFT_STORAGE_PREFIX = 'chronogazer.freeTrends.draft:';

export type DraftStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem' | 'key' | 'length'>;

export function draftStorageKey(target: DraftTarget): string {
	return `${DRAFT_STORAGE_PREFIX}${target}`;
}

/** 空の新規の下書き。 */
export function emptyDraft(): FreeTrendDraft {
	return { baseRevision: null, name: '', timeWindowSec: DEFAULT_TREND_WINDOW_SEC, pens: [] };
}

/** ペンを入力の形に写す（属性を落とさない。`FreeTrend` が持つ余分なキーは持ち込まない）。 */
export function penToInput(pen: Pen | FreeTrendPenInput): FreeTrendPenInput {
	return {
		tagId: pen.tagId,
		colorSlot: pen.colorSlot ?? null,
		label: pen.label ?? null,
		unit: pen.unit ?? null,
		decimals: pen.decimals ?? null,
		lineStyle: pen.lineStyle ?? null,
		lineWidth: pen.lineWidth ?? null,
		marker: pen.marker ?? null,
		markerSize: pen.markerSize ?? null,
		axis: pen.axis ?? null,
		yMin: pen.yMin ?? null,
		yMax: pen.yMax ?? null,
		limitLines: pen.limitLines ?? null,
		limitHh: pen.limitHh ?? null,
		limitH: pen.limitH ?? null,
		limitL: pen.limitL ?? null,
		limitLl: pen.limitLl ?? null,
		bitStyle: pen.bitStyle ?? null,
		bitBandHeight: pen.bitBandHeight ?? null
	};
}

/** 保存された自由トレンドから下書き（の元）を作る。 */
export function draftFromFreeTrend(trend: FreeTrend): FreeTrendDraft {
	return {
		baseRevision: trend.revision,
		name: trend.name,
		timeWindowSec: trend.timeWindowSec,
		pens: trend.pens.map(penToInput)
	};
}

/** 保存の入力へ変える。`expectedRevision` は更新のときだけ付ける（属性はそのまま送る）。 */
export function draftToInput(draft: FreeTrendDraft, withRevision: boolean): FreeTrendInput {
	const input: FreeTrendInput = {
		name: draft.name,
		timeWindowSec: draft.timeWindowSec,
		pens: draft.pens.map(penToInput)
	};
	if (withRevision && draft.baseRevision !== null) input.expectedRevision = draft.baseRevision;
	return input;
}

/** 版を除いた中身が同じか（下書きが元と変わらなければ持たない判断に使う）。 */
export function sameDraftContent(a: FreeTrendDraft, b: FreeTrendDraft): boolean {
	return (
		a.name === b.name &&
		a.timeWindowSec === b.timeWindowSec &&
		JSON.stringify(a.pens.map(penToInput)) === JSON.stringify(b.pens.map(penToInput))
	);
}

/**
 * 編集の結果を下書きとして持つか。元（保存された内容、新規は空）と同じなら `null`（持たない）。
 * 下書きを持つ = 画面の「下書き（未保存）」と、チャートが下書きのペンで描くこと。
 */
export function draftToKeep(next: FreeTrendDraft, baseline: FreeTrendDraft): FreeTrendDraft | null {
	return sameDraftContent(next, baseline) ? null : next;
}

// --- ストレージ -------------------------------------------------------------

/** sessionStorage はアクセスそのものが投げることがある。 */
export function sessionDraftStorage(): DraftStorage | undefined {
	try {
		return typeof sessionStorage === 'undefined' ? undefined : sessionStorage;
	} catch {
		return undefined;
	}
}

const LINE_STYLES = ['solid', 'dashed', 'dashDot', 'dashDotDot', 'none'];
const MARKERS = ['none', 'circle', 'square', 'triangle', 'diamond'];
const AXES = ['left', 'right'];
const BIT_STYLES = ['step', 'band'];

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

const isNullOr = (value: unknown, ok: (v: unknown) => boolean): boolean =>
	value === null || value === undefined || ok(value);
const isStr = (v: unknown): boolean => typeof v === 'string';
const isNum = (v: unknown): boolean => typeof v === 'number' && Number.isFinite(v);
const isInt = (v: unknown): boolean => typeof v === 'number' && Number.isInteger(v);
const isBool = (v: unknown): boolean => typeof v === 'boolean';
const oneOf =
	(set: readonly string[]) =>
	(v: unknown): boolean =>
		typeof v === 'string' && set.includes(v);

/** 形の確かめ。合わなければ `null`。キー無しは `null`（既定）と読む。 */
export function parsePenInput(raw: unknown): FreeTrendPenInput | null {
	if (!isRecord(raw)) return null;
	if (!isInt(raw.tagId) || (raw.tagId as number) <= 0) return null;
	const slot = raw.colorSlot;
	if (!isNullOr(slot, (v) => isInt(v) && (v as number) >= 1 && (v as number) <= 16)) return null;
	const checks: [string, (v: unknown) => boolean][] = [
		['label', isStr],
		['unit', isStr],
		['decimals', isInt],
		['lineStyle', oneOf(LINE_STYLES)],
		['lineWidth', isNum],
		['marker', oneOf(MARKERS)],
		['markerSize', isNum],
		['axis', oneOf(AXES)],
		['yMin', isNum],
		['yMax', isNum],
		['limitLines', isBool],
		['limitHh', isNum],
		['limitH', isNum],
		['limitL', isNum],
		['limitLl', isNum],
		['bitStyle', oneOf(BIT_STYLES)],
		['bitBandHeight', isInt]
	];
	for (const [key, ok] of checks) if (!isNullOr(raw[key], ok)) return null;
	return penToInput(raw as unknown as FreeTrendPenInput);
}

/** 下書きの形の確かめ。合わなければ `null`。 */
export function parseDraft(raw: unknown): FreeTrendDraft | null {
	if (!isRecord(raw)) return null;
	const base = raw.baseRevision;
	if (base !== null && !(isInt(base) && (base as number) >= 0)) return null;
	if (typeof raw.name !== 'string' || raw.name.length > MAX_FREE_TREND_NAME_CHARS * 4) return null;
	if (!isTrendWindowSec(raw.timeWindowSec)) return null;
	if (!Array.isArray(raw.pens) || raw.pens.length > MAX_PENS_PER_FREE_TREND) return null;
	const pens: FreeTrendPenInput[] = [];
	const seen = new Set<number>();
	for (const item of raw.pens) {
		const pen = parsePenInput(item);
		if (pen === null || seen.has(pen.tagId)) return null;
		seen.add(pen.tagId);
		pens.push(pen);
	}
	return {
		baseRevision: base as number | null,
		name: raw.name,
		timeWindowSec: raw.timeWindowSec,
		pens
	};
}

/** 下書きを読む。無い・壊れている・ストレージが使えないときは `null`（例外を投げない）。 */
export function loadDraft(
	storage: Pick<Storage, 'getItem'> | undefined,
	target: DraftTarget
): FreeTrendDraft | null {
	if (!storage) return null;
	try {
		const text = storage.getItem(draftStorageKey(target));
		if (text === null) return null;
		return parseDraft(JSON.parse(text));
	} catch {
		return null;
	}
}

/** 下書きを書く（ベストエフォート）。 */
export function saveDraft(
	storage: Pick<Storage, 'setItem'> | undefined,
	target: DraftTarget,
	draft: FreeTrendDraft
): void {
	if (!storage) return;
	try {
		storage.setItem(draftStorageKey(target), JSON.stringify(draft));
	} catch {
		// 容量超過・無効化。下書きが残らないだけで、編集は続ける。
	}
}

/** 下書きを捨てる（ベストエフォート）。 */
export function clearDraft(
	storage: Pick<Storage, 'removeItem'> | undefined,
	target: DraftTarget
): void {
	if (!storage) return;
	try {
		storage.removeItem(draftStorageKey(target));
	} catch {
		// 捨てられなくても表示は続ける。
	}
}

/**
 * ストレージにある下書きのキーのうち、捨てる対象の判定。一覧に無い ID と、形の違う
 * （数でも `new` でもない）接頭辞つきのキーを捨てる。`new` は ID の不在とは関係ないので残す。
 */
export function draftKeysToDiscard(
	keys: readonly string[],
	existingIds: ReadonlySet<number>
): string[] {
	return keys.filter((key) => {
		if (!key.startsWith(DRAFT_STORAGE_PREFIX)) return false;
		const rest = key.slice(DRAFT_STORAGE_PREFIX.length);
		if (rest === 'new') return false;
		return !(/^\d+$/.test(rest) && existingIds.has(Number(rest)));
	});
}

/** 一覧を読んだら、一覧に無い ID の下書きを捨てる（ベストエフォート）。 */
export function pruneDrafts(
	storage: DraftStorage | undefined,
	existingIds: readonly number[]
): void {
	if (!storage) return;
	try {
		const keys: string[] = [];
		for (let i = 0; i < storage.length; i += 1) {
			const key = storage.key(i);
			if (key !== null) keys.push(key);
		}
		for (const key of draftKeysToDiscard(keys, new Set(existingIds))) storage.removeItem(key);
	} catch {
		// 掃除は便利機能。失敗しても表示は続ける。
	}
}

// --- 編集（ペンの追加・削除・並べ替え・色） ---------------------------------

export function remainingPens(draft: Pick<FreeTrendDraft, 'pens'>): number {
	return Math.max(MAX_PENS_PER_FREE_TREND - draft.pens.length, 0);
}

/** このペンの行で選べない（ほかのペンが使っている）タグのキー（`tag:<id>`、`PenTagPicker` の `used`）。 */
export function usedTagKeys(
	draft: Pick<FreeTrendDraft, 'pens'>,
	index: number | null
): Set<string> {
	const used = new Set<string>();
	draft.pens.forEach((pen, i) => {
		if (i !== index) used.add(`tag:${pen.tagId}`);
	});
	return used;
}

/**
 * タグを末尾に足す。残りを超えるときは**何も足さない**（黙って一部だけ足さない）。すでに使っている
 * タグは足さない。足したペンは属性が既定。
 */
export function addPens(draft: FreeTrendDraft, tagIds: readonly number[]): FreeTrendDraft {
	const used = new Set(draft.pens.map((pen) => pen.tagId));
	const fresh = [...new Set(tagIds)].filter((id) => !used.has(id));
	if (fresh.length === 0 || fresh.length > remainingPens(draft)) return draft;
	const pens = [
		...draft.pens,
		...fresh.map((tagId) => penToInput({ tagId, colorSlot: null } as FreeTrendPenInput))
	];
	return { ...draft, pens };
}

export function removePenAt(draft: FreeTrendDraft, index: number): FreeTrendDraft {
	if (index < 0 || index >= draft.pens.length) return draft;
	return { ...draft, pens: draft.pens.filter((_, i) => i !== index) };
}

/** ペンの順番を 1 つ動かす（端なら何もしない）。 */
export function movePenAt(draft: FreeTrendDraft, index: number, delta: -1 | 1): FreeTrendDraft {
	const target = index + delta;
	if (index < 0 || index >= draft.pens.length || target < 0 || target >= draft.pens.length) {
		return draft;
	}
	const pens = [...draft.pens];
	[pens[index], pens[target]] = [pens[target], pens[index]];
	return { ...draft, pens };
}

/** 色の枠を決める（`null` = 既定）。 */
export function setPenColor(
	draft: FreeTrendDraft,
	index: number,
	colorSlot: number | null
): FreeTrendDraft {
	if (index < 0 || index >= draft.pens.length) return draft;
	return {
		...draft,
		pens: draft.pens.map((pen, i) => (i === index ? { ...pen, colorSlot } : pen))
	};
}

// --- 権限・入口 -------------------------------------------------------------

/** 新しく作れるか（画面の出し分け。サーバーも断る）。 */
export function canCreateFreeTrend(session: {
	authDisabled: boolean;
	publicViewer: boolean;
	localDesktop: boolean;
	/** 確かな ID（アカウントのログイン）を持つか。 */
	signedIn: boolean;
}): boolean {
	if (session.authDisabled || session.publicViewer) return false;
	return session.localDesktop || session.signedIn;
}

/** 新しく作るボタンの状態。上限なら押せず理由を返す。 */
export function createBlocker(args: {
	canCreate: boolean;
	count: number;
	max: number;
}): string | null {
	if (!args.canCreate) return null;
	return args.count >= args.max
		? `自由トレンドは ${args.max} 件まで保存できます。不要なものを削除してください。`
		: null;
}

// --- 保存 -------------------------------------------------------------------

/** 保存の完了後にすること。 */
export interface FreeTrendSaveFollowUp {
	/** 下書き（保存を始めた対象のもの）を捨てる。保存は済んでいるので、離れていても捨てる。 */
	clearDraft: boolean;
	/** 編集欄を閉じて、保存したものの `?id=` へ移る。 */
	navigate: boolean;
	/** 一覧を読み直す。 */
	reload: boolean;
}

/**
 * 保存の応答を待つ間に画面を離れた・別のものを選んだ場合に、後から来た完了が遷移や選択を
 * 上書きしないための判断（純関数。`afterFreeTrendDelete` と同じ考え方）。`startedFor` は保存を
 * 始めたときの編集の対象、`currentTarget` は完了時点で編集している対象（編集欄が閉じていれば `null`）。
 */
export function afterFreeTrendSave(args: {
	destroyed: boolean;
	startedFor: DraftTarget;
	currentTarget: DraftTarget | null;
}): FreeTrendSaveFollowUp {
	if (args.destroyed) return { clearDraft: true, navigate: false, reload: false };
	return {
		clearDraft: true,
		navigate: args.currentTarget === args.startedFor,
		reload: true
	};
}

/**
 * 保存の成功後に、保持している下書きを捨てるか。保存を始めた後に編集が続いた（同じ画面でも、
 * 離れて戻った別の画面でも）場合、いまの下書きには保存に含まれなかった変更があるので捨てない。
 * 捨てるのは、いまの下書きが送った中身（版も含めて）と同じときだけ。`current` が無い・壊れている
 * ときは捨てるものが無い。
 */
export function shouldDiscardSavedDraft(
	current: FreeTrendDraft | null,
	sent: FreeTrendDraft
): boolean {
	return (
		current !== null &&
		current.baseRevision === sent.baseRevision &&
		sameDraftContent(current, sent)
	);
}

/** 保存の失敗の後にすること。 */
export interface FreeTrendSaveFailureFollowUp {
	/** 失敗を編集欄（`conflict`・欄のエラー）に出す。違えば、どれの失敗かを示す通知だけにする。 */
	applyToEditor: boolean;
	/** 一覧を読み直す。 */
	reload: boolean;
}

/**
 * 保存の失敗を、保存を始めた編集欄だけに出すための判断（純関数）。待つ間に別のものを選んで
 * 編集欄を開いていたら、その欄に誤った競合・誤りを立てない。
 */
export function afterFreeTrendSaveFailure(args: {
	destroyed: boolean;
	startedFor: DraftTarget;
	currentTarget: DraftTarget | null;
}): FreeTrendSaveFailureFollowUp {
	if (args.destroyed) return { applyToEditor: false, reload: false };
	return { applyToEditor: args.currentTarget === args.startedFor, reload: true };
}

/** 保存のエラーを画面のどこに出すか。 */
export interface FreeTrendFieldErrors {
	name: string[];
	timeWindowSec: string[];
	/** ペン全体（数・重複）。 */
	pens: string[];
	/** ペンの行ごと（0 始まり）。 */
	penRows: Record<number, string[]>;
	/** どの欄にも出せないもの。 */
	other: string[];
}

export function emptyFreeTrendFieldErrors(): FreeTrendFieldErrors {
	return { name: [], timeWindowSec: [], pens: [], penRows: {}, other: [] };
}

/** サーバーの `field_errors` を欄ごとに振り分ける（`pens[i].<項目>`・`name`・`timeWindowSec`・`pens`）。 */
export function sortFreeTrendFieldErrors(
	fieldErrors: readonly { field: string; message: string }[]
): FreeTrendFieldErrors {
	const sorted = emptyFreeTrendFieldErrors();
	for (const fe of fieldErrors) {
		const pen = /^pens\[(\d+)\](\..+)?$/.exec(fe.field);
		if (pen) (sorted.penRows[Number(pen[1])] ??= []).push(fe.message);
		else if (fe.field === 'name') sorted.name.push(fe.message);
		else if (fe.field === 'timeWindowSec') sorted.timeWindowSec.push(fe.message);
		else if (fe.field === 'pens') sorted.pens.push(fe.message);
		else sorted.other.push(fe.message);
	}
	return sorted;
}

/** 保存の失敗の見せ方。 */
export type FreeTrendSaveFailure =
	| { kind: 'conflict' }
	| { kind: 'validation'; errors: FreeTrendFieldErrors }
	| { kind: 'other'; message: string };

/** 失敗を分類する（版の食い違い → 欄のエラー → その他）。 */
export function classifySaveFailure(
	err: unknown,
	isConflict: (err: unknown) => boolean,
	fallbackMessage: (err: unknown) => string
): FreeTrendSaveFailure {
	if (isConflict(err)) return { kind: 'conflict' };
	const body = (err as { body?: { kind?: unknown; field_errors?: unknown } } | null)?.body;
	if (body && body.kind === 'validation' && Array.isArray(body.field_errors)) {
		return {
			kind: 'validation',
			errors: sortFreeTrendFieldErrors(body.field_errors as { field: string; message: string }[])
		};
	}
	return { kind: 'other', message: fallbackMessage(err) };
}
