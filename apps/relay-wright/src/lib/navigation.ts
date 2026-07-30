/**
 * Sidebar navigation definition.
 *
 * W1 (plan `luminous-discovering-goblet.md`): only banto's standard
 * settings/users/audit-log screens exist. ChronoGazer's domain screens
 * (監視/ヒストリカル/イベント) are NOT carried over - this app's own
 * monitoring/rule/write-target screens land in W2 (registry CRUD) and W4
 * (monitoring/operation screens), as new entries here, not a revival of
 * ChronoGazer's.
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
	{ path: '/settings', label: '設定', icon: '⚙️' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find(
		(entry) => pathname === entry.path || pathname.startsWith(entry.path + '/')
	);
	return item?.label ?? APP_NAME;
}
