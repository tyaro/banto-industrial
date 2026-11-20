/**
 * 監視画面の「束」の純粋な部分（#1207・#1231 PR 5b、trend-design.md §9 Q5）。
 *
 * 束は表示グループの任意の名前で、同じ名前のグループが 1 つの束になる（束のテーブルは
 * 持たない）。束が 1 つも無ければ画面は今までどおりグループのタブ 1 列。束があるときは、
 * 束のタブ（1 段目）と、選んだ束のグループのタブ（2 段目）に分ける。選んだ束は選択中の
 * グループから求める（`?group=` と前回のグループの記憶がそのまま効く）。
 */

import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';

/** 束なしのグループをまとめる 1 つの束タブの表示名。 */
export const NO_BUNDLE_LABEL = '束なし';

export interface BundleTab {
	/** 束の名前。`null` = 束なしのグループの束。 */
	key: string | null;
	label: string;
}

/** グループの束のキー（束なし = `null`）。 */
export function bundleKeyOf(group: Pick<DisplayGroup, 'bundle'>): string | null {
	return group.bundle ?? null;
}

/**
 * 束のタブの一覧。`groups` は表示の並び（`sortGroups` の後）。束が 1 つも無ければ
 * `null`（束の段を出さない）。束は最初のグループの現れる順、束なしは 1 つにまとめて
 * 最初の束なしのグループの現れる位置に置く。
 */
export function bundleTabs(groups: readonly Pick<DisplayGroup, 'bundle'>[]): BundleTab[] | null {
	if (!groups.some((group) => bundleKeyOf(group) !== null)) return null;
	const seen = new Set<string | null>();
	const tabs: BundleTab[] = [];
	for (const group of groups) {
		const key = bundleKeyOf(group);
		if (seen.has(key)) continue;
		seen.add(key);
		tabs.push({ key, label: key ?? NO_BUNDLE_LABEL });
	}
	return tabs;
}

/** 束のグループ（並びは `groups` のまま）。 */
export function groupsOfBundle<T extends Pick<DisplayGroup, 'bundle'>>(
	groups: readonly T[],
	key: string | null
): T[] {
	return groups.filter((group) => bundleKeyOf(group) === key);
}

/** タブ列のキー操作（WAI-ARIA のタブの作法）。移動しないキーは `null`。 */
export function tabKeyTarget(key: string, index: number, length: number): number | null {
	if (length <= 0) return null;
	if (key === 'ArrowRight') return (index + 1) % length;
	if (key === 'ArrowLeft') return (index - 1 + length) % length;
	if (key === 'Home') return 0;
	if (key === 'End') return length - 1;
	return null;
}
