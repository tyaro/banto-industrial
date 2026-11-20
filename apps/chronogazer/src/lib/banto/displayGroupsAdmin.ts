/**
 * 表示グループ（#393 = #524 の段階 1）の API クライアント。
 * `tagRegistryAdmin.ts` と同じ三環境分岐:
 *
 * - Tauri webview -> `invoke()` の `display_groups_*` コマンド
 *   （`apps/chronogazer/src-tauri/src/lib.rs`）。
 * - LAN ブラウザ（組み込みサーバー） -> `/api/display-groups[...]`
 *   （`apps/chronogazer/core/src/rest/display_groups.rs`）。
 * - 単体ブラウザのデモモード -> DB が無いので `DEMO_MODE_MESSAGE` で拒否。
 *
 * 読み取りは viewer 以上、書き込みは editor 以上（R0 §3.6）。両経路とも同じ
 * サービス・同じ検証（`chronogazer_core::display_groups::validate_display_group`）
 * を通り、同じ形で監査される。型は Rust の `DisplayGroup`/`DisplayGroupPayload`
 * と 1:1（camelCase）。**しきい値は持たない** - タグ定義の属性で、画面は
 * タグの値を読み取り専用で見せるだけ（recorder-requirements.md §3.7.1）。
 */
import type { HubTagRow, Tag } from './tagRegistryAdmin';
import {
	demoModeError,
	httpRequest,
	invokeCommand,
	isTagRegistryAvailable
} from './tagRegistryAdmin';
import { getBantoMode } from './setup';

/** 表示種別（§3.2 の 4 種固定）。 */
export type DisplayKind = 'trend' | 'digital' | 'bar' | 'gauge';

/** 種別ごとの表示属性（閉じた集合。今はトレンドの既定時間窓だけ）。 */
export interface DisplayAttributes {
	/** トレンドの既定時間窓（秒）。トレンドだけ。 */
	timeWindowSec?: number;
}

export type PenLineStyle = 'solid' | 'dashed' | 'dashDot' | 'dashDotDot' | 'none';
export type PenMarker = 'none' | 'circle' | 'square' | 'triangle' | 'diamond';
export type PenAxis = 'left' | 'right';

/** `lineWidth` が `null` のときの線幅（px）。Rust の `DEFAULT_LINE_WIDTH` と同じ値。 */
export const DEFAULT_LINE_WIDTH = 1.5;
/** `markerSize` が `null` のときの大きさ（px）。Rust の `DEFAULT_MARKER_SIZE` と同じ値。 */
export const DEFAULT_MARKER_SIZE = 4;

/** bit の表し方。`null` = 段差（`step`）。帯は描画域の下から自動で積む（縦の位置は持たない）。 */
export type BitStyle = 'step' | 'band';

/** `bitBandHeight` が `null` のときの帯の高さ（px）。Rust の `DEFAULT_BIT_BAND_HEIGHT` と同じ値。 */
export const DEFAULT_BIT_BAND_HEIGHT = 16;

/**
 * ペンの属性（#1207・#1231 PR 6a。Mirrors the attribute fields of `Pen`）。すべて任意で、
 * `null` = 既定（`label`・`unit`・`decimals` はタグの設定、線は実線 `DEFAULT_LINE_WIDTH` px、
 * マーカーなし、縦軸は左・工学値レンジ）。
 */
export interface PenStyle {
	label: string | null;
	unit: string | null;
	decimals: number | null;
	lineStyle: PenLineStyle | null;
	lineWidth: number | null;
	marker: PenMarker | null;
	markerSize: number | null;
	axis: PenAxis | null;
	yMin: number | null;
	yMax: number | null;
	/**
	 * 管理線（#1207・#1231 PR 7a）。`limitLines` が `true` のとき HH・H・L・LL の線を引く
	 * （`null`・`false` = 引かない）。`limitHh`・`limitH`・`limitL`・`limitLl` は表示用の上書きで、
	 * `null` = タグのしきい値を使う。値は表示だけに使い、しきい値のイベントは変えない。
	 * 設定した値どうしは `LL <= L <= H <= HH`。`limitLines` が無効でも保存する。
	 */
	limitLines: boolean | null;
	limitHh: number | null;
	limitH: number | null;
	limitL: number | null;
	limitLl: number | null;
	/**
	 * bit の表し（#1207・#1231 PR 7c）。`bitStyle` は `null` = 段差、`bitBandHeight` は整数 8〜32（px）、
	 * `null` = 16。種別・タグの型を問わず保存し、効くのはトレンドの bit のタグだけ。
	 */
	bitStyle: BitStyle | null;
	bitBandHeight: number | null;
}

/** 全部が既定のペンの属性。 */
export const EMPTY_PEN_STYLE: Readonly<PenStyle> = {
	label: null,
	unit: null,
	decimals: null,
	lineStyle: null,
	lineWidth: null,
	marker: null,
	markerSize: null,
	axis: null,
	yMin: null,
	yMax: null,
	limitLines: null,
	limitHh: null,
	limitH: null,
	limitL: null,
	limitLl: null,
	bitStyle: null,
	bitBandHeight: null
};

export interface Pen extends PenStyle {
	tagId: number;
	/** banto チャートの系列色の枠（1..16）。`null` = 既定（ペンの位置 + 1）。 */
	colorSlot: number | null;
}

/**
 * 保存で送るペン（Mirrors `PenPayload`、#1179 PR5）。`tagId`（登録済みの記録項目）か
 * `hubTag`（Hub のカタログの行。保存で記録項目に登録される）の**どちらか一つ**。
 */
export type PenInput = (
	{ tagId: number; colorSlot: number | null } | { hubTag: HubTagRow; colorSlot: number | null }
) &
	PenStyle;

/** `hubTag` のペンの登録先（Mirrors `HubTagGroupPayload`）。どちらか一つ。 */
export type HubTagGroupInput =
	{ collectionGroupId: number } | { newGroup: { name: string; periodMs: number } };

/** Mirrors `chronogazer_core::display_groups::DisplayGroup`. */
export interface DisplayGroup {
	id: number;
	name: string;
	/** 束の名前（`null` = 束なし）。同じ名前のグループが 1 つの束になる。 */
	bundle: string | null;
	sortOrder: number;
	kind: DisplayKind;
	attributes: DisplayAttributes;
	pens: Pen[];
	revision: number;
}

/**
 * 作成・更新の応答（Mirrors `DisplayGroupSaveResponse`）。`registeredTags` は今回 Hub の
 * タグから記録項目に登録したときだけ付く。`pens` は全部 `tagId`。
 */
export type DisplayGroupSaveResponse = DisplayGroup & { registeredTags?: Tag[] };

/** Mirrors `chronogazer_core::display_groups::DisplayGroupPayload`. */
export interface DisplayGroupInput {
	name: string;
	/**
	 * 束の名前（50 文字以内）。空は `null`（束なし）。**更新では必須**（キーが無いと拒否される。
	 * 外すときは `null`）。「束なし」は束の名前に使えない（core の `NO_BUNDLE_LABEL`）。
	 */
	bundle: string | null;
	/** 作成のときだけ使う（省略で末尾）。**更新では無視される**（並びは `reorderDisplayGroups` だけで変える）。この画面は送らない。 */
	sortOrder?: number;
	kind: DisplayKind;
	attributes: DisplayAttributes;
	pens: PenInput[];
	/** `hubTag` のペンがあるときだけ付ける登録先。 */
	hubTagGroup?: HubTagGroupInput;
	/** 楽観ロック（更新のみ）。編集を始めたときの `revision`。 */
	expectedRevision?: number;
}

export async function listDisplayGroups(): Promise<DisplayGroup[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<DisplayGroup[]>('display_groups_list');
	return httpRequest<DisplayGroup[]>('/api/display-groups', { method: 'GET' });
}

export async function createDisplayGroup(
	input: DisplayGroupInput
): Promise<DisplayGroupSaveResponse> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<DisplayGroupSaveResponse>('display_groups_create', { input });
	}
	return httpRequest<DisplayGroupSaveResponse>('/api/display-groups', {
		method: 'POST',
		body: input
	});
}

export async function updateDisplayGroup(
	id: number,
	input: DisplayGroupInput
): Promise<DisplayGroupSaveResponse> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<DisplayGroupSaveResponse>('display_groups_update', { id, input });
	}
	return httpRequest<DisplayGroupSaveResponse>(`/api/display-groups/${id}`, {
		method: 'PUT',
		body: input
	});
}

export async function deleteDisplayGroup(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('display_groups_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/display-groups/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

/** 並べ替え: 全グループの ID を新しい順で 1 回ずつ。 */
export async function reorderDisplayGroups(ids: number[]): Promise<DisplayGroup[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<DisplayGroup[]>('display_groups_reorder', { ids });
	}
	return httpRequest<DisplayGroup[]>('/api/display-groups/order', {
		method: 'PUT',
		body: { ids }
	});
}
