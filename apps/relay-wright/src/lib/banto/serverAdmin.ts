/**
 * Thin wrapper around the src-tauri embedded-server lifecycle commands
 * (spec §11.4). Every export here only makes sense inside the Tauri
 * webview - callers must guard with `isTauri()` (setup.ts) first, same as
 * any other capability-gated UI (spec §11.3: functionality a LAN browser
 * client cannot use is hidden by capability judgement, not disabled/greyed
 * out).
 */
import { invoke } from '@tauri-apps/api/core';

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
	urls: string[];
	qrSvgs: QrSvg[];
}

/** Current persisted settings + live running state (spec §11.4). */
export function getServerStatus(): Promise<ServerStatus> {
	return invoke('server_status');
}

/** Persist new settings, stop/restart the server to match, and return the resulting status. */
export function applyServerSettings(
	enabled: boolean,
	bind: string,
	port: number
): Promise<ServerStatus> {
	return invoke('server_apply', { enabled, bind, port });
}
