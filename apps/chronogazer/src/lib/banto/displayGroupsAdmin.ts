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

export interface Pen {
	tagId: number;
	/** banto チャートの系列色の枠（1..8）。`null` = 既定（ペンの位置 + 1）。 */
	colorSlot: number | null;
}

/** Mirrors `chronogazer_core::display_groups::DisplayGroup`. */
export interface DisplayGroup {
	id: number;
	name: string;
	sortOrder: number;
	kind: DisplayKind;
	attributes: DisplayAttributes;
	pens: Pen[];
	revision: number;
}

/** Mirrors `chronogazer_core::display_groups::DisplayGroupPayload`. */
export interface DisplayGroupInput {
	name: string;
	/** 作成のときだけ使う（省略で末尾）。**更新では無視される**（並びは `reorderDisplayGroups` だけで変える）。この画面は送らない。 */
	sortOrder?: number;
	kind: DisplayKind;
	attributes: DisplayAttributes;
	pens: Pen[];
	/** 楽観ロック（更新のみ）。編集を始めたときの `revision`。 */
	expectedRevision?: number;
}

export async function listDisplayGroups(): Promise<DisplayGroup[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<DisplayGroup[]>('display_groups_list');
	return httpRequest<DisplayGroup[]>('/api/display-groups', { method: 'GET' });
}

export async function createDisplayGroup(input: DisplayGroupInput): Promise<DisplayGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<DisplayGroup>('display_groups_create', { input });
	}
	return httpRequest<DisplayGroup>('/api/display-groups', { method: 'POST', body: input });
}

export async function updateDisplayGroup(
	id: number,
	input: DisplayGroupInput
): Promise<DisplayGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<DisplayGroup>('display_groups_update', { id, input });
	}
	return httpRequest<DisplayGroup>(`/api/display-groups/${id}`, { method: 'PUT', body: input });
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
