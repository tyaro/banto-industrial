import { isAdmin } from '$lib/permissions';
import { isTauri } from '$lib/banto/setup';
import { isAuditLogAvailable } from '$lib/banto/auditLogAdmin';
import { isBackupsAvailable } from '$lib/banto/backupsAdmin';
import { sessionStore } from '$lib/session.svelte';
import { canManageAuthMode } from './shared';
import { SETTINGS_CATEGORIES, type SettingsCategoryId } from './categories';

/**
 * `settings/` route group 全体の可視カテゴリを計算する（#359 chronogazer
 * 分）。`await parent()` で `(app)/+layout.ts` が `sessionStore` を初期化
 * 済みであることを保証してから判定する（banto-hub の同名ファイルと同じ
 * 順序要件）。
 *
 * 可視条件は元 `+page.svelte`（分割前）の各セクションの `{#if}` ガードを
 * そのまま踏襲しただけで、新しい判定は作っていない:
 * - `appearance`: 常に表示（テーマ section 自体にガード無し。ウィンドウ効果
 *   だけが `tauri && isAdmin && vibrancyStatus?.supported` だが、カテゴリ
 *   自体はテーマ section があるので常に可視）。
 * - `account`: 常に表示（アカウント section にガード無し。自動ログイン
 *   section だけが `tauri && isAdmin` だが、カテゴリ自体はアカウント
 *   section があるので常に可視）。
 * - `connectivity`: 元「LANアクセス」section の `isAdmin(sessionStore.role)`。
 * - `data`: 元「監査ログの保持ポリシー」`auditAvailable && isAdmin` と
 *   「バックアップ/リストア」`backupsAvailable && isAdmin` の OR
 *   （どちらか一方でも見えれば data カテゴリ自体は可視）。
 * - `security`: 元「認証」section の `tauri && canManageAuthMode`
 *   （`canManageAuthMode` は `shared.ts` 参照）。
 */
export async function load({ parent }) {
	await parent();

	const admin = isAdmin(sessionStore.role);
	const tauri = isTauri();

	const visible: Record<SettingsCategoryId, boolean> = {
		appearance: true,
		account: true,
		connectivity: admin,
		data: admin && (isAuditLogAvailable() || isBackupsAvailable()),
		security: tauri && canManageAuthMode()
	};

	return { categories: SETTINGS_CATEGORIES.filter((category) => visible[category.id]) };
}
