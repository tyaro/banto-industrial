/**
 * 試運転モード/ロックダウン（設計 §5.6・2026-08-30 オーナー決定）の管理
 * REST クライアント（`GET /api/commissioning/status`・
 * `POST /api/commissioning/lock-down`、`apps/banto-hub/core/src/rest.rs` の
 * `commissioning_router`/`apps/banto-hub/core/src/commissioning.rs` 参照）。
 *
 * `usersAdmin.ts` と同じ規約（CSRF ヘッダ + Bearer 併用の `httpRequest`）
 * だが、この2ルートは他の admin エンドポイントと違う点が2つある:
 *
 * 1. `status` は**未認証でも読める**（バックエンド側で
 *    `require_auth_or_commissioning` を掛けていない - `commissioning_router`
 *    のdoc comment参照）。ただし CSRF ヘッダ（`X-Banto-Client`）は admin
 *    ルーター全体に掛かる `require_banto_client_header` の対象内なので、
 *    ここでは付ける必要がある。
 * 2. `(app)/+layout.ts` のルートガードはログイン判定より**前**にこれを
 *    叩く。まだ `AuthProvider` のトークンが存在しない/使えない前提の
 *    呼び出しなので、`fetchCommissioningStatusOrNull` は例外を握りつぶして
 *    `null`（＝取得失敗）を返す薄いラッパーとして用意する - 呼び出し側
 *    （ルートガード）が「取得に失敗したら安全側（ログイン必須）に倒す」
 *    という実装指示をこの1関数の戻り値だけで判断できるようにするため。
 */
import { getAuthProvider, ProviderError, type ErrorBody, type Identity } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/**
 * `GET /api/commissioning/status`/`POST /api/commissioning/lock-down` の
 * 応答（`crate::commissioning::CommissioningStatus`、`serde(rename_all =
 * "camelCase")` により wire は `{ lockedDown: boolean }`）。
 */
export interface CommissioningStatus {
	lockedDown: boolean;
}

/**
 * 試運転モード中に `crate::commissioning::synthetic_identity()` がサーバー
 * 側で使う合成 identity と**値を一致させた**クライアント側の定数。
 *
 * 注意（実装上ハマった点）: `createHttpAuthProvider().getIdentity()` は
 * ローカルに保存された bearer トークンが無いと `GET /api/auth/identity` を
 * 呼ぶことすらせず即座に `null` を返す（`@banto/admin-core` の
 * `providers/http.ts` 参照）。`/api/auth/*` は `banto_server` クレート側の
 * 別ルーターで、`commissioning` の認証バイパスは admin/tag-space 側の
 * ミドルウェア（`actor_identity`）にしか配線されていない - つまり試運転
 * モード中に「サーバーから合成 identity が返ってくる」経路は実在しない。
 * そのため `sessionStore`（`$lib/session.svelte.ts`）はこの定数をルート
 * ガード側でその場で設定する形にした。これは権限を勝手に底上げしている
 * わけではない: 試運転モード中はサーバー側がどのみち無条件に admin 相当
 * として全リクエストを受け付ける（`actor_identity`参照）ので、フロント
 * 側の RBAC 表示（`$lib/permissions.ts` の `isAdmin`/`canWriteResources`）
 * をサーバーの実際の挙動に合わせているだけである。
 */
export const COMMISSIONING_IDENTITY: Identity = {
	id: 'commissioning',
	name: '試運転モード',
	role: 'admin'
};

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

async function httpRequest<T>(path: string, method: 'GET' | 'POST'): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method, headers });
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

/** 現在の試運転モード/ロックダウン状態を取得する（未認証で呼べる）。 */
export async function getCommissioningStatus(): Promise<CommissioningStatus> {
	return httpRequest<CommissioningStatus>('/api/commissioning/status', 'GET');
}

/**
 * ルートガード（`(app)/+layout.ts`）専用: `getCommissioningStatus` の
 * 例外（ネットワーク断・非2xx・応答形状不正 等、原因を問わない全て）を
 * 握りつぶして `null` にする。**「取得に失敗した場合は安全側（ログインを
 * 要求する）に倒すこと」という実装指示を、ここで一度だけ具体化する**
 * - 呼び出し側は `null` を「ロックダウン済みと同様に扱う」だけでよく、
 * try/catch をルートガード側に重複させない。
 */
export async function fetchCommissioningStatusOrNull(): Promise<CommissioningStatus | null> {
	try {
		return await getCommissioningStatus();
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

/**
 * ルートガードの分岐判定（純関数、`(app)/+layout.ts` と vitest が共有）。
 *
 * 3分岐:
 * - `status` が取得できて `lockedDown: false`（試運転モード確定）→
 *   ログインを迂回してよい。
 * - `status` が取得できて `lockedDown: true`（ロックダウン済み確定）→
 *   通常どおりログイン必須。
 * - `status` が `null`（取得失敗）→ **安全側に倒し**通常どおりログイン
 *   必須（ロックダウン済みと同じ扱い）。通信エラー1つで認証が丸ごと
 *   外れる事態を避けるため、「わからない」は「ロックダウン済み」と
 *   同じ扱いにする（試運転モードだと誤認する方向には絶対に倒さない）。
 */
export function shouldBypassLoginForCommissioning(status: CommissioningStatus | null): boolean {
	return status !== null && !status.lockedDown;
}
