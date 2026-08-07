/**
 * ConnectionTree.svelte（T13-1、docs/ux-plan.md §4b）のノードデータ型。
 * TreeView.svelte の `treeTypes.ts` と同じ理由で、.svelte のインスタンス
 * スクリプトから export せず素の .ts ファイルに置く（呼び出し側の
 * tags ページがこの型を import できるようにするため）。
 */
import type { PlcConnection, CollectionGroup } from '$lib/banto/tagRegistryAdmin';

export type ConnectionTreeNodeData =
	| { kind: 'all' }
	| { kind: 'connection'; connection: PlcConnection }
	| { kind: 'group'; group: CollectionGroup; connection: PlcConnection };
