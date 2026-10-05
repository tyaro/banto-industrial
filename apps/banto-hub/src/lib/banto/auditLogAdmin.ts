/**
 * `admin` 限定の監査ログ閲覧 API クライアント。chronogazer の同名ファイル
 * から複製し、Tauri 分岐・デモモード分岐を削除して HTTP 一択にした。
 *
 * `getAuditConfig`/`setAuditConfig`（`GET`/`PUT /api/audit-log/config`）は
 * このファイルには**持っていない**。複製した当時は banto-hub にそのルートが
 * 無かったため削除した。その後 P3-a（docs/banto-hub-remaining-plan.md）で
 * `apps/banto-hub/core/src/rest.rs` に admin 限定の `GET`/`PUT
 * /api/audit-log/config` が足されたが、この画面のクライアントには戻して
 * いない（保持ポリシーの表示・変更の UI が banto-hub にはまだ無い）。
 */
import {
	getAuthProvider,
	ProviderError,
	SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE,
	type ErrorBody,
	type ListParams,
	type SnapshotListFetcher,
	type SnapshotListResult
} from '@banto/admin-core';
import { runWithLimit } from './runWithLimit';
import { CSRF_HEADER } from './setup';

/** Mirrors `banto_hub_core::audit::AuditLogEntry`（wire は camelCase）。 */
export interface AuditLogEntry {
	id: number;
	ts: string;
	actorUsername: string | null;
	actorRole: string | null;
	action: string;
	resource: string;
	entityId: string | null;
	/** 保存されている生の JSON 文字列。表示時に必要に応じて JSON.parse する。 */
	detail: string | null;
	origin: string;
	result: string;
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
	signal?: AbortSignal;
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
 * Mirrors `banto_admin_services::audit::AuditLogList`（I3' で banto のものに
 * 置き換えた）: `ListResult` の `rows`/`totalCount` に、**この応答が使った
 * スナップショット境界** `asOfId`（空の表では `0`）と、保持期間の剪定の世代
 * `deletionEpoch` を足したもの。画面の `SnapshotListResource`（banto #248）は
 * `deletionEpoch` が変わったら世代を失効させる。
 */
export type AuditLogList = SnapshotListResult<AuditLogEntry>;

/**
 * 監査ログ 1 ブロックの読み取りに掛ける上限（#428。chronogazer の #410 と
 * 同じ値）。backend はローカルの SQLite に対する索引つきの `DELETE`（保持
 * 期間）と `SELECT` だけなので、これに当たるのは「遅い」ではなく「返って
 * こない」。
 *
 * banto の `SnapshotListResource` にも上限（`requestTimeoutMs`、既定 30 秒）が
 * あるが、その失敗の文言は英語の固定文なので、画面に出す日本語の文言を持つ
 * ためにこちらの上限（[`createAuditLogFetcher`]）を短く掛けている。
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
 * フィルタ/ソート/ページングつきの監査ログ読み取り（admin限定の閲覧画面用）。
 *
 * **`asOfId` = スナップショット境界**（#428、banto #248）。`null` を渡すと
 * サーバーが**その時点の最大 `id`** を境界にして、使った境界を
 * [`AuditLogList.asOfId`] で返す。画面の `SnapshotListResource` が 1 つの
 * 「世代」の最初の応答でそれを固定し、同じ世代の後続ブロックにすべて渡す。
 */
export async function listAuditLog(
	params: ListParams,
	asOfId: number | null = null,
	signal?: AbortSignal
): Promise<AuditLogList> {
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
 * 切り替え・`dispose()`）と上限のどちらでも要求を中断する。
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
