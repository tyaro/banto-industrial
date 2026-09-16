/**
 * #381 レビュー対応5回目（2026-09-16）: 「閉じたら開いた要素へフォーカスを戻す」
 * を**安全に**行う共有ヘルパー。`escLayering.ts`（Esc の層の約束）の隣に置く小
 * モジュールで、こちらも banto-hub のストア・型を import しない
 * （`lib/components/` 直下のアプリ非依存の規約）。
 *
 * 覚えておいた戻り先は、閉じるまでのあいだに**消えたり不活性になったりする**。
 * 起きうるのは大きく3つ:
 *
 * - **消える**: 条件レンダリング（`{#if}`）の中にあった要素が、その間に外れる。
 *   例: コンテキストメニューの項目から開いた Drawer - メニューは項目を選んだ
 *   直後にアンマウントされるので、閉じるころには戻り先が DOM に居ない。
 * - **不活性になる**: `inert` の中に入る。例: コンテキストメニューを開いたまま
 *   広幅→狭幅にすると、戻り先のツリーノードは退避パネルごと `inert` になる
 *   （`SplitPane.svelte`）。`inert` の中の要素へ `focus()` しても効かない。
 * - **不可視になる**: `display: none` / `visibility: hidden` になる。
 *
 * いずれも「戻せないなら呼び出し側が知っている代わりの要素へ」が正しい振る舞い
 * なので、**戻せるかを判定してから `focus()` し、駄目なら `fallback` へ**渡す。
 * `fallback` が無ければ何もしない（`document.body` へ落とすより、フォーカスの
 * 行き先は呼び出し側に決めさせる）。
 */

/** `el` にフォーカスを戻せるか（DOM に居る・`inert` の中でない・可視）。 */
export function canRestoreFocusTo(el: HTMLElement): boolean {
	// `<body>`/`<html>` は「戻し先」にならない（#381 レビュー対応13回目）:
	// 層を開いた時点のフォーカスが `<body>`（どこもフォーカスしていない状態）
	// だったとき、これを生きた戻し先として受け入れると fallback が飛ばされ、
	// 閉じたあとフォーカスがどこにも無いままになる。
	if (el === document.body || el === document.documentElement) return false;
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
	if (target && canRestoreFocusTo(target)) {
		target.focus();
		return;
	}
	fallback?.();
}
