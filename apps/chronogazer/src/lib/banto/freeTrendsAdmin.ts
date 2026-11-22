/**
 * 自由トレンド（#1207・#1231 PR 8a、docs/chronogazer/trend-design.md §2・§3・§9）の API クライアント。
 * `displayGroupsAdmin.ts` と同じ三環境分岐:
 *
 * - Tauri webview -> `invoke()` の `free_trends_*` コマンド（`apps/chronogazer/src-tauri/src/lib.rs`）。
 * - LAN ブラウザ（組み込みサーバー） -> `/api/free-trends[...]`
 *   （`apps/chronogazer/core/src/rest/free_trends.rs`）。
 *
 * 自由トレンドは全員で共有する名前付きのペンの組。開くのは全員（ログインなしのセッションと
 * API キーも）、作れるのはアカウントのログイン（viewer 以上）とこの PC からの接続（localDesktop）、
 * 変えられる・消せるのは作った人と editor 以上（2026-11-20 オーナー決定）。画面は各件の `canEdit`
 * でボタンを出し分ける。型は Rust の `FreeTrend`・`FreeTrendPayload` と 1:1（camelCase）。
 * ペンの属性は表示グループと同じ語彙（`PenStyle`）。
 */
import type { Pen, PenStyle } from './displayGroupsAdmin';
import { httpRequest, invokeCommand } from './tagRegistryAdmin';
import { getBantoMode } from './setup';

/** 保存できる自由トレンドの数（Rust の `MAX_FREE_TRENDS`）。 */
export const MAX_FREE_TRENDS = 200;
/** 1 つの自由トレンドのペンの上限（Rust の `MAX_PENS_PER_FREE_TREND`）。 */
export const MAX_PENS_PER_FREE_TREND = 16;
/** 名前の上限（文字数。Rust の `free_trends::MAX_NAME_CHARS`）。 */
export const MAX_FREE_TREND_NAME_CHARS = 64;

/** Mirrors `chronogazer_core::free_trends::FreeTrend`. */
export interface FreeTrend {
	id: number;
	name: string;
	/** 時間窓（秒）。表示グループのトレンドの候補（60・300・600・1800・3600）から 1 つ。 */
	timeWindowSec: number;
	pens: Pen[];
	revision: number;
	/** 作った人（アカウント名、またはこの PC からの接続の `localDesktop`）。 */
	createdBy: string;
	/** 作った日時（UTC、`YYYY-MM-DD HH:MM:SS`）。 */
	createdAt: string;
	updatedBy: string;
	updatedAt: string;
	/** 今の主体がこの自由トレンドを変えられる・消せるか。 */
	canEdit: boolean;
}

/** 保存で送るペン（Mirrors `FreeTrendPenPayload`）。登録済みのタグ（`tagId`）だけ。 */
export type FreeTrendPenInput = { tagId: number; colorSlot: number | null } & PenStyle;

/** Mirrors `chronogazer_core::free_trends::FreeTrendPayload`. */
export interface FreeTrendInput {
	name: string;
	/** 省略・`null` は既定（600 秒）。 */
	timeWindowSec?: number | null;
	pens: FreeTrendPenInput[];
	/** 楽観ロック（更新のみ）。編集を始めたときの `revision`。 */
	expectedRevision?: number;
}

export async function listFreeTrends(): Promise<FreeTrend[]> {
	if (getBantoMode() === 'tauri') return invokeCommand<FreeTrend[]>('free_trends_list');
	return httpRequest<FreeTrend[]>('/api/free-trends', { method: 'GET' });
}

export async function getFreeTrend(id: number): Promise<FreeTrend> {
	if (getBantoMode() === 'tauri') return invokeCommand<FreeTrend>('free_trends_get', { id });
	return httpRequest<FreeTrend>(`/api/free-trends/${id}`, { method: 'GET' });
}

export async function createFreeTrend(input: FreeTrendInput): Promise<FreeTrend> {
	if (getBantoMode() === 'tauri') {
		return invokeCommand<FreeTrend>('free_trends_create', { input });
	}
	return httpRequest<FreeTrend>('/api/free-trends', { method: 'POST', body: input });
}

export async function updateFreeTrend(id: number, input: FreeTrendInput): Promise<FreeTrend> {
	if (getBantoMode() === 'tauri') {
		return invokeCommand<FreeTrend>('free_trends_update', { id, input });
	}
	return httpRequest<FreeTrend>(`/api/free-trends/${id}`, { method: 'PUT', body: input });
}

export async function deleteFreeTrend(id: number): Promise<void> {
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('free_trends_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/free-trends/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}
