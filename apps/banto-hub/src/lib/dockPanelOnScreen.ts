/**
 * ドックの枠のパネルが**実際に画面に出ているか**（#1253。下のログのパネルを、見えている
 * 間だけ読むための判定）。runes を使わない純粋な関数で、単体テストできる。
 *
 * 部品の `isVisible` / `dock.panels` の `visible` は、畳んだ区画・自動で隠した帯・タブの組の
 * 裏も「見えている」に数えるので、ここでは配置（`dock.layout`）から読む（タグ登録の
 * 属性の `paneOperableIn` と同じ読み方。PR #1269 のレビュー P2）:
 *
 * - 浮かせた窓: 開いていて、タイトルバーへ畳んでいない。閉じた浮動（隠した）は偽。
 * - 自動で隠した帯: 帯から引き出している（`flyoutPanel` がそのパネル）間だけ真。
 * - ドッキング: その区画（パネル自身、またはタブの組）が畳まれておらず、タブの組なら
 *   表のタブがそのパネル。
 *
 * 狭い表示では部品が追加パネルを隠す（閉じた浮動になる）ので、狭い表示の一時の配置を
 * そのまま渡せば偽になる。
 */
import type { DockLayout, DockNode } from '@banto/dock-svelte';

export function isDockPanelOnScreen(
	layout: DockLayout,
	id: string,
	flyoutPanel: string | null
): boolean {
	const win = layout.floating.find((w) => w.id === id);
	if (win) return win.open && win.collapsed !== true;
	const autoHidden = (layout.autoHidden ?? []).some((entry) =>
		entry.node.type === 'tabs'
			? entry.node.children.some((child) => child.id === id)
			: entry.node.id === id
	);
	if (autoHidden) return flyoutPanel === id;
	return dockedOnScreen(layout.docked, id, null);
}

function dockedOnScreen(node: DockNode | null, id: string, parent: DockNode | null): boolean {
	if (!node) return false;
	if (node.type === 'panel') {
		if (node.id !== id) return false;
		if (node.collapsed === true) return false;
		if (parent?.type === 'tabs') {
			return parent.collapsed !== true && parent.children[parent.activeIndex]?.id === id;
		}
		return true;
	}
	return node.children.some((child) => dockedOnScreen(child, id, node));
}
