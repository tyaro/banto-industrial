/**
 * サイドバーの折り畳み / 開くボタン（☰）の置き場所とフォーカスの扱い。banto-hub の
 * #714（`apps/banto-hub/src/lib/mobileNav.ts` の `NAV_TOGGLE_ATTR`・`SIDEBAR_ID`・
 * `findVisibleNavToggle`）と同じ形。ChronoGazer には狭幅のオフキャンバスが無いので、
 * ☰ は折り畳みの切り替えだけを担う。
 *
 * ボタンは状態により「サイドバー側」と「ヘッダー側」のどちらか一方だけが出る
 * （開いている → サイドバーのロゴ行に折り畳みボタン、折り畳み中 → ヘッダーに開くボタン）。
 * 押すと押した側が消えるので、描画後に見えている側へフォーカスを移す（`<body>` に落とさない）。
 */
import { tick } from 'svelte';
import { settings } from './settings.svelte.js';

/** ☰ ボタン（ヘッダー側・サイドバー側）が付ける印。フォーカスの逃げ先の探索に使う。 */
export const NAV_TOGGLE_ATTR = 'data-nav-toggle';

/** サイドバー（`<aside>`）の id。トグルの `aria-controls` が指す。 */
export const SIDEBAR_ID = 'app-sidebar';

/** いま見えている方のトグルを探して返す（見えていなければ null）。 */
export function findVisibleNavToggle(root: ParentNode = document): HTMLElement | null {
	for (const el of root.querySelectorAll<HTMLElement>(`[${NAV_TOGGLE_ATTR}]`)) {
		if (el.closest('[inert]')) continue;
		if (el.getClientRects().length === 0) continue;
		if (getComputedStyle(el).visibility === 'hidden') continue;
		return el;
	}
	return null;
}

/**
 * サイドバーの折り畳み / 展開。押したボタンが入れ替わって消えるので、
 * 描画後に見えている側のトグルへフォーカスを移す。
 */
export function toggleSidebarWithFocus(): void {
	settings.toggleSidebar();
	void tick().then(() => findVisibleNavToggle()?.focus());
}
