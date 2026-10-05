import { isAdmin } from '#lib/permissions.js';
import { sessionStore } from '#lib/session.svelte.js';
import { commissioningState } from '#lib/banto/commissioningState.svelte.js';
import { SETTINGS_CATEGORIES, type SettingsCategoryId } from './categories';

/**
 * `settings/` route group 全体の可視カテゴリを計算する（#359 段階2）。
 * `await parent()` で `(app)/+layout.ts` がセッションを確定済み（`sessionStore`
 * は SessionController の snapshot からの導出）であることを保証してから判定する
 * （`users/+page.ts` と同じ順序要件）。
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
 * - `security`: **サーバーが試運転モード（未ロックダウン）か**
 *   （`commissioningState.serverCommissioning`、`GET /api/commissioning/status` を
 *   この load のたびに読み直す）。セッションの種別（試運転の grant か、アカウントか）
 *   では決めない: 初回セットアップで作った admin はアカウントのセッションのままで、
 *   ロックダウンにはまさにその admin が要る（2026-10-04 オーナー指示。v3.0.0 追従の
 *   最初の実装が `kind === 'commissioning'` で出し分けていたのを戻した）。
 *   取得に失敗したら欄を出さない側（安全側）。
 */
export async function load({ parent }) {
	await parent();
	await commissioningState.refresh();

	const admin = isAdmin(sessionStore.role);

	const visible: Record<SettingsCategoryId, boolean> = {
		appearance: true,
		account: true,
		connectivity: admin,
		data: admin,
		security: commissioningState.serverCommissioning
	};

	return { categories: SETTINGS_CATEGORIES.filter((category) => visible[category.id]) };
}
