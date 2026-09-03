/**
 * サイドバーナビゲーション定義。relay-wright/chronogazer の navigation.ts
 * と同型だが、項目は banto-hub 独自（状態(/status)・タグ登録・タグモニタ・
 * APIキー(admin限定)・ユーザー管理(admin限定)・監査ログ(admin限定)・
 * 書き込み監査(admin限定、T2-4)・設定）を書き下ろし。
 *
 * 状態モニタ(/status)を先頭に置くのは、このアプリの主用途が「タグサーバー
 * が正しく収集できているかを見る」ことだからで、ルート `/` からの
 * redirect 先にもなっている（routes/+page.ts）。
 *
 * T19 S1-d（UX-30、docs/banto-hub-t19-design.md、2026-09-03）: 旧
 * `PLC接続`（`/plc-connections`）・`収集グループ`（`/collection-groups`）の
 * 2エントリを削除した。設定の入口はタグ画面（`/tags`）へ一本化済み（S1-a〜
 * S1-c）で、両画面の固有機能（接続テスト・シミュレーション切替・
 * `word_order`・calc/mem 配下のグループ操作・viewer の閲覧手段）は既に
 * タグ画面のツリー右クリックメニュー・Drawer へ移設されている（設計 §7.1）。
 * `commands.ts` はこの配列（`navItems`）から自動生成するため、削除した
 * 2エントリはコマンドパレットからも同時に消える。
 */
import { APP_NAME } from '$lib/appName';

export interface NavItem {
	path: string;
	label: string;
	/** Placeholder icon (emoji) until an icon set is decided. */
	icon: string;
	/** RBAC: only shown to the `admin` role. Undefined/false = visible to every role. */
	adminOnly?: boolean;
}

export const navItems: NavItem[] = [
	{ path: '/status', label: '状態', icon: '📡' },
	{ path: '/tags', label: 'タグ登録', icon: '🏷️' },
	{ path: '/monitor', label: 'タグモニタ', icon: '📈' },
	{ path: '/api-keys', label: 'APIキー', icon: '🔑', adminOnly: true },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true },
	{ path: '/write-audit', label: '書き込み監査', icon: '✍️', adminOnly: true },
	{ path: '/settings', label: '設定', icon: '⚙️' }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find(
		(entry) => pathname === entry.path || pathname.startsWith(entry.path + '/')
	);
	return item?.label ?? APP_NAME;
}
