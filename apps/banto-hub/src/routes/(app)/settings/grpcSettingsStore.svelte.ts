/**
 * gRPC サーバー設定を共有する最小限のストア（#359 段階1）。
 * `mqttSettingsStore.svelte.ts` と同じ理由（構成パッケージの import 成功後、
 * DataSection 相当のロジックが ConnectivitySection 相当のフォームへ最新値を
 * 反映する必要がある）で、フォーム送信中フラグ・エラー文言は
 * ConnectivitySection にしか要らないのでローカルのまま残す。
 */
import { getGrpcSettings, type GrpcSettings } from '$lib/banto/grpcSettingsAdmin';

class GrpcSettingsStore {
	enabled = $state(false);
	bind = $state('127.0.0.1');
	port = $state(50051);

	applyLoaded(loaded: GrpcSettings): void {
		this.enabled = loaded.enabled;
		this.bind = loaded.bind;
		this.port = loaded.port;
	}

	async load(): Promise<void> {
		this.applyLoaded(await getGrpcSettings());
	}
}

export const grpcSettingsStore = new GrpcSettingsStore();
