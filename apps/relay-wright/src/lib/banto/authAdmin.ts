/**
 * Thin wrapper around the src-tauri auth-mode commands (spec M11: login-not-
 * required mode + desktop autologin). Every export here only makes sense
 * inside the Tauri webview - callers must guard with `isTauri()` (setup.ts)
 * first, same as `serverAdmin.ts`'s embedded-server controls (spec §11.4):
 * a LAN browser client/the plain-browser demo have no desktop process of
 * their own to configure autologin or a windowless "trust this device" mode
 * for, so those environments get a fixed "デスクトップアプリでのみ変更でき
 * ます" note instead of calling any of these.
 *
 * Unlike `serverAdmin.ts`, rejections here are normalized to `ProviderError`
 * (same convention as `providers/tauri.ts`/`usersAdmin.ts`) rather than left
 * as the raw `ErrorBody`-shaped value a Tauri command rejects with - the
 * settings page needs `err.message` to be the exact Japanese string the
 * exclusivity guards (`auth_config_apply`/`server_apply`'s Rust-side checks)
 * produce, e.g. "認証無効モード中はLANアクセスを有効化できません", so it can
 * be shown in a toast as-is.
 */
import { invoke } from '@tauri-apps/api/core';
import { isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';

export type AuthDisabledRole = 'admin' | 'editor' | 'viewer';

/** Mirrors src-tauri's `AuthSettings` (camelCase on the wire, spec M11). */
export interface AuthSettings {
	disabled: boolean;
	disabledRole: AuthDisabledRole;
	autologinEnabled: boolean;
	autologinUsername: string | null;
}

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

/** Same type guard as providers/tauri.ts / usersAdmin.ts (spec §10/§11.1). */
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

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

/** Current auth-mode settings. Any authenticated role may call this (it only feeds a settings-screen display). */
export function getAuthSettings(): Promise<AuthSettings> {
	return call<AuthSettings>('auth_config_get');
}

/** Toggle login-not-required mode and its synthetic-identity role. `admin`-only, EXCEPT while the mode is currently on (escape hatch - see `auth_config_apply`'s Rust doc comment). */
export function applyAuthSettings(
	disabled: boolean,
	disabledRole: AuthDisabledRole
): Promise<AuthSettings> {
	return call<AuthSettings>('auth_config_apply', { disabled, disabledRole });
}

/** Enable desktop autologin for `username`/`password` (verified against the real account store first - a bad credential surfaces as a `ProviderError` with `kind: 'validation'`). `admin`-only. */
export function enableAutologin(username: string, password: string): Promise<void> {
	return call<void>('autologin_enable', { username, password });
}

/** Disable desktop autologin and remove its stored credential from the OS keyring. `admin`-only. */
export function disableAutologin(): Promise<void> {
	return call<void>('autologin_disable');
}
