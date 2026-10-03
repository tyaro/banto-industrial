/**
 * 試運転モード/ロックダウン（設計 §5.6・2026-08-30 オーナー決定）の管理
 * REST クライアント（`GET /api/commissioning/status`・
 * `POST /api/commissioning/lock-down`、`apps/banto-hub/core/src/rest.rs` の
 * `commissioning_router`/`apps/banto-hub/core/src/commissioning.rs` 参照）。
 *
 * banto v3.0.0（ADR-0017）から、試運転モードはサーバーが発行する grant
 * （`POST /api/auth/grant/commissioning`）で入る通常のセッションになった
 * （kind は `'commissioning'`）。ルートガード（`(app)/+layout.ts`）は
 * `grantFallback` で grant を取るだけで、この `status` は読まない。ここの
 * `status` を読むのはタグ画面の「即時反映」の表示だけ。
 *
 * `usersAdmin.ts` と同じ規約（CSRF ヘッダ + Bearer 併用の `httpRequest`）。
 * `status` は未認証でも読める（`commissioning_router` の doc 参照）が、
 * CSRF ヘッダ（`X-Banto-Client`）は admin ルーター全体に掛かるので付ける。
 * `lock-down` は admin の bearer（試運転の grant でも可）が要る。
 */
import {
	getAuthProvider,
	ProviderError,
	type ErrorBody,
	type SessionSnapshot
} from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/**
 * `GET /api/commissioning/status`/`POST /api/commissioning/lock-down` の
 * 応答（`crate::commissioning::CommissioningStatus`、`serde(rename_all =
 * "camelCase")` により wire は `{ lockedDown: boolean }`）。
 */
export interface CommissioningStatus {
	lockedDown: boolean;
}

/** 試運転の grant のセッションの kind（サーバーの `GET /api/auth/identity` の `kind`、grant の種別）。 */
export const COMMISSIONING_KIND = 'commissioning';

/** 試運転の grant のセッションが確定しているか（`active` かつ kind が `commissioning`）。 */
export function isCommissioningSession(snapshot: SessionSnapshot): boolean {
	return snapshot.status === 'active' && snapshot.kind === COMMISSIONING_KIND;
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

/**
 * `signal`（省略可）は `fetch` にそのまま渡す。中断すると要求そのもの
 * （本文の読み出しを含む）が止まり、`ProviderError` で reject する
 * （#445 の確認の時間切れ、PR #447 の再レビュー）。
 */
async function httpRequest<T>(
	path: string,
	method: 'GET' | 'POST',
	signal?: AbortSignal
): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method, headers, signal });
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
 * 現在の試運転モード/ロックダウン状態を取得する（未認証で呼べる）。
 * `signal` を渡すと、中断で要求そのものを止める（省略可）。
 */
export async function getCommissioningStatus(signal?: AbortSignal): Promise<CommissioningStatus> {
	return httpRequest<CommissioningStatus>('/api/commissioning/status', 'GET', signal);
}

/**
 * `getCommissioningStatus` の例外（ネットワーク断・非2xx・応答形状不正 等、
 * 原因を問わない全て）を握りつぶして `null`（＝取得失敗）にする。表示用の
 * 読み取りで、取得できなくても画面を止めたくない呼び出し側のための薄い
 * ラッパー（ルートガードは使わない。認証の判断は grant の経路）。
 */
export async function fetchCommissioningStatusOrNull(
	signal?: AbortSignal
): Promise<CommissioningStatus | null> {
	try {
		return await getCommissioningStatus(signal);
	} catch {
		return null;
	}
}

/**
 * 試運転モード → ロックダウン済みへの唯一の正方向遷移（設計 §5.6）。
 * admin アカウントが1件も無いとサーバーが `validation` エラーで拒否する
 * （`no_admin_account_error`、`apps/banto-hub/core/src/commissioning.rs`）
 * - 呼び出し側（設定画面）でそのエラーメッセージをそのまま表示する。
 *
 * **試運転モードを解除する方向のエンドポイントは存在しない**
 * （`banto-hub-elev.exe` 経由限定、REST 非公開）。このクライアントにも
 * 意図的に実装しない。
 */
export async function lockDown(): Promise<CommissioningStatus> {
	return httpRequest<CommissioningStatus>('/api/commissioning/lock-down', 'POST');
}
