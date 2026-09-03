/**
 * T19 S3-a（UX-43、docs/banto-hub-t19-design.md §8.2「案C・banto-hub の
 * み」）: ≤900px でサイドバーをオフキャンバス化するための開閉状態を扱う
 * 依存ゼロの純粋ロジック。`$state` を持たないので vitest でそのまま
 * テストできる（`mobileNav.svelte.ts` がこれを runes でラップする）。
 */

export interface MobileNavState {
	/** オフキャンバス（≤900px 時のスライドインサイドバー）が開いているか。 */
	open: boolean;
	/** 現在のビューポートが狭幅（≤900px）かどうか。 */
	isNarrow: boolean;
}

export function initialMobileNavState(): MobileNavState {
	return { open: false, isNarrow: false };
}

/**
 * ビューポート幅の変化を状態へ反映する。デスクトップ幅へ広がったときは
 * オフキャンバスを開いたままにしておく理由が無いので、ここで強制的に
 * 閉じる（狭幅 → 回転/リサイズでデスクトップ幅に戻ったときに、次に狭幅へ
 * 戻ったときも開いたままになる不具合を防ぐ）。
 */
export function applyNarrowChange(state: MobileNavState, isNarrow: boolean): MobileNavState {
	return { isNarrow, open: isNarrow ? state.open : false };
}

/** ☰ ボタンの振り分け先。 */
export type HamburgerTarget = 'offcanvas' | 'desktop-collapse';

/**
 * ☰ ボタンが何を切り替えるべきかを決める（docs/banto-hub-t19-design.md の
 * 「ハンバーガーの振り分け」節）: 狭幅（≤900px）ではオフキャンバスの開閉、
 * デスクトップではサイドバーの折り畳み（既存の `settings.toggleSidebar`）。
 */
export function resolveHamburgerTarget(isNarrow: boolean): HamburgerTarget {
	return isNarrow ? 'offcanvas' : 'desktop-collapse';
}
