/**
 * Sidebar navigation definition.
 *
 * W1 (plan `luminous-discovering-goblet.md`): only banto's standard
 * settings/users/audit-log screens existed. ChronoGazer's domain screens
 * (監視/ヒストリカル/イベント) are NOT carried over - this app's own
 * monitoring/rule/write-target screens are new entries here, not a revival
 * of ChronoGazer's.
 *
 * W2 adds this app's own registry CRUD screens: write-targets (書き込み先)
 * and write-rules (書き込みルール). Both are editor-write/viewer-read (spec
 * M10), same as the rest of this list, so they are NOT `adminOnly` - the
 * page itself hides create/edit/delete controls for a viewer
 * (`canWriteResources`, see the two routes' `+page.svelte`). Monitoring/
 * operation screens remain W4.
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
	{ path: '/write-targets', label: '書き込み先', icon: '🎯' },
	{ path: '/write-rules', label: '書き込みルール', icon: '🧮' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find(
		(entry) => pathname === entry.path || pathname.startsWith(entry.path + '/')
	);
	return item?.label ?? APP_NAME;
}
