/**
 * #378 / #381 レビュー対応（2026-09-16）: **Esc の「層の約束」**をここ1箇所に
 * 集約する。`SplitPane.svelte`（狭幅の退避ツリー）と `(app)/+layout.svelte`
 * （オフキャンバスサイドバー）の両方がこれを使う - 同じ説明・同じ判定を2箇所に
 * 置かない。
 *
 * ## 約束
 *
 * 重なって出ている UI は **Esc で上から1層ずつ畳む**。そのために、
 *
 * 1. **閉じる側はイベントを消費する**（`event.preventDefault()`）。下の層は
 *    `event.defaultPrevented` を見て譲る。
 * 2. **下の層は、可視な上位層が出ているあいだ Esc に反応しない**
 *    （{@link hasVisibleLayerAbove}）。`event.target` を見るだけでは足りない:
 *    `Drawer.svelte` はタブ移動を閉じ込めない（同ファイル冒頭 doc）ので、
 *    ダイアログが出たままフォーカスだけが下の層に戻っていることがあり、その
 *    Esc は「発生元が上位層の中ではない」ので素通りしてしまう。
 *
 * 1 だけに頼れないのは、window に張った Esc ハンドラ同士では
 * `stopPropagation` が効かず、どちらが先に走るかがリスナーの登録順しだいに
 * なるため（登録順は「いつ開いたか」で変わる）。2 があれば順序に依存しない。
 *
 * ## 層（z-index、大きいほど手前）
 *
 * | 層                                          | z-index | role                      |
 * | ------------------------------------------- | ------- | ------------------------- |
 * | `CommandPalette.svelte` / `ToastHost.svelte` / `TreeContextMenu.svelte` | 1000 | `dialog` / -（トースト）/ `menu` |
 * | `Drawer.svelte` / `Modal.svelte`            | 900     | `dialog`                  |
 * | オフキャンバスサイドバー（`Sidebar.svelte`、バックドロップ 700） | 710 | -（常設ナビ）    |
 * | 狭幅の退避ツリー（`SplitPane.svelte`、バックドロップ 600） | 610 | `region`         |
 *
 * {@link LAYER_ABOVE_SELECTOR} が拾うのは **Esc で閉じる一時的な UI**
 * （`dialog` = Drawer/Modal/CommandPalette、`menu` = TreeContextMenu）だけ。
 * トーストは Esc で閉じないので入れない。サイドバーは role を名乗らない常設
 * ナビなのでセレクタでは拾えず、代わりに約束1（閉じるときに消費する）を
 * `(app)/+layout.svelte` 側が守る - それより下の層は退避ツリーだけなので
 * これで足りる。新しく重なる UI を足すときは、この表と下のセレクタを更新すること。
 */

/** Esc で閉じる一時的な上位層が名乗る role（上の表を参照）。 */
export const LAYER_ABOVE_SELECTOR = '[role="dialog"], [role="menu"]';

/**
 * いま**可視な**上位層（{@link LAYER_ABOVE_SELECTOR}）が出ているか。
 *
 * 閉じている Drawer/Modal/メニューは `{#if open}` で DOM ごと消えるが、
 * `display: none` や `visibility: hidden` で閉じるものが将来混じっても
 * 誤検出しないよう、矩形の有無と計算済みスタイルの両方で可視性を見る。
 */
export function hasVisibleLayerAbove(): boolean {
	for (const el of document.querySelectorAll(LAYER_ABOVE_SELECTOR)) {
		if (el.getClientRects().length === 0) continue;
		const style = getComputedStyle(el);
		if (style.display === 'none' || style.visibility === 'hidden') continue;
		return true;
	}
	return false;
}
