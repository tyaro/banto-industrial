/**
 * 設定画面の section コンポーネント（AppearanceSection/AccountSection/
 * ConnectivitySection/DataSection/SecuritySection.svelte）が2つ以上から
 * 参照する非リアクティブな共有関数の置き場（#359 relay-wright 分、
 * banto-hub・chronogazer の shared.ts と同じ役割・同じファイル名）。1つの
 * section にしか要らないものはここに出さず、その section 自身に残す。
 *
 * リアクティブな共有 state（`AuthSettings` 本体）は
 * `authSettingsStore.svelte.ts` を参照。Svelte 5 の rune は `.svelte.ts`
 * でしか使えないため、プレーン TS のこのファイルには置けない。
 */
import { isProviderError } from '@banto/admin-core';
import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';

/**
 * 元 `+page.svelte` に重複していた `errorMessage()` を1本化しただけで、
 * 判定ロジックは無改変（`ProviderError` の validation field_errors を
 * 優先して表示する。Appearance/Account/Data/Security の各 section が使う）。
 */
export function errorMessage(err: unknown): string {
	if (isProviderError(err)) {
		if (err.body.kind === 'validation' && err.body.field_errors.length > 0) {
			return err.body.field_errors.map((fe) => fe.message).join(' / ');
		}
		return err.message;
	}
	return String(err);
}

/**
 * ESCAPE HATCH（spec M11、元 `+page.svelte` の `canManageAuthMode` を無改変で
 * 1本化）: ログイン不要モードが**現在**有効な間は、どのロールでもこれを
 * 呼べる - でなければ `admin` 未満の合成セッション（例: `viewer` に設定した
 * キオスク）が二度と認証を戻せなくなる。`+layout.ts`（security カテゴリの
 * 可視性判定の一部 - タグモニタ/アーム時限失効との OR、doc comment参照）と
 * `SecuritySection.svelte`（認証サブセクションの表示ガード）の両方から
 * 参照されるため shared.ts に置く（新しい判定は発明していない）。
 */
export function canManageAuthMode(): boolean {
	return isAdmin(sessionStore.role) || sessionStore.authDisabled;
}
