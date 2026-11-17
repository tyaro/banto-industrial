/**
 * #1159（banto v7.2.0 のメニューバー、nav-menubar-plan.md §4.9・D10-a）: メニューバーが
 * 狭幅・入りきらないときに戻るサイドバーのドロワー（オフキャンバス）の開閉状態と、
 * ナビの置き場所・幅から「シェルが何を出すか」を決める、依存ゼロの純粋ロジック。
 * `$state` を持たないので vitest でそのままテストできる（`mobileNav.svelte.ts` がこれを
 * runes でラップする）。banto-hub の `mobileNav.ts`（T19 S3-a）と同じ形。
 *
 * Hub との差: ChronoGazer のサイドバー表示には元から狭幅のドロワーが無い（広幅の列のまま、
 * ☰ は折り畳みだけ）。その挙動は変えず、ドロワーになるのは**メニューバーが戻る先**
 * （≤900px、または >900px で「その他」にも入らないとき）だけにした。
 */

export interface MobileNavState {
	/** ドロワー（スライドインのサイドバー）が開いているか。 */
	open: boolean;
	/** 現在のビューポートが狭幅（≤900px）かどうか。 */
	isNarrow: boolean;
}

export function initialMobileNavState(): MobileNavState {
	return { open: false, isNarrow: false };
}

/**
 * ビューポート幅の変化を状態へ反映する。広幅へ広がったときはドロワーを開いたままに
 * しておく理由が無いので、ここで強制的に閉じる（次に狭幅へ戻ったとき開いたままに
 * なるのを防ぐ）。
 */
export function applyNarrowChange(state: MobileNavState, isNarrow: boolean): MobileNavState {
	return { isNarrow, open: isNarrow ? state.open : false };
}

/** ☰ ボタンの振り分け先。 */
export type HamburgerTarget = 'offcanvas' | 'desktop-collapse';

/**
 * ☰ ボタンが何を切り替えるか: サイドバーがドロワーのときはその開閉、それ以外
 * （サイドバー表示の広幅）はサイドバーの折り畳み（`settings.toggleSidebar`）。
 */
export function resolveHamburgerTarget(drawerMode: boolean): HamburgerTarget {
	return drawerMode ? 'offcanvas' : 'desktop-collapse';
}

export type ShellNavLayout = 'sidebar' | 'launcher' | 'menubar';

export interface ShellNavView {
	/** ヘッダーにメニューバーをマウントするか（>900px のメニューバー）。 */
	showBar: boolean;
	/** サイドバー（`<aside>`）を描くか。 */
	showSidebar: boolean;
	/** 描いているサイドバーがドロワー（オフキャンバス）か。 */
	drawerMode: boolean;
	/** ヘッダーのドロワーのボタンを出すか（サイドバーがドロワーのとき）。 */
	showDrawerButton: boolean;
}

/**
 * - サイドバー: 常にサイドバー（列。狭幅でも従来どおりで、ドロワーにはしない）。
 * - ドック: サイドバーは出さない（≤900px は `LauncherBar` が下のタブバーになる）。
 * - メニューバー: >900px はヘッダーにバー（サイドバー無し）。≤900px、またはバーが
 *   「その他」にまとめても入りきらない（`insufficient`）ときは、サイドバーをドロワーとして
 *   出す。バーは `insufficient` の間も隠してマウントしたまま（幅が戻ったことを測れるように）。
 */
export function resolveShellNav(
	layout: ShellNavLayout,
	isNarrow: boolean,
	menubarInsufficient: boolean
): ShellNavView {
	const showBar = layout === 'menubar' && !isNarrow;
	const insufficient = showBar && menubarInsufficient;
	const drawerMode = layout === 'menubar' && (isNarrow || insufficient);
	const showSidebar = layout === 'sidebar' || drawerMode;
	return { showBar, showSidebar, drawerMode, showDrawerButton: drawerMode };
}
