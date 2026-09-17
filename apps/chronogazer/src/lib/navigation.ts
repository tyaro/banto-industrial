/**
 * Sidebar navigation definition.
 *
 * From M2, entries for CRUD pages are derived from resource definitions
 * (spec §3.1); manual entries like the ones below remain possible.
 *
 * #359 chronogazer 分（設定画面のカテゴリ別ルート化）: `設定` の遷移先を
 * 先頭カテゴリ `/settings/appearance` に変えた（`/settings` 自体は redirect
 * 専用になるため、ナビからは直接その先を指す）。ただし「現在地の判定」
 * （`pageTitle()`・`Sidebar.svelte` の `isActive`）は `/settings` 配下の
 * どのカテゴリを開いていても『設定』のまま出したい - `path` を素直に
 * prefix 判定に使うと `/settings/data` などで前方一致しなくなる
 * （`path='/settings/appearance'` は `/settings/data` の prefix ではない）
 * ため、判定専用の `activeMatch`（既定は `path` 自身、他の項目は今までと
 * 同じ挙動）を導入し、`設定` だけ `/settings` を明示している
 * （banto-hub の navigation.ts と同じパターン）。
 *
 * #383 段階2a / R1-B: `/tags`（タグ設定）を追加。位置は
 * recorder-requirements.md §6 の画面順（監視・ヒストリカル・タグ設定・…）に
 * 合わせ、ヒストリカルの直後・イベントの手前に置く。`adminOnly` は付けない -
 * viewer も閲覧できる（R0 §3.6: 読み取りは viewer 以上、書き込みは editor
 * 以上）。
 */
export interface NavItem {
	path: string;
	label: string;
	/** Placeholder icon (emoji) until an icon set is decided. */
	icon: string;
	/** Spec M10 RBAC: only shown to the `admin` role. Undefined/false = visible to every role. */
	adminOnly?: boolean;
	/**
	 * 「現在地」判定（前方一致）に使う基準パス。省略時は `path` を使う。
	 * `path` がナビの遷移先（クリック時の実際の URL）で、`activeMatch` は
	 * それとは独立に「この項目が現在地としてハイライトされるべき URL
	 * prefix」を表す - 通常は同じだが、`設定` のようにクリック時の遷移先が
	 * サブパスでも、判定は親パス配下すべてを対象にしたい場合に分ける。
	 */
	activeMatch?: string;
}

export const navItems: NavItem[] = [
	{ path: '/monitor', label: '監視', icon: '📈' },
	{ path: '/historical', label: 'ヒストリカル', icon: '🕰️' },
	{ path: '/tags', label: 'タグ設定', icon: '🏷️' },
	{ path: '/events', label: 'イベント', icon: '🔔' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true },
	{ path: '/settings/appearance', label: '設定', icon: '⚙️', activeMatch: '/settings' }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find((entry) => {
		const match = entry.activeMatch ?? entry.path;
		return pathname === match || pathname.startsWith(match + '/');
	});
	return item?.label ?? 'ChronoGazer';
}
