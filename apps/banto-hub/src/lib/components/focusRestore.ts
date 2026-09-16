/**
 * #381 レビュー対応5回目（2026-09-16）: 「閉じたら開いた要素へフォーカスを戻す」
 * を**安全に**行う共有ヘルパー。`escLayering.ts`（Esc の層の約束）の隣に置く小
 * モジュールで、こちらも banto-hub のストア・型を import しない
 * （`lib/components/` 直下のアプリ非依存の規約）。
 *
 * 覚えておいた戻り先は、閉じるまでのあいだに**消えたり不活性になったりする**:
 *
 * - コンテキストメニューを開いたまま広幅→狭幅にすると、戻り先のツリーノードは
 *   退避パネルごと `inert` になる（`SplitPane.svelte`）。`inert` の中の要素へ
 *   `focus()` しても効かず、フォーカスは `<body>` に落ちる。
 * - モニタ（`(app)/monitor/+page.svelte`）はタグが0件になると `SplitPane` ごと
 *   アンマウントするので、戻り先のトグルボタン自体が DOM から消える。
 *
 * どちらも「戻せないなら呼び出し側が知っている代わりの要素へ」が正しい振る舞い
 * なので、**戻せるかを判定してから `focus()` し、駄目なら `fallback` へ**渡す。
 * `fallback` が無ければ何もしない（`document.body` へ落とすより、フォーカスの
 * 行き先は呼び出し側に決めさせる）。
 */

/** `el` にフォーカスを戻せるか（DOM に居る・`inert` の中でない・可視）。 */
function canRestoreTo(el: HTMLElement): boolean {
	if (!el.isConnected) return false;
	if (el.closest('[inert]')) return false;
	if (el.getClientRects().length === 0) return false;
	const style = getComputedStyle(el);
	return style.display !== 'none' && style.visibility !== 'hidden';
}

/**
 * `target` へフォーカスを戻す。戻せない（消えた / `inert` / 不可視）ときは
 * `fallback` を呼ぶ - `fallback` 未指定なら何もしない。
 */
export function restoreFocus(target: HTMLElement | null | undefined, fallback?: () => void): void {
	if (target && canRestoreTo(target)) {
		target.focus();
		return;
	}
	fallback?.();
}
