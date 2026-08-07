/**
 * TreeView.svelte（汎用部品、T13-1 — docs/ux-plan.md §4b）のノード型。
 *
 * `@banto/grid-svelte` の `types.ts`（`GridColumn` 等を独立ファイルに
 * 切り出し、`BantoGrid.svelte` 自体からは export しない）と同じ流儀 -
 * .svelte ファイルのインスタンススクリプトから型を export して他ファイル
 * から `import type { X } from './Y.svelte'` する構成は避け、共有する
 * 型は素の .ts ファイルに置く。
 */
export interface TreeNode<T> {
	id: string;
	data: T;
	children?: TreeNode<T>[];
}
