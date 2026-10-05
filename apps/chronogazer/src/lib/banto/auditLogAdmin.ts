/**
 * Client for the `admin`-only audit-log API (spec M14, `docs/roadmap.md`).
 * Same Tauri/REST split as `usersAdmin.ts`: the Tauri webview calls
 * `invoke()` directly (the `audit_log_list`/`audit_config_get`/
 * `audit_config_apply` commands, `apps/admin-template/src-tauri/src/lib.rs`),
 * a LAN browser client served by the embedded server calls `fetch()` against
 * `/api/audit-log/*` (`apps/admin-template/core/src/rest.rs`), reusing the
 * same bearer-token/CSRF-header mechanism `@banto/admin-core`'s
 * `createHttpDataProvider`/`createHttpAuthProvider` use.
 *
 * Deliberately NOT built on `@banto/admin-core`'s generic
 * `DataProvider`/`getDataProvider()` - same reasoning as `usersAdmin.ts`:
 * this is a small, dedicated surface (one list read + one settings read/
 * write) with its own Tauri command names, not a `{resource}_list`-shaped
 * CRUD resource.
 *
 * Plain `vite dev`/`vite preview` (spec §11.1's third environment, no Rust
 * backend at all): there is no audit-log database to read, so every export
 * here throws/rejects with a `ProviderError` carrying `DEMO_MODE_MESSAGE`,
 * mirroring `usersAdmin.ts`'s `isUsersAdminAvailable()`/`demoModeError()`.
 */
import { invoke } from '@tauri-apps/api/core';
import {
	getAuthProvider,
	isProviderError,
	ProviderError,
	SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE,
	type ErrorBody,
	type ListParams,
	type SnapshotListFetcher,
	type SnapshotListResult
} from '@banto/admin-core';
import { runWithLimit } from './hubAdmin';
import { CSRF_HEADER, getBantoMode } from './setup';

/** Mirrors `admin_template_core::audit::AuditLogEntry` (camelCase on the wire). */
export interface AuditLogEntry {
	id: number;
	ts: string;
	actorUsername: string | null;
	actorRole: string | null;
	action: string;
	resource: string;
	entityId: string | null;
	/** Raw JSON-encoded summary string, as stored - `JSON.parse` on demand for display. */
	detail: string | null;
	origin: string;
	result: string;
}

/** Mirrors `admin_template_core::settings::AuditSettings` (camelCase on the wire). `null` on either field means unlimited on that dimension (spec M14: "0以下は無制限"). */
export interface AuditSettings {
	retentionDays: number | null;
	retentionRows: number | null;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is this environment backed by a real audit-log database (Tauri or the embedded server)? False in plain-browser demo mode. */
export function isAuditLogAvailable(): boolean {
	return getBantoMode() !== 'demo';
}

const ERROR_KINDS = new Set([
	'not_found',
	'bad_request',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

/** Same type guard as providers/tauri.ts / providers/http.ts / usersAdmin.ts (spec §10/§11.1). */
function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

interface HttpInit {
	method: string;
	body?: unknown;
	signal?: AbortSignal;
}

/** Same token lookup as usersAdmin.ts - see that file's doc comment. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
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
			body: hasBody ? JSON.stringify(init.body) : undefined,
			signal: init.signal
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

/**
 * Mirrors `banto_admin_services::audit::AuditLogList`（I2a で banto のものに
 * 置き換えた）: `ListResult` の `rows`/`totalCount` に、**この応答が使った
 * スナップショット境界** `asOfId`（空の表では `0`）と、保持期間の剪定の世代
 * `deletionEpoch` を足したもの。画面の `SnapshotListResource`（banto #248）は
 * `deletionEpoch` が変わったら世代を失効させる。
 */
export type AuditLogList = SnapshotListResult<AuditLogEntry>;

/**
 * 監査ログ 1 ブロックの読み取りに掛ける上限（#410）。backend はローカルの
 * SQLite に対する索引つきの `DELETE`（保持期間）と `SELECT` だけなので、
 * これに当たるのは「遅い」ではなく「返ってこない」。
 *
 * banto の `SnapshotListResource` にも上限（`requestTimeoutMs`、既定 30 秒）が
 * あるが、その失敗の文言は英語の固定文なので、画面に出す日本語の文言を持つ
 * ためにこちらの上限（[`createAuditLogFetcher`]）を短く掛けている。Tauri の
 * `invoke` は取り消せないので、こちらは待つのをやめるだけ（`hubAdmin.ts` の
 * `runWithLimit` と同じ非対称）。
 */
export const AUDIT_LIST_TIMEOUT_MS = 15000;

/** [`AUDIT_LIST_TIMEOUT_MS`] に当たったときの文言。 */
export function auditListTimeoutMessage(timeoutMs: number = AUDIT_LIST_TIMEOUT_MS): string {
	return `監査ログの読み取りが${Math.round(timeoutMs / 1000)}秒以内に返りませんでした。待つのをやめただけなので、「再読み込み」でもう一度試せます。`;
}

/** 要求した境界と違う境界の応答（サーバーが `asOfId` を無視したとき）の文言。 */
export const AUDIT_BOUNDARY_MISMATCH_MESSAGE =
	'監査ログの取得範囲がサーバー側で切り替わりました。「再読み込み」でもう一度読み込んでください。';

/** スナップショット失効（読み込みの途中で剪定が走った）の文言。 */
export const AUDIT_SNAPSHOT_EXPIRED_MESSAGE =
	'読み込みの途中で、保持ポリシーにより古い記録が削除されました。このまま続きを読むと行がずれるため、続きの読み込みを止めています。「再読み込み」で最新の状態から読み直してください。';

/**
 * 画面に出す失敗の文言。banto の `SnapshotListResource` が自分で記録する
 * 境界の食い違い（英語の固定文 `SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE`）だけを
 * 日本語に置き換え、それ以外（サーバーの `ErrorBody`・接続できない・
 * [`auditListTimeoutMessage`]）はそのまま出す。
 */
export function auditErrorText(error: ProviderError): string {
	return error.message === SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE
		? AUDIT_BOUNDARY_MISMATCH_MESSAGE
		: error.message;
}

/**
 * Filtered/sorted/paginated audit-log read (spec M14's admin-only viewer).
 *
 * **`asOfId` = スナップショット境界**（#410、banto #248）。`null` を渡すと
 * サーバーが**その時点の最大 `id`** を境界にして、使った境界を
 * [`AuditLogList.asOfId`] で返す。画面の `SnapshotListResource` が 1 つの
 * 「世代」の最初の応答でそれを固定し、同じ世代の後続ブロックにすべて渡す。
 * `signal` は REST でだけ効く（Tauri の `invoke` は取り消せない -
 * `hubAdmin.ts` と同じ非対称）。
 */
export async function listAuditLog(
	params: ListParams,
	asOfId: number | null = null,
	signal?: AbortSignal
): Promise<AuditLogList> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<AuditLogList>(
			'audit_log_list',
			asOfId === null ? { params } : { params, asOfId }
		);
	}
	const query = asOfId === null ? '' : `?asOfId=${encodeURIComponent(String(asOfId))}`;
	return httpRequest<AuditLogList>(`/api/audit-log/list${query}`, {
		method: 'POST',
		body: params,
		signal
	});
}

/** [`listAuditLog`] の形（テストで偽のサーバーに差し替える口）。 */
export type AuditLogLister = (
	params: ListParams,
	asOfId: number | null,
	signal?: AbortSignal
) => Promise<AuditLogList>;

/**
 * `createSnapshotListResource` に渡す取得 1 本（ブロック 1 つ）。要求の
 * 並べ替え・絞り込み・ページングと境界（`asOfId`）をそのまま
 * [`listAuditLog`] に渡し、[`AUDIT_LIST_TIMEOUT_MS`] の上限を掛ける
 * （上限に当たったら日本語の文言で失敗させる）。リソースの `signal`（世代の
 * 切り替え・`dispose()`）と上限のどちらでも要求を中断する（REST のみ）。
 */
export function createAuditLogFetcher(
	list: AuditLogLister = listAuditLog,
	timeoutMs: number = AUDIT_LIST_TIMEOUT_MS
): SnapshotListFetcher<AuditLogEntry> {
	return async (request, signal) => {
		const params: ListParams = {
			pagination: request.pagination,
			sort: request.sort,
			filters: request.filters
		};
		const outcome = await runWithLimit(
			(limitSignal) => list(params, request.asOfId, AbortSignal.any([signal, limitSignal])),
			timeoutMs
		);
		if (outcome.kind === 'ok') return outcome.value;
		if (outcome.kind === 'failed') throw outcome.error;
		throw new ProviderError({ kind: 'other', message: auditListTimeoutMessage(timeoutMs) });
	};
}

/** Current audit-log retention policy. Any authenticated role may call this (it only feeds a settings-screen display) - see `audit_config_get`'s Rust doc comment. */
export async function getAuditConfig(): Promise<AuditSettings> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<AuditSettings>('audit_config_get');
	return httpRequest<AuditSettings>('/api/audit-log/config', { method: 'GET' });
}

/** Persist a new retention policy. `admin`-only (rejected with a `forbidden` `ProviderError` otherwise). */
export async function setAuditConfig(config: AuditSettings): Promise<AuditSettings> {
	if (!isAuditLogAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<AuditSettings>('audit_config_apply', {
			retentionDays: config.retentionDays,
			retentionRows: config.retentionRows
		});
	}
	return httpRequest<AuditSettings>('/api/audit-log/config', { method: 'PUT', body: config });
}
