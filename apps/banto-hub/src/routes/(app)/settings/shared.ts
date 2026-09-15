/**
 * 設定画面の section コンポーネント（AppearanceSection/AccountSection/
 * ConnectivitySection/DataSection/SecuritySection.svelte）が2つ以上から
 * 参照する非リアクティブな共有関数の置き場（#359 段階1、上流テンプレート
 * v1.6.0 の settings/shared.ts と同じ役割・同じファイル名）。1つの section
 * にしか要らないものはここに出さず、その section 自身に残す。
 *
 * リアクティブな共有 state（MQTT/gRPC 設定値・収集状態）は
 * `mqttSettingsStore.svelte.ts` / `grpcSettingsStore.svelte.ts` /
 * `hubStatusStore.svelte.ts` を参照。Svelte 5 の rune は `.svelte.ts` でしか
 * 使えないため、プレーン TS のこのファイルには置けない（テンプレートの
 * doc comment と同じ理由）。
 */
import { isProviderError } from '@banto/admin-core';
import { isConfigPackageImportAbortedError } from '$lib/banto/configPackageAdmin';

/**
 * 元 `+page.svelte` に重複していた `errorMessage()` を1本化しただけで、
 * 判定ロジックは無改変（DataSection の構成パッケージ import 中断エラー・
 * その他の `ProviderError` の validation field_errors を優先して表示する）。
 */
export function errorMessage(err: unknown): string {
	if (isConfigPackageImportAbortedError(err)) {
		return err.message;
	}
	if (isProviderError(err)) {
		if (err.body.kind === 'validation' && err.body.field_errors.length > 0) {
			return err.body.field_errors.map((fe) => fe.message).join(' / ');
		}
		return err.message;
	}
	return String(err);
}
