/**
 * MQTT 発行設定（`password` を除く）を共有する最小限のストア（#359 段階1）。
 *
 * 元 `+page.svelte` では ConnectivitySection 相当のロジック（MQTT フォーム）
 * が保有する設定値を、DataSection 相当のロジック（構成パッケージの import
 * 成功後）が `getMqttSettings()` を叩き直して直接上書きしていた
 * （`applyMqttSettings` 呼び出し）。section を別コンポーネントに分けると
 * DataSection から ConnectivitySection のローカル state を直接書けなくなる
 * ため、フォームが表示する設定値そのものを2つの section の外＝ここに出す
 * （フォーム送信中フラグ・エラー文言・`password` 入力欄は
 * ConnectivitySection にしか要らないのでローカルのまま残す - 1つの
 * section に閉じる state は共有に出さない方針）。
 */
import { getMqttSettings, type MqttSettings } from '$lib/banto/mqttSettingsAdmin';

class MqttSettingsStore {
	enabled = $state(false);
	host = $state('');
	port = $state(1883);
	clientId = $state('banto-hub');
	username = $state('');
	prefix = $state('banto');
	qos: 0 | 1 = $state(1);
	minIntervalMs = $state(1000);

	/** `password` はサーバーが返さないため対象外（常に空欄表示 - `mqttSettingsAdmin.ts` の doc comment参照）。 */
	applyLoaded(loaded: MqttSettings): void {
		this.enabled = loaded.enabled;
		this.host = loaded.host;
		this.port = loaded.port;
		this.clientId = loaded.clientId;
		this.username = loaded.username ?? '';
		this.prefix = loaded.prefix;
		this.qos = loaded.qos === 0 ? 0 : 1;
		this.minIntervalMs = loaded.minIntervalMs;
	}

	async load(): Promise<void> {
		this.applyLoaded(await getMqttSettings());
	}
}

export const mqttSettingsStore = new MqttSettingsStore();
