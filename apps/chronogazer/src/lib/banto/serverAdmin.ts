// banto v3.0.0 の admin-template
// `apps/admin-template/src/lib/banto/serverAdmin.ts` からコピー（I2b、2026-10-04）。
// chronogazer 固有の差: なし（本文は無改変。doc が挙げる `systemAdmin.ts` は ChronoGazer には無い）。
/**
 * Thin wrapper around the src-tauri embedded-server lifecycle commands
 * (spec §11.4). Every export here only makes sense inside the Tauri
 * webview - callers must guard with `isTauri()` (setup.ts) first, same as
 * any other capability-gated UI (spec §11.3: functionality a LAN browser
 * client cannot use is hidden by capability judgement, not disabled/greyed
 * out).
 */
import { invoke } from '@tauri-apps/api/core';
import { isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';

/** One LAN access URL and its QR code (as an inline SVG string), for the settings screen (spec §11.4). */
export interface QrSvg {
	url: string;
	svg: string;
}

/** Mirrors src-tauri's `server_status`/`server_apply` command response shape. */
export interface ServerStatus {
	enabled: boolean;
	running: boolean;
	bind: string;
	port: number;
	/**
	 * 閲覧公開 (Issue #189): whether LAN clients may obtain a synthetic
	 * `viewer` session without logging in (`POST /api/auth/grant/publicViewer`).
	 * Persisted with the other server settings; it has no effect inside this
	 * window, only on the LAN surface.
	 */
	viewerPublic: boolean;
	urls: string[];
	qrSvgs: QrSvg[];
}

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'bad_request',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

/** Same type guard as systemAdmin.ts / providers/tauri.ts (spec §10/§11.1). */
function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

/** Tauri rejects with a `{ kind, message }` object, not an `Error` (Issue #287). */
export function toProviderError(err: unknown): ProviderError {
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

/** Current persisted settings + live running state (spec §11.4). */
export function getServerStatus(): Promise<ServerStatus> {
	return invokeCommand('server_status');
}

/** Persist new settings, stop/restart the server to match, and return the resulting status. */
export function applyServerSettings(
	enabled: boolean,
	bind: string,
	port: number,
	viewerPublic: boolean
): Promise<ServerStatus> {
	return invokeCommand('server_apply', { enabled, bind, port, viewerPublic });
}
