/**
 * メニューバーが戻るサイドバーのドロワー（≤900px、または入りきらないとき）の開閉状態と
 * 現在のビューポート判定を集約するシングルトンストア（#1159、banto-hub の
 * `mobileNav.svelte.ts` と同じ形）。Svelte 5 runes のクラス。状態遷移の純粋ロジックは
 * `mobileNav.ts`（vitest 対象）に切り出し、ここでは runes への配線と `window.matchMedia` の
 * 購読（`viewportWatch.ts`）だけを行う。☰・サイドバーのトグルの印と探索は
 * `sidebarToggle.ts`（ChronoGazer の既存）。
 */
import { tick, untrack } from 'svelte';
import { settings } from './settings.svelte';
import {
	applyNarrowChange,
	initialMobileNavState,
	resolveHamburgerTarget,
	resolveShellNav,
	type MobileNavState,
	type ShellNavView
} from './mobileNav';
import { findVisibleNavToggle, NAV_TOGGLE_ATTR, toggleSidebarWithFocus } from './sidebarToggle';
import { watchMediaQuery } from './viewportWatch';

/** `(app)/+layout.svelte` のバックドロップ `<button>` のクラス名。 */
export const NAV_BACKDROP_CLASS = 'nav-backdrop';

/** admin-template のブレークポイントに合わせる。 */
export const NARROW_BREAKPOINT_QUERY = '(max-width: 900px)';

class MobileNavStore {
	// ドロワーの条件が CSS のメディアクエリから `drawerMode`（JS）に移ったので、最初の
	// 描画から正しい幅で始める（`watchViewport` の購読を待つと、狭幅で開いた瞬間にサイド
	// バーが列のまま 1 フレーム出る）。matchMedia が無い環境は広幅の既定。
	#state: MobileNavState = $state(
		applyNarrowChange(
			initialMobileNavState(),
			typeof window !== 'undefined' &&
				typeof window.matchMedia === 'function' &&
				window.matchMedia(NARROW_BREAKPOINT_QUERY).matches
		)
	);
	/** メニューバーが「その他」にまとめても入りきらない（`Header` が `HeaderNav` から受けて渡す）。 */
	#menubarInsufficient = $state(false);

	get menubarInsufficient(): boolean {
		return this.#menubarInsufficient;
	}

	setMenubarInsufficient(insufficient: boolean): void {
		this.#menubarInsufficient = insufficient;
	}

	/** ナビの置き場所・幅・入りきらないかから決めるシェルの表示（`resolveShellNav`）。 */
	get view(): ShellNavView {
		return resolveShellNav(settings.navLayout, this.#state.isNarrow, this.#menubarInsufficient);
	}

	/** サイドバーがドロワー（オフキャンバス）になっているか（メニューバーの戻り先のとき）。 */
	get drawerMode(): boolean {
		return this.view.drawerMode;
	}

	get open(): boolean {
		return this.#state.open;
	}

	get isNarrow(): boolean {
		return this.#state.isNarrow;
	}

	openNav(): void {
		if (this.#state.open) return;
		this.#state = { ...this.#state, open: true };
	}

	closeNav(): void {
		if (!this.#state.open) return;
		this.#state = { ...this.#state, open: false };
	}

	/**
	 * バックドロップのクリックで閉じる。バックドロップ自身が閉じると同時に DOM から消えるので、
	 * フォーカスが `<body>` に落ちる。閉じた後に見えている ☰ へ移す。バックドロップ以外に
	 * フォーカスがあるときは奪わない。
	 */
	closeNavFromBackdrop(): void {
		const active = document.activeElement;
		// クリックでボタンにフォーカスが付かない環境（Safari 等）では `<body>` のまま。
		const hadFocus =
			active === document.body ||
			(active instanceof HTMLElement && active.classList.contains(NAV_BACKDROP_CLASS));
		this.closeNav();
		if (!hadFocus) return;
		void tick().then(() => findVisibleNavToggle()?.focus());
	}

	toggleNav(): void {
		this.#state = { ...this.#state, open: !this.#state.open };
	}

	/**
	 * `(app)/+layout.svelte` の `$effect` から一度だけ呼ぶ。返り値の関数を `$effect` の
	 * クリーンアップとして呼ぶこと。**`untrack` が必須**: `watchMediaQuery` は購読直後に
	 * `onChange` を同期的に 1 回呼ぶので、`untrack` 無しで `this.#state` を読んでから書くと、
	 * その読み取りが呼び出し元の `$effect` の依存になり、書き込みのたびに再実行されて
	 * `effect_update_depth_exceeded` になる（banto-hub の実機で踏んだ罠、2026-09-04）。
	 */
	watchViewport(): () => void {
		return watchMediaQuery(NARROW_BREAKPOINT_QUERY, (matches) => {
			untrack(() => {
				// 幅の切り替えで、フォーカスのあった ☰ が消えることがある。消えるとフォーカスが
				// `<body>` に落ちるので、**状態を変える前（DOM 更新前）**に「☰ にフォーカスが
				// あったか」を控え、更新後に見えている側の ☰ へ移す。☰ 以外にフォーカスが
				// あるときは何もしない（奪わない）。
				const active = document.activeElement;
				const hadToggleFocus =
					active instanceof HTMLElement && active.hasAttribute(NAV_TOGGLE_ATTR);
				this.#state = applyNarrowChange(this.#state, matches);
				if (!hadToggleFocus) return;
				void tick().then(() => {
					const now = document.activeElement;
					const stillOnToggle =
						now instanceof HTMLElement && now.isConnected && now.hasAttribute(NAV_TOGGLE_ATTR);
					if (!stillOnToggle) findVisibleNavToggle()?.focus();
				});
			});
		});
	}

	/**
	 * `Header.svelte` の ☰ から呼ぶ振り分け: ドロワーのときは開閉、それ以外はサイドバーの
	 * 折り畳み（押したボタンが入れ替わるので、描画後に見えている側のトグルへフォーカスを
	 * 移す、`toggleSidebarWithFocus`）。
	 */
	toggleHamburger(): void {
		if (resolveHamburgerTarget(this.drawerMode) === 'offcanvas') {
			this.toggleNav();
		} else {
			toggleSidebarWithFocus();
		}
	}
}

export const mobileNavStore = new MobileNavStore();
