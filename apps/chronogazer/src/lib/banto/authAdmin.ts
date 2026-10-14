/**
 * Thin wrapper around the auth-mode settings (spec M11: login-not-required
 * mode + desktop autologin; #392 B2-2: this PC's connection `localGrant`).
 *
 * - **Tauri webview**: the src-tauri commands (`auth_config_get`/
 *   `auth_config_apply`/`autologin_*`). Rejections are normalized to
 *   `ProviderError` (same convention as `providers/tauri.ts`/`usersAdmin.ts`):
 *   the settings page needs `err.message` to be the exact Japanese string the
 *   exclusivity guards (`auth_config_apply`/`server_apply`'s Rust-side checks)
 *   produce, e.g. "認証無効モード中はLANアクセスを有効化できません", so it can
 *   be shown in a toast as-is.
 * - **Browser tab served by the service host** (`server` mode, #392 B2-2):
 *   `GET`/`PUT /api/settings/auth` (admin; B2-1). The login-not-required mode
 *   is read-only there (it re-binds the desktop window's own session and has
 *   no effect on REST sessions - the service uses `localGrant` instead), and
 *   autologin does not exist (no desktop process to log in) - those calls
 *   reject with a fixed message. A `localGrant` change is accepted only from
 *   this PC (`hostInfo.ts`'s `settingsEditableHere`).
 * - **Plain-browser demo**: rejects (no backend).
 */
import { invoke } from '@tauri-apps/api/core';
import { ProviderError } from '@banto/admin-core';
import { restJson, toProviderError } from './restJson';
import { getBantoMode } from './setup';

export type AuthDisabledRole = 'admin' | 'editor' | 'viewer';

/** `auth.local_grant`（#392 B2-1）: この PC からの接続に渡す role、または `off`。 */
export type LocalGrantSetting = 'off' | 'viewer' | 'editor' | 'admin';

/**
 * Mirrors src-tauri's `AuthSettings` (camelCase on the wire, spec M11). In
 * `server` mode the REST view (`chronogazer_core::server_admin::AuthConfigView`)
 * is mapped onto it: autologin is always off there, and the `localGrant*`
 * fields are present (absent in the Tauri answer).
 */
export interface AuthSettings {
	disabled: boolean;
	disabledRole: AuthDisabledRole;
	autologinEnabled: boolean;
	autologinUsername: string | null;
	/** Saved `auth.local_grant` (REST only). */
	localGrant?: LocalGrantSetting;
	/** Whether this host issues `localDesktop` at all (`chronogazer-server` only; REST only). */
	localGrantSupported?: boolean;
	/** The saved value takes effect from the next start (REST only). */
	localGrantRestartRequired?: boolean;
}

/** `GET`/`PUT /api/settings/auth` の応答。 */
interface AuthConfigView {
	disabled: boolean;
	disabledRole: AuthDisabledRole;
	localGrant: LocalGrantSetting;
	localGrantSupported: boolean;
	localGrantRestartRequired: boolean;
}

function fromRest(view: AuthConfigView): AuthSettings {
	return {
		disabled: view.disabled,
		disabledRole: view.disabledRole,
		autologinEnabled: false,
		autologinUsername: null,
		localGrant: view.localGrant,
		localGrantSupported: view.localGrantSupported,
		localGrantRestartRequired: view.localGrantRestartRequired
	};
}

/** `server` モードで使えない操作の reject の本文。 */
export const DESKTOP_ONLY_MESSAGE =
	'この設定はデスクトップアプリの画面でのみ変更できます（サービスではこの PC からの接続の設定を使います）';
/** `tauri` モードで使えない操作（`localGrant`）の reject の本文。 */
export const SERVICE_ONLY_MESSAGE = 'この PC からの接続の設定はサービスの画面でのみ変更できます';
/** demo の reject の本文。 */
export const AUTH_SETTINGS_DEMO_MESSAGE = 'デモモードでは利用できません';

function rejectWith(message: string): never {
	throw new ProviderError({ kind: 'other', message });
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

/** Current auth-mode settings. Any authenticated role may call this (it only feeds a settings-screen display). */
export async function getAuthSettings(): Promise<AuthSettings> {
	const mode = getBantoMode();
	if (mode === 'server') {
		return fromRest(await restJson<AuthConfigView>('/api/settings/auth', { method: 'GET' }));
	}
	if (mode === 'demo') rejectWith(AUTH_SETTINGS_DEMO_MESSAGE);
	return call<AuthSettings>('auth_config_get');
}

/**
 * `server` モードだけ（#392 B2-2）: `auth.local_grant` を保存する（admin、この PC から
 * だけ）。値が変わると発行済みの `localDesktop` のトークンは失効する（呼び出し側の
 * セッションも含みうるので、画面は保存の後に `refreshAll()` で確定し直す）。
 */
export async function applyLocalGrant(localGrant: LocalGrantSetting): Promise<AuthSettings> {
	const mode = getBantoMode();
	if (mode === 'tauri') rejectWith(SERVICE_ONLY_MESSAGE);
	if (mode === 'demo') rejectWith(AUTH_SETTINGS_DEMO_MESSAGE);
	return fromRest(
		await restJson<AuthConfigView>('/api/settings/auth', { method: 'PUT', body: { localGrant } })
	);
}

function requireTauri(): void {
	const mode = getBantoMode();
	if (mode === 'server') rejectWith(DESKTOP_ONLY_MESSAGE);
	if (mode === 'demo') rejectWith(AUTH_SETTINGS_DEMO_MESSAGE);
}

/** Toggle login-not-required mode and its synthetic-identity role. `admin`-only, EXCEPT while the mode is currently on (escape hatch - see `auth_config_apply`'s Rust doc comment). */
export async function applyAuthSettings(
	disabled: boolean,
	disabledRole: AuthDisabledRole
): Promise<AuthSettings> {
	requireTauri();
	return call<AuthSettings>('auth_config_apply', { disabled, disabledRole });
}

/** Enable desktop autologin for `username`/`password` (verified against the real account store first - a bad credential surfaces as a `ProviderError` with `kind: 'validation'`). `admin`-only. */
export async function enableAutologin(username: string, password: string): Promise<void> {
	requireTauri();
	return call<void>('autologin_enable', { username, password });
}

/** Disable desktop autologin and remove its stored credential from the OS keyring. `admin`-only. */
export async function disableAutologin(): Promise<void> {
	requireTauri();
	return call<void>('autologin_disable');
}
