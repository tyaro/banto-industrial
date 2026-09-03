/**
 * `admin` 限定の履歴（tstore）保持期間設定クライアント（T19 S2-d、
 * docs/banto-hub-t19-design.md §5.1、UX-39、新規作成）。
 * `apps/banto-hub/core/src/rest.rs` の `GET/PUT /api/store-settings`・
 * `POST /api/store-settings/prune-preview`・`POST /api/store-settings/
 * prune-now` に対応する - `mqttSettingsAdmin.ts`/`grpcSettingsAdmin.ts` と
 * 同じ httpRequest 雛形（camelCase の admin-UI リソース）。
 *
 * 2026-09-03 オーナー決定1（`apps/banto-hub/core/src/rest.rs`の
 * `store_settings_put`のdoc comment参照）: **`setStoreSettings`（PUT）は
 * 保持方針を保存するだけで、剪定（履歴ファイルの削除）は一切しない**。
 * 剪定は `pruneNow`（`POST prune-now`）だけが行う別の破壊的操作 -
 * `previewPrune`で件数を確認してから呼ぶこと（`+page.svelte`参照）。
 *
 * `retentionDays: null` は「無制限（剪定しない）」を意味する
 * （`crate::settings::StoreSettings::retention_days`のdoc comment参照、
 * UX-39 オーナー決定2）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `GET/PUT /api/store-settings`の応答/入力（同一形）。 */
export interface StoreSettings {
	/** `null` = 無制限（剪定しない）。それ以外は保持日数。 */
	retentionDays: number | null;
}

/** `POST /api/store-settings/prune-preview`の応答。実際には削除しない。 */
export interface PrunePreviewResult {
	wouldDeleteCount: number;
}

/** `POST /api/store-settings/prune-now`の応答。実際に削除した件数。 */
export interface PruneNowResult {
	deletedCount: number;
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

interface HttpInit {
	method: string;
	body?: unknown;
}

async function httpRequest<T>(path: string, init: HttpInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (hasBody) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: hasBody ? JSON.stringify(init.body) : undefined
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as T;
}

export async function getStoreSettings(): Promise<StoreSettings> {
	return httpRequest<StoreSettings>('/api/store-settings', { method: 'GET' });
}

/**
 * 保持方針を**保存するだけ**（このファイルの doc comment参照 - 剪定は
 * `pruneNow`だけが行う）。次回の24時間周期タスク/再起動から自然に反映
 * される。
 */
export async function setStoreSettings(input: StoreSettings): Promise<StoreSettings> {
	return httpRequest<StoreSettings>('/api/store-settings', { method: 'PUT', body: input });
}

/**
 * **保存済みの**保持方針で削除される見込みのファイル数を返す。実際には
 * 削除しない - 確認ダイアログの前段（`+page.svelte`参照）。
 */
export async function previewPrune(): Promise<PrunePreviewResult> {
	return httpRequest<PrunePreviewResult>('/api/store-settings/prune-preview', { method: 'POST' });
}

/**
 * **保存済みの**保持方針で今すぐ剪定する。不可逆（実際にファイルを削除する）
 * - 呼び出し前に必ず `previewPrune` + 確認ダイアログを挟むこと。
 */
export async function pruneNow(): Promise<PruneNowResult> {
	return httpRequest<PruneNowResult>('/api/store-settings/prune-now', { method: 'POST' });
}
