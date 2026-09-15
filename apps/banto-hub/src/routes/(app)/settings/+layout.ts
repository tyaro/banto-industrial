import { isAdmin } from '$lib/permissions';
import { sessionStore } from '$lib/session.svelte';
import { SETTINGS_CATEGORIES, type SettingsCategoryId } from './categories';

/**
 * `settings/` route group 全体の可視カテゴリを計算する（#359 段階2）。
 * `await parent()` で `(app)/+layout.ts` が `sessionStore` を初期化済みで
 * あることを保証してから判定する（`users/+page.ts` と同じ順序要件）。
 *
 * 可視条件は元 `+page.svelte`（段階1の各 section）の `{#if}` ガードを
 * そのまま踏襲しただけで、新しい判定は作っていない:
 * - `appearance`/`account`: 常に表示（元々ガード無し）。
 * - `connectivity`: 元の `canManageMqtt || canManageGrpc`
 *   （ConnectivitySection.svelte、いずれも `isAdmin(sessionStore.role)`
 *   で同値）。
 * - `data`: 元の `canManageStore || (canManageMqtt || canManageGrpc)`
 *   （DataSection.svelte、データ保持と構成パッケージの両ガード。全て
 *   `isAdmin(sessionStore.role)` と同値）。
 * - `security`: 元の `sessionStore.commissioningMode`
 *   （SecuritySection.svelte）。
 */
export async function load({ parent }) {
	await parent();

	const admin = isAdmin(sessionStore.role);

	const visible: Record<SettingsCategoryId, boolean> = {
		appearance: true,
		account: true,
		connectivity: admin,
		data: admin,
		security: sessionStore.commissioningMode
	};

	return { categories: SETTINGS_CATEGORIES.filter((category) => visible[category.id]) };
}
