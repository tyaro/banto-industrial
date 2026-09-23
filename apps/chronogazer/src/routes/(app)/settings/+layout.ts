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
 * - `hub`（#332、新設）: `connectivity` と同じ `isAdmin(sessionStore.role)`。
 *   Hub への接続は `connectivity`（自アプリの LAN 公開）と同じ「サーバ
 *   制御系 = admin」の範囲。実行形態による可用性（プレーンな `vite dev`
 *   では backend が無い）はカテゴリの可視性ではなく `HubSection.svelte`
 *   側の注記で扱う - `connectivity` が Tauri 以外で注記を出すのと同じ
 *   作法。
 * - `collect`（#383 段階2b / R1-C の C-3b、新設）: **常に表示**。収集の状態の
 *   読み取りは `chronogazer_core::collect::COLLECT_READ_ROLE` = viewer 以上で、
 *   「収集が動いているか」は閲覧者にも要る基本情報だから（R0 §3.6 の viewer は
 *   「閲覧のみ」であって「何も見えない」ではない）。**editor 以上に限るのは
 *   操作（開始・停止・再起動）だけ**で、それは `CollectSection.svelte` 側が
 *   ボタンを出さないことで表す - カテゴリごと隠すと viewer が状態を見る手段を
 *   失う。`hub` を admin 限定にしているのは接続先とキーを扱うからで、収集の
 *   稼働状態はそれとは性質が違う（`COLLECT_READ_ROLE` の Rust doc と同じ
 *   考え方）。
 *
 * `depends('settings:categories')` を宣言し、`SecuritySection.svelte` が
 * 認証モードの変更に成功したあと `invalidate('settings:categories')` を
 * 呼べるようにする（PR #372 Copilot レビュー指摘）。`canManageAuthMode()`
 * は `sessionStore.authDisabled`（エスケープハッチ、spec M11）を参照する
 * ため、admin 未満のロールでログイン不要モードを OFF に戻すと
 * `canManageAuthMode()` の結果が変わる - この `load` は再実行されるまで
 * 結果を再計算しないので、依存キーで明示的に再実行できるようにしておかない
 * とナビに `セキュリティ` が残ったまま `SecuritySection` が何も描画しない
 * 空のページになる。
 */
export async function load({ parent, depends }) {
	depends('settings:categories');
	await parent();

	const admin = isAdmin(sessionStore.role);
	const tauri = isTauri();

	const visible: Record<SettingsCategoryId, boolean> = {
		appearance: true,
		account: true,
		connectivity: admin,
		data: admin && (isAuditLogAvailable() || isBackupsAvailable()),
		security: tauri && canManageAuthMode(),
		hub: admin,
		collect: true
	};

	return { categories: SETTINGS_CATEGORIES.filter((category) => visible[category.id]) };
}
