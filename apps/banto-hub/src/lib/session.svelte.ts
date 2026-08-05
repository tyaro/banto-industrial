/**
 * relay-wright の同名ファイルから複製。現在セッションの identity/role
 * （Svelte 5 runes）。
 *
 * 差分: `authDisabled` は常に false 固定に単純化した。relay-wright の
 * ログイン不要モード（Tauri のみ・spec M11）は banto-hub のスコープ外
 * （実装指示「スコープ外: ...自動ログイン設定セクション」）なので、
 * `isTauri()`/`getAuthSettings()` への依存自体を削除している（banto-hub
 * は Tauri を持たない headless axum サーバーのみ）。
 */
import { getAuthProvider, type Identity } from '@banto/admin-core';
import { parseRole, type Role } from './permissions';

class SessionStore {
	identity: Identity | null = $state(null);
	role: Role = $state('viewer');

	/** banto-hub には Tauri のログイン不要モードが無いため常に false。 */
	authDisabled = $state(false);

	/** 現在の identity を取得し、role を導出する（フェイルクローズ - parseRole 参照）。 */
	async load(): Promise<void> {
		this.identity = await getAuthProvider().getIdentity();
		this.role = parseRole(this.identity);
	}
}

export const sessionStore = new SessionStore();
