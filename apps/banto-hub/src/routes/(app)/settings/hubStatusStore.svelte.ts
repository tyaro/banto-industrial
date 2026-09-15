/**
 * `GET /api/status` の `collection_state`（収集の稼働状態）だけを共有する
 * 最小限のストア（#359 段階1）。
 *
 * 元 `+page.svelte` では MQTT セクションの5秒ポーリング（admin 限定の
 * `$effect`、`getHubStatus()` を叩く）が `mqttConnected` と `collectionState`
 * の両方を1回のフェッチで更新し、後者を構成パッケージ import ガード
 * （`importGuardActive`）が読んでいた。section 分割後もポーリングの起動・
 * 停止は引き続き ConnectivitySection（MQTT 表示の一部）が担う - 1つの
 * section に閉じる `mqttConnected` はこのストアに出さずローカルのままにし、
 * DataSection（構成パッケージ）からも参照される `collectionState` だけを
 * ここに出す（2つ以上の section が参照する state だけを共有する方針）。
 *
 * テンプレートの authSettingsStore/systemInfoStore（layout マウント時に
 * 一度だけ load する形）とは異なり、この値は「5秒ごとに変わりうる収集の
 * 稼働状態」であり、layout の初回ロードで1回読むだけでは不十分なため、
 * `load()` は ConnectivitySection のポーリングと DataSection の import 後
 * 再取得の両方から都度呼ばれる想定にしている。
 */
import { getHubStatus, type StatusResponse } from '$lib/banto/hubStatus';

class HubStatusStore {
	collectionState: string | null = $state(null);

	/** `getHubStatus()` を1回叩き、`collectionState` を更新して結果を返す（呼び出し元は `mqtt.connected` 等の他フィールドも使える）。 */
	async load(): Promise<StatusResponse> {
		const status = await getHubStatus();
		this.collectionState = status.collection_state;
		return status;
	}
}

export const hubStatusStore = new HubStatusStore();
