/**
 * H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: API キーの
 * 「期限接近」「期限切れ」「長期未使用」警告判定（純関数のみ、依存ゼロ）。
 *
 * `apiKeysAdmin.ts` からこの内容だけ独立したファイルに切り出してある理由:
 * `apiKeysAdmin.ts` は `@banto/admin-core` の `getAuthProvider` 等をトップ
 * レベルで import しており、その依存先の1つが Svelte 5 の rune
 * （`$state`）をモジュールスコープで使う `.svelte.ts` である。このリポジトリ
 * の vitest 構成は `@sveltejs/vite-plugin-svelte` を導入しない意図的な
 * 最小構成（`vitest.config.ts` の doc comment、H5 決定: 「純関数のユニット
 * テストのみを対象」）なので、rune を含むモジュールを import しただけで
 * `ReferenceError: $state is not defined` になり test が壊れる。この
 * 警告判定ロジックを依存ゼロのこのファイルへ切り出すことで、
 * `apiKeyWarnings.test.ts` が `@banto/admin-core` を一切経由せずに読み込める
 * ようにしてある（`apiKeysAdmin.ts` は既存の import 元互換のため、この
 * モジュールを re-export する - `+page.svelte` は今までどおり
 * `$lib/banto/apiKeysAdmin` から `apiKeyWarnings` を使える）。
 */

/**
 * [`apiKeyWarnings`] が読む最小の形。`apiKeysAdmin.ts` の `ApiKeySummary`
 * は構造的にこれを満たす（TypeScript の構造的部分型付けにより、呼び出し側
 * は `ApiKeySummary` をそのまま渡せる）が、このファイル自体は
 * `apiKeysAdmin.ts` に一切依存しない（上のモジュール doc comment参照）。
 */
export interface ApiKeyExpiryInfo {
	expiresAt: number | null;
	lastUsedAt: number | null;
}

/** 「期限接近」警告のしきい値 - 残り期間がこれ以下なら接近とみなす。 */
export const EXPIRY_WARNING_THRESHOLD_MS = 14 * 24 * 60 * 60 * 1000;

/** 「長期未使用」警告のしきい値 - `lastUsedAt` からの経過がこれ以上なら
 *  長期未使用とみなす。 */
export const LONG_UNUSED_THRESHOLD_MS = 90 * 24 * 60 * 60 * 1000;

/** [`apiKeyWarnings`] の戻り値。 */
export interface ApiKeyWarnings {
	/** 無期限でなく、残り期間が [`EXPIRY_WARNING_THRESHOLD_MS`] 以下（まだ
	 *  期限切れではない）。 */
	expiringSoon: boolean;
	/** 無期限でなく、`expiresAt` が現在時刻以下（境界含む）。 */
	expired: boolean;
	/** `lastUsedAt` があり、そこからの経過が [`LONG_UNUSED_THRESHOLD_MS`]
	 *  以上。 */
	longUnused: boolean;
}

/**
 * `key` の警告状態を `nowMs` 時点で判定する純関数（DB/API を叩かないので
 * `+page.svelte` からもテスト（`apiKeyWarnings.test.ts`）からも同じ結果に
 * なる）。
 *
 * - `expired`/`expiringSoon` は排他: 期限切れの瞬間からは「接近」ではなく
 *   「切れ」表示に切り替わる（`is_expired` と同じ `now_ms >= expires_at`
 *   境界 - `apps/banto-hub/core/src/api_keys.rs` の `is_expired` 参照。
 *   フロントでも同じ境界にして表示と実際の 401 タイミングを一致させる）。
 * - 無期限キー（`expiresAt === null`）は両方とも常に `false`。
 * - `longUnused` は `lastUsedAt === null`（一度も使われていない、大抵は
 *   発行直後）では立てない - 発行直後のキーが即座に「長期未使用」表示に
 *   なる誤検知を避ける設計判断（主たる統制は引き続き `last_used_at` の
 *   監視であり、この警告はその補助表示）。
 */
export function apiKeyWarnings(key: ApiKeyExpiryInfo, nowMs: number): ApiKeyWarnings {
	const expired = key.expiresAt !== null && nowMs >= key.expiresAt;
	const expiringSoon =
		!expired && key.expiresAt !== null && key.expiresAt - nowMs <= EXPIRY_WARNING_THRESHOLD_MS;
	const longUnused = key.lastUsedAt !== null && nowMs - key.lastUsedAt >= LONG_UNUSED_THRESHOLD_MS;
	return { expiringSoon, expired, longUnused };
}
