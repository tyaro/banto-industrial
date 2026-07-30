/**
 * Thin wrapper around the src-tauri window-vibrancy commands (spec M12:
 * desktop "real glass" - Windows Acrylic behind a transparent window,
 * Windows only per the 2026-07-08 roadmap decision). Tauri-webview only -
 * callers must guard with `isTauri()` (setup.ts) first, same convention as
 * `serverAdmin.ts`/`authAdmin.ts`. The settings page additionally hides the
 * toggle entirely when `vibrancy_status()` reports `supported: false`
 * (non-Windows, or an OS build without Acrylic).
 *
 * Rejections are normalized to `ProviderError` (same as `authAdmin.ts`) so
 * the settings page can toast `err.message` as-is.
 */
import { invoke } from '@tauri-apps/api/core';
import { isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';

/** Mirrors src-tauri's `vibrancy_status` command response shape (spec M12). */
export interface VibrancyStatus {
	enabled: boolean;
	supported: boolean;
}

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

/** Same type guard as providers/tauri.ts / authAdmin.ts (spec §10/§11.1). */
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

/** Current vibrancy state + whether this OS supports it at all. Any authenticated role may call this (it only gates a settings-screen display). */
export function getVibrancyStatus(): Promise<VibrancyStatus> {
	return call<VibrancyStatus>('vibrancy_status');
}

/** Turn the window acrylic effect on/off; resolves to the resulting enabled state. `admin`-only. */
export function applyVibrancy(enabled: boolean): Promise<boolean> {
	return call<boolean>('vibrancy_apply', { enabled });
}
