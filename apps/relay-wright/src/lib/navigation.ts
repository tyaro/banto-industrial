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
 * (`canWriteResources`, see the two routes' `+page.svelte`).
 *
 * W4 adds the monitoring/operation screens: engine (エンジン制御・監視) near
 * the top since it is the operator's primary control surface, and
 * write-audit-log (書き込み監査ログ) alongside the other log/監査 screens.
 * Both are viewer+ reachable (status/log are viewer-readable) - the engine
 * page role-gates its arm/disarm/dry-run/reload controls internally
 * (`isAdmin`/`canWriteResources`, backend also enforces), so neither is
 * `adminOnly` here.
 *
 * R1-B adds the tag-registry CRUD screens: plc-connections (PLC接続) and
 * tags (タグ登録; 収集グループ management lives INSIDE that screen). They
 * sit ABOVE 書き込み先/書き込みルール so the registry reads top-down in
 * setup order: 接続 → タグ → 書き込み先 → ルール. Same viewer-read/
 * editor-write split as the W2 screens, so not `adminOnly`.
 *
 * qr-codes (QRコード) is a debug utility: 画面に表示したQRコードをタッチ
 * パネル（HMI）のQRリーダーでスキャンするための文字列リスト+表示画面。
 * PLC/レジストリ系とは独立なので、ログ系の下（管理者専用画面の手前）に
 * 置く。viewer は表示（スキャン）可能なので not `adminOnly`.
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
	{ path: '/engine', label: 'エンジン制御・監視', icon: '🕹️' },
	{ path: '/settings', label: '設定', icon: '⚙️' },
	{ path: '/plc-connections', label: 'PLC接続', icon: '🔌' },
	{ path: '/tags', label: 'タグ登録', icon: '🏷️' },
	{ path: '/write-targets', label: '書き込み先', icon: '🎯' },
	{ path: '/write-rules', label: '書き込みルール', icon: '🧮' },
	{ path: '/write-audit-log', label: '書き込み監査ログ', icon: '📝' },
	{ path: '/qr-codes', label: 'QRコード', icon: '🔳' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find(
		(entry) => pathname === entry.path || pathname.startsWith(entry.path + '/')
	);
	return item?.label ?? APP_NAME;
}
