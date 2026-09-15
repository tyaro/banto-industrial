/**
 * `GET /api/status` から複数の設定カテゴリ・ルートが参照する値をまとめて
 * 共有する最小限のストア（#359 段階2、PR #371 の Copilot レビュー是正）。
 *
 * 段階1（section 分割のみ・単一 `/settings` ページ）では「収集の稼働状態」の
 * 5秒ポーリングを ConnectivitySection（MQTT 表示を持つ section）の admin
 * 限定 `$effect` が担い、`mqttConnected`（当時は1つの section にしか
 * 要らなかった）はローカルに残し、DataSection（構成パッケージ import
 * ガード）からも参照される `collectionState` だけをここに出していた。
 *
 * ところが段階2でカテゴリを実ルートへ分割した結果、`/settings/data` へ
 * 直接遷移すると ConnectivitySection がマウントされずポーリングが
 * 一度も始まらないため、`collectionState` が `null` のまま =
 * 構成パッケージの import ガード（`DataSection.svelte` の
 * `importGuardActive`）が常に無効という回帰が生じた（PR #371 の Copilot
 * レビュー指摘）。
 *
 * 上流テンプレート v1.6.0 も同種の問題（複数ルートから参照される値の
 * 初回ロードを1つの section 任せにすると、他ルートへ直接来たときに
 * 抜け落ちる）に当たっており、`authSettingsStore`/`systemInfoStore` の
 * 取得を `settings/+layout.svelte` の `$effect` に引き上げることで
 * 解決している（上流 PR #198 の Copilot レビュー由来）。本ストアも同じ
 * 方針に揃え、ポーリングの起動・停止を `settings/+layout.svelte` へ
 * 移した（admin 限定という既存条件はそのまま維持 - `+layout.svelte` 参照）。
 * 1回のフェッチで取れる `collectionState` と `mqttConnected` の両方を
 * ここに持たせ、ConnectivitySection は自前でポーリングせずこのストアを
 * 読むだけにする（MQTT 設定の保存直後だけは即時反映のため個別に
 * `load()` を呼ぶ - `ConnectivitySection.svelte` 参照）。
 */
import { getHubStatus, type StatusResponse } from '$lib/banto/hubStatus';

class HubStatusStore {
	collectionState: string | null = $state(null);
	mqttConnected = $state(false);

	/** `getHubStatus()` を1回叩き、`collectionState`/`mqttConnected` を更新して結果を返す（呼び出し元は他フィールドも使える）。 */
	async load(): Promise<StatusResponse> {
		const status = await getHubStatus();
		this.collectionState = status.collection_state;
		this.mqttConnected = status.mqtt.connected;
		return status;
	}
}

export const hubStatusStore = new HubStatusStore();
