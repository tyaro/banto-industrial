/**
 * `admin` 限定の API キー管理クライアント（#1161 PR4b、`apps/chronogazer/core/src/rest/api_keys.rs` の
 * `/api/api-keys*` に対応、docs/chronogazer-agent-api-design.md §3.4）。HTTP（LAN・サービス）専用。
 * 平文のキーは発行の応答（`IssuedApiKey.key`）にしか載らない（DB にはハッシュだけ）ので、呼び出し側は
 * その場で表示する。削除は失効済みのキーだけ（有効なキーは 409）。
 *
 * ChronoGazer のスコープは `read` と `config` だけ（`config` は `read` を含む、設計書 §3.3）。
 * 対象を絞る書式や、レート制限のトリップ（HUB の機能）は持たない。
 */
import { restJson } from './restJson';

export type ApiKeyScope = 'read' | 'config';

/** Mirrors `banto_api_keys::ApiKeySummary`（wire は camelCase）。ハッシュは返らない。 */
export interface ApiKeySummary {
	id: number;
	name: string;
	prefix: string;
	scopes: string[];
	createdAt: string;
	/** epoch ミリ秒。未使用は `null`。 */
	lastUsedAt: number | null;
	revokedAt: string | null;
	/** epoch ミリ秒。`null` = 無期限。 */
	expiresAt: number | null;
}

export interface CreateApiKeyInput {
	name: string;
	scopes: ApiKeyScope[];
	/** epoch ミリ秒。省略・`null` = 無期限。過去は 400。 */
	expiresAt?: number | null;
}

/** Mirrors REST の `IssuedApiKeyResponse` - `key` はこの応答でしか手に入らない平文全体。 */
export interface IssuedApiKey {
	id: number;
	name: string;
	prefix: string;
	scopes: string[];
	key: string;
}

export type ApiKeyState = 'active' | 'revoked' | 'expired';

/** 一覧の状態。失効が期限切れより優先（失効した後に期限が来ても「失効」と見せる）。 */
export function apiKeyState(key: ApiKeySummary, nowMs: number): ApiKeyState {
	if (key.revokedAt !== null) return 'revoked';
	if (key.expiresAt !== null && key.expiresAt <= nowMs) return 'expired';
	return 'active';
}

export function listApiKeys(): Promise<ApiKeySummary[]> {
	return restJson<ApiKeySummary[]>('/api/api-keys', { method: 'GET' });
}

/** 発行。応答の `key` は呼び出し限りで、画面はその場で見せる。 */
export function createApiKey(input: CreateApiKeyInput): Promise<IssuedApiKey> {
	return restJson<IssuedApiKey>('/api/api-keys', { method: 'POST', body: input });
}

/** 失効（冪等）。行は残り `revokedAt` が立つ。 */
export function revokeApiKey(id: number): Promise<ApiKeySummary> {
	return restJson<ApiKeySummary>(`/api/api-keys/${id}/revoke`, { method: 'POST' });
}

/** 失効済みのキーの削除（有効なキーは 409）。 */
export function deleteApiKey(id: number): Promise<void> {
	return restJson<void>(`/api/api-keys/${id}`, { method: 'DELETE' });
}

/** 待受のアドレスが loopback か（`127.0.0.0/8`・`::1`・`localhost`）。 */
export function isLoopbackHost(host: string): boolean {
	const h = host
		.trim()
		.replace(/^\[|\]$/g, '')
		.toLowerCase();
	return h === 'localhost' || h === '::1' || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(h);
}

/**
 * 設計書 §7: キーが平文で LAN を流れうるか。ページを HTTP で開いていて（HTTPS なら暗号化される）、
 * 待受（`bind`）が loopback でない、または今の接続先が loopback でないときに注意を出す。
 * 待受が分からない（`bind` が `null`）ときは安全側で出す。
 */
export function plaintextOverLan(
	pageProtocol: string,
	pageHostname: string,
	bind: string | null
): boolean {
	if (pageProtocol === 'https:') return false;
	if (!isLoopbackHost(pageHostname)) return true;
	return bind === null || !isLoopbackHost(bind);
}
