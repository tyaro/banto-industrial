/**
 * ナビの置き場所と、ドックの位置の型・既定値・保存値の読み込み（#1159、banto v7.2.0 の
 * メニューバー。nav-launcher-plan.md §3・nav-menubar-plan.md §3）。runes を使わない
 * 純粋なモジュールなので単体テストできる（`settings.svelte.ts` が localStorage と
 * `UiSettingsProvider` への配線を持つ）。不正・未保存の値は既定へ戻す。
 */

/**
 * ナビの置き場所（nav-launcher-plan.md §3・nav-menubar-plan.md §3）: サイドバー、
 * ドック（値は `'launcher'`）、ヘッダーの行に入れるメニューバー。
 */
export type NavLayout = 'sidebar' | 'launcher' | 'menubar';

/** Hub の既定はメニューバー（横幅を取り戻す、#1159）。admin-template の既定はサイドバー。 */
export const NAV_LAYOUT_DEFAULT: NavLayout = 'menubar';

/** ドックを置く画面の端。`@banto/ui` の `LauncherPosition` と同じ値（900px 以下は常に下のタブバー）。 */
export type LauncherPosition = 'bottom' | 'top' | 'left' | 'right';

export const LAUNCHER_POSITION_DEFAULT: LauncherPosition = 'bottom';

export function isNavLayout(value: unknown): value is NavLayout {
	return value === 'sidebar' || value === 'launcher' || value === 'menubar';
}

export function isLauncherPosition(value: unknown): value is LauncherPosition {
	return value === 'bottom' || value === 'top' || value === 'left' || value === 'right';
}

/** 保存値 -> ナビの置き場所。未保存・不正な値は既定（メニューバー）。 */
export function parseNavLayout(stored: string | null): NavLayout {
	return isNavLayout(stored) ? stored : NAV_LAYOUT_DEFAULT;
}

/** 保存値 -> ドックの位置。未保存・不正な値は既定（下）。 */
export function parseLauncherPosition(stored: string | null): LauncherPosition {
	return isLauncherPosition(stored) ? stored : LAUNCHER_POSITION_DEFAULT;
}
