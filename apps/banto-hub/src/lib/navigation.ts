/**
 * サイドバーナビゲーション定義。relay-wright/chronogazer の navigation.ts
 * と同型だが、項目は banto-hub 独自（実装指示: 状態(/status)・PLC接続・
 * 収集グループ・タグ登録・APIキー(admin限定)・ユーザー管理(admin限定)・
 * 監査ログ(admin限定)・書き込み監査(admin限定、T2-4)・設定）を書き下ろし。
 *
 * 状態モニタ(/status)を先頭に置くのは、このアプリの主用途が「タグサーバー
 * が正しく収集できているかを見る」ことだからで、ルート `/` からの
 * redirect 先にもなっている（routes/+page.ts）。
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
	{ path: '/plc-connections', label: 'PLC接続', icon: '🔌' },
	{ path: '/collection-groups', label: '収集グループ', icon: '🗂️' },
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
