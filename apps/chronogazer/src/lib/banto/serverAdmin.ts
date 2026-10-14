// banto v3.0.0 の admin-template
// `apps/admin-template/src/lib/banto/serverAdmin.ts` からコピー（I2b、2026-10-04）。
// chronogazer 固有の差（#392 B2-2、2026-10-14）: `server` モード（サービスの
// `chronogazer-server` の HTTP の画面）では Tauri のコマンドの代わりに REST
// （`GET`/`PUT /api/settings/server`、B2-1）を呼び、応答の `restartRequired` を持つ。
// demo では reject する。エラーの正規化は `restJson.ts` に寄せた（doc が挙げる
// `systemAdmin.ts` は ChronoGazer には無い）。
/**
 * The embedded-server (LAN access) settings (spec §11.4). Inside the Tauri
 * webview: the src-tauri lifecycle commands (`server_status`/`server_apply`).
 * In a browser tab served by the service host (`chronogazer-server`, #392
 * B2-2): the same shape over REST, admin-only; the server accepts a change
 * only from this PC (`hostInfo.ts`'s `settingsEditableHere`), and a change of
 * the always-on listener's shape (the port, `0.0.0.0`) is saved for the next
 * start (`restartRequired`). Callers still hide what they cannot use
 * (spec §11.3).
 */
import { invoke } from '@tauri-apps/api/core';
import { ProviderError } from '@banto/admin-core';
import { restJson, toProviderError } from './restJson';
import { getBantoMode } from './setup';

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
	/**
	 * REST only (#392 B2-2, `chronogazer-server`): the saved settings change the
	 * always-on listener (the port, `0.0.0.0`) and take effect from the next
	 * start. Absent from the Tauri commands' answer.
	 */
	restartRequired?: boolean;
}

/** The message a settings call rejects with when there is no backend (plain `vite dev`/demo). */
export const SERVER_SETTINGS_DEMO_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: SERVER_SETTINGS_DEMO_MESSAGE });
}

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

/** Current persisted settings + live running state (spec §11.4). */
export async function getServerStatus(): Promise<ServerStatus> {
	const mode = getBantoMode();
	if (mode === 'server') return restJson<ServerStatus>('/api/settings/server', { method: 'GET' });
	if (mode === 'demo') throw demoModeError();
	return invokeCommand('server_status');
}

/** Persist new settings, stop/restart the server to match, and return the resulting status. */
export async function applyServerSettings(
	enabled: boolean,
	bind: string,
	port: number,
	viewerPublic: boolean
): Promise<ServerStatus> {
	const mode = getBantoMode();
	if (mode === 'server') {
		return restJson<ServerStatus>('/api/settings/server', {
			method: 'PUT',
			body: { enabled, bind, port, viewerPublic }
		});
	}
	if (mode === 'demo') throw demoModeError();
	return invokeCommand('server_apply', { enabled, bind, port, viewerPublic });
}
