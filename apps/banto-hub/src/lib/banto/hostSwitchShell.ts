/**
 * ローカルシェル（banto-hub-shell）向け Desktop↔Service 切替の薄いラッパ。
 *
 * Hub 管理 UI 自体は引き続き HTTP REST で運転するが、SCM／切替／UAC 昇格は
 * シェル composition に属するため、ここだけ Tauri invoke／event を使う
 * （docs/banto-hub-desktop-plan.md §9.7）。
 *
 * 非シェル（通常ブラウザ）では `isLocalShell()` が false になり、カード側で
 * 無効化する。`@tauri-apps/api` の静的 import はモジュール評価時に壊れない
 * （invoke 呼び出し時のみランタイムが必要）。
 */

import { invoke } from '@tauri-apps/api/core';

/** `host_switch_status` の戻り値。 */
export type HostSwitchStatus = {
	scmState: string | null;
	autoStart: boolean;
	canOperate: boolean;
	view: 'desktop' | 'service' | 'fallback' | string;
	switching: boolean;
};

/** `host_switch_progress` イベントのペイロード。 */
export type HostSwitchProgress = {
	phase: string;
	message: string;
	done: boolean;
	error: string | null;
};

/** True inside the Tauri webview（シェルの WebView）。 */
export function isLocalShell(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** SCM / ビュー / 権限のスナップショット。 */
export async function getHostSwitchStatus(): Promise<HostSwitchStatus> {
	return (await invoke('host_switch_status')) as HostSwitchStatus;
}

/** Desktop/Offline → Service。 */
export async function switchToService(): Promise<void> {
	await invoke('switch_to_service');
}

/** Service → Desktop。 */
export async function switchToDesktop(): Promise<void> {
	await invoke('switch_to_desktop');
}

/**
 * 自動起動 ON/OFF。UAC 経由で elev を起動する（現在の Running/Stopped は変えない）。
 */
export async function setServiceAutostart(enabled: boolean): Promise<void> {
	await invoke('set_service_autostart', { enabled });
}

/**
 * 進捗イベントを購読する。解除用の関数を返す。
 * 非シェルでは no-op の解除関数を返す。
 */
export async function listenHostSwitchProgress(
	handler: (event: HostSwitchProgress) => void
): Promise<() => void> {
	if (!isLocalShell()) {
		return () => {};
	}
	const { listen } = await import('@tauri-apps/api/event');
	const unlisten = await listen<HostSwitchProgress>('host_switch_progress', (ev) => {
		handler(ev.payload);
	});
	return unlisten;
}
