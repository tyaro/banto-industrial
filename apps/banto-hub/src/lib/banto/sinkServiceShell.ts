/**
 * S6（docs/banto-hub-external-db-design.md §5.5・§7 row S6）: デスクトップ
 * シェル（banto-hub-shell）向け「サービス」一覧のうち `BantoHubSink`
 * （外部 DB 連携 S5 のサイドカー）側の薄いラッパー。
 *
 * `hostSwitchShell.ts`（`BantoHub` 自身の Desktop↔Service 切替）と同じ形の
 * Tauri invoke ラッパーだが、`BantoHubSink` にはホスト切替（desktop/service
 * の二重化）という概念が無い - 単純な SCM 上の1サービスとして
 * query/start/stop するだけ（`apps/banto-hub/src-tauri/src/sink_service_ipc.rs`
 * のモジュール doc参照）。非シェル（通常ブラウザ）では `isLocalShell()` が
 * false になり、呼び出し側（`status/+page.svelte`）でカードごと隠す。
 */

import { invoke } from '@tauri-apps/api/core';
import { isLocalShell } from './hostSwitchShell';

export { isLocalShell };

/** `sink_service_status` の戻り値。 */
export type SinkServiceStatus = {
	scmState: string | null;
	canOperate: boolean;
};

/** `BantoHubSink` の SCM 状態・権限のスナップショット。 */
export async function getSinkServiceStatus(): Promise<SinkServiceStatus> {
	return (await invoke('sink_service_status')) as SinkServiceStatus;
}

/** `BantoHubSink` を開始する（冪等）。 */
export async function startSinkService(): Promise<void> {
	await invoke('sink_service_start');
}

/** `BantoHubSink` を停止する（冪等）。 */
export async function stopSinkService(): Promise<void> {
	await invoke('sink_service_stop');
}
