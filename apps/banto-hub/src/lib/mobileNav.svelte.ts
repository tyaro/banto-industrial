/**
 * T19 S3-a（UX-43、docs/banto-hub-t19-design.md §8.2）: オフキャンバス
 * サイドバー（≤900px）の開閉状態と現在のビューポート判定を集約する
 * シングルトンストア。`commandPalette.svelte.ts` と同じ流儀（Svelte 5
 * runes のクラス）。状態遷移の純粋ロジックは `mobileNav.ts`
 * （vitest 対象）に切り出し、ここでは runes への配線と
 * `window.matchMedia` の購読（`viewportWatch.ts`）だけを行う。
 */
import { untrack } from 'svelte';
import { settings } from './settings.svelte';
import {
	applyNarrowChange,
	initialMobileNavState,
	resolveHamburgerTarget,
	type MobileNavState
} from './mobileNav';
import { watchMediaQuery } from './viewportWatch';

/** admin-template のブレークポイント・docs/banto-hub-t19-design.md §8.2 に合わせる。 */
export const NARROW_BREAKPOINT_QUERY = '(max-width: 900px)';

class MobileNavStore {
	#state: MobileNavState = $state(initialMobileNavState());

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

	toggleNav(): void {
		this.#state = { ...this.#state, open: !this.#state.open };
	}

	/**
	 * `(app)/+layout.svelte` の `$effect` から一度だけ呼ぶ。返り値の関数を
	 * `$effect` のクリーンアップとして呼ぶこと。SSR/`matchMedia` 未実装
	 * 環境でも例外を投げない（`viewportWatch.ts` 側のガード）。
	 *
	 * **`untrack` が必須**（実機で踏んだ罠、2026-09-04）: `watchMediaQuery`
	 * は購読直後に `onChange` を同期的に1回呼ぶ。この呼び出しは
	 * `$effect(() => mobileNavStore.watchViewport())` の実行中に起こるため、
	 * `untrack` 無しで `this.#state` を読んでから書くと、その読み取りが
	 * 「今動いている $effect の依存」として登録されてしまう。直後の書き込み
	 * は毎回新しいオブジェクト（スプレッドで作った別参照）なので、値が同じ
	 * でも「依存が変わった」と判定され、その $effect 自身が再実行 → また
	 * 読んで書く → 再実行…という無限ループになり、Svelte が
	 * `effect_update_depth_exceeded` を投げて描画が壊れる（ログアウト後に
	 * ログイン画面が描画されず `page.getByLabel('ユーザー名')` が
	 * タイムアウトする形で E2E に現れた）。`untrack` で読み取りを追跡対象
	 * から外し、この effect が `#state` に依存しないようにして断ち切る。
	 */
	watchViewport(): () => void {
		return watchMediaQuery(NARROW_BREAKPOINT_QUERY, (matches) => {
			untrack(() => {
				this.#state = applyNarrowChange(this.#state, matches);
			});
		});
	}

	/**
	 * `Header.svelte` の ☰ ボタンから呼ぶ振り分け: 狭幅ではオフキャンバスの
	 * 開閉、デスクトップでは従来どおりサイドバーの折り畳み。
	 */
	toggleHamburger(): void {
		if (resolveHamburgerTarget(this.#state.isNarrow) === 'offcanvas') {
			this.toggleNav();
		} else {
			settings.toggleSidebar();
		}
	}
}

export const mobileNavStore = new MobileNavStore();
