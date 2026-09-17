import { isAdmin } from '$lib/permissions';
import { isTauri } from '$lib/banto/setup';
import { isAuditLogAvailable } from '$lib/banto/auditLogAdmin';
import { isBackupsAvailable } from '$lib/banto/backupsAdmin';
import { isMonitorAvailable } from '$lib/banto/monitorAdmin';
import { isEngineAvailable } from '$lib/banto/engineAdmin';
import { sessionStore } from '$lib/session.svelte';
import { canManageAuthMode } from './shared';
import { SETTINGS_CATEGORIES, type SettingsCategoryId } from './categories';

/**
 * `settings/` route group 全体の可視カテゴリを計算する（#359 relay-wright
 * 分）。`await parent()` で `(app)/+layout.ts` が `sessionStore` を初期化
 * 済みであることを保証してから判定する（banto-hub・chronogazer の同名
 * ファイルと同じ順序要件）。
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
 * - `security`: relay-wright 固有の3セクション（認証・タグモニタ手動書き込み・
 *   アーム時限失効 H10、カテゴリ割り当ての判断理由は
 *   `SecuritySection.svelte` の doc comment参照）の OR:
 *   元「認証」section の `tauri && canManageAuthMode()`、元「タグモニタ
 *   手動書き込み」section の `monitorAvailable && isAdmin`、元「アーム
 *   時限失効」section の `armConfigAvailable && isAdmin`。どれか1つでも
 *   見えれば security カテゴリ自体は可視（`data` と同じ OR パターン）。
 * - `hub`（#332、新設）: `connectivity` と同じ `isAdmin(sessionStore.role)`。
 *   Hub への接続は `connectivity`（自アプリの LAN 公開）と同じ「サーバ
 *   制御系 = admin」の範囲。実行形態による可用性（プレーンな `vite dev`
 *   では backend が無い）はカテゴリの可視性ではなく `HubSection.svelte`
 *   側の注記で扱う - `connectivity` が Tauri 以外で注記を出すのと同じ
 *   作法。
 *
 * `depends('settings:categories')` を宣言し、`SecuritySection.svelte` が
 * 認証モードの変更に成功したあと `invalidate('settings:categories')` を
 * 呼べるようにする（chronogazer PR #372 Copilot レビュー指摘と同型の
 * 事前対応）。`canManageAuthMode()` は `sessionStore.authDisabled`
 * （エスケープハッチ、spec M11）を参照するため、admin 未満のロールで
 * ログイン不要モードを OFF に戻すと `canManageAuthMode()` の結果が変わる -
 * この `load` は再実行されるまで結果を再計算しないので、依存キーで明示的に
 * 再実行できるようにしておかないと、（タグモニタ/アーム時限失効も見えない
 * non-admin セッションの場合）ナビに `セキュリティ` が残ったまま
 * `SecuritySection` が何も描画しない空のページになる。
 */
export async function load({ parent, depends }) {
	depends('settings:categories');
	await parent();

	const admin = isAdmin(sessionStore.role);
	const tauri = isTauri();
	const monitorAvailable = isMonitorAvailable();
	const armConfigAvailable = isEngineAvailable();

	const visible: Record<SettingsCategoryId, boolean> = {
		appearance: true,
		account: true,
		connectivity: admin,
		data: admin && (isAuditLogAvailable() || isBackupsAvailable()),
		security:
			(tauri && canManageAuthMode()) || (monitorAvailable && admin) || (armConfigAvailable && admin),
		hub: admin
	};

	return { categories: SETTINGS_CATEGORIES.filter((category) => visible[category.id]) };
}
