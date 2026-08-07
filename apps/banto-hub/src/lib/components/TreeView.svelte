<script lang="ts" generics="T">
	/**
	 * 汎用部品（T13-1、docs/ux-plan.md §4b）: 2階層のツリー
	 * （ノード展開/折りたたみ・選択・バッジ/カウントのスロット）。
	 *
	 * データはジェネリックな `nodes` prop で受け取り、banto-hub の型
	 * （PlcConnection/CollectionGroup 等）には一切依存しない。ノードの
	 * 見た目（ラベル・バッジ・カウント）は `label` スナペットに委ねる —
	 * TreeView 自身はバッジ/カウントの概念を知らない（呼び出し側の
	 * ConnectionTree.svelte がシミュレーションバッジ等をここで描画する）。
	 *
	 * 展開状態は内部管理（非 controlled）。新しいルートノードが現れたら
	 * 自動展開し、それ以外はユーザーの開閉操作を尊重する（`reload()` の
	 * たびに `nodes` 配列が丸ごと差し替わる呼び出し元があっても、既存
	 * ノードの開閉状態を勝手に巻き戻さないため）。
	 *
	 * 2026-08-08 スコープ追加（T13-3 の「ツリーからの右クリック作成」の
	 * 拡張点、オーナー決定）: ノードの右クリックを `oncontextmenu`
	 * コールバック prop として上位へ通知できる。メニュー UI 自体は
	 * この汎用部品に含めない（呼び出し側が出す）。prop 未指定なら
	 * `preventDefault` せず素通しし、ブラウザ標準のコンテキストメニューを
	 * そのまま出す。
	 */
	import type { Snippet } from 'svelte';
	import type { TreeNode } from './treeTypes';

	interface Props {
		nodes: TreeNode<T>[];
		selectedId?: string | null;
		onselect?: (node: TreeNode<T>) => void;
		ontoggle?: (node: TreeNode<T>, expanded: boolean) => void;
		oncontextmenu?: (node: TreeNode<T>, position: { x: number; y: number }) => void;
		label: Snippet<[TreeNode<T>]>;
	}

	let { nodes, selectedId = null, onselect, ontoggle, oncontextmenu, label }: Props = $props();

	let expanded = $state<Set<string>>(new Set());
	const seenRootIds = new Set<string>();

	// 新規に現れたルートノードだけ自動展開する（既存ノードの開閉状態は
	// 保持する）。上のコメント参照。
	$effect(() => {
		for (const node of nodes) {
			if (!seenRootIds.has(node.id)) {
				seenRootIds.add(node.id);
				expanded.add(node.id);
			}
		}
	});

	function toggle(node: TreeNode<T>): void {
		if (!node.children?.length) return;
		if (expanded.has(node.id)) {
			expanded.delete(node.id);
			ontoggle?.(node, false);
		} else {
			expanded.add(node.id);
			ontoggle?.(node, true);
		}
	}

	function handleContextMenu(node: TreeNode<T>, event: MouseEvent): void {
		if (!oncontextmenu) return; // 素通し（ブラウザ標準メニュー）
		event.preventDefault();
		oncontextmenu(node, { x: event.clientX, y: event.clientY });
	}
</script>

<div class="tree" role="tree">
	{#each nodes as node (node.id)}
		<div class="node-group">
			<div
				class="node-row"
				class:selected={selectedId === node.id}
				role="treeitem"
				aria-selected={selectedId === node.id}
			>
				{#if node.children?.length}
					<button
						type="button"
						class="toggle"
						onclick={() => toggle(node)}
						aria-label={expanded.has(node.id) ? '折りたたむ' : '展開する'}
					>
						{expanded.has(node.id) ? '▾' : '▸'}
					</button>
				{:else}
					<span class="toggle-spacer"></span>
				{/if}
				<button
					type="button"
					class="node-label"
					onclick={() => onselect?.(node)}
					oncontextmenu={(event) => handleContextMenu(node, event)}
				>
					{@render label(node)}
				</button>
			</div>
			{#if node.children?.length && expanded.has(node.id)}
				<div class="children">
					{#each node.children as child (child.id)}
						<div
							class="node-row child"
							class:selected={selectedId === child.id}
							role="treeitem"
							aria-selected={selectedId === child.id}
						>
							<span class="toggle-spacer"></span>
							<button
								type="button"
								class="node-label"
								onclick={() => onselect?.(child)}
								oncontextmenu={(event) => handleContextMenu(child, event)}
							>
								{@render label(child)}
							</button>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{/each}
</div>

<style>
	.tree {
		display: flex;
		flex-direction: column;
		padding: 0.4rem;
		font-size: 0.85rem;
	}

	.node-row {
		display: flex;
		align-items: center;
		gap: 0.2rem;
	}

	.node-row.child {
		padding-left: 1.4rem;
	}

	.toggle,
	.toggle-spacer {
		flex: 0 0 auto;
		width: 1.4rem;
		height: 1.6rem;
	}

	.toggle {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		cursor: pointer;
		font-size: 0.7rem;
		border-radius: var(--banto-radius);
	}

	.toggle:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.node-label {
		flex: 1;
		min-width: 0;
		display: block;
		text-align: left;
		border: none;
		background: none;
		color: var(--banto-text);
		padding: 0.35rem 0.5rem;
		border-radius: var(--banto-radius);
		cursor: pointer;
		font: inherit;
	}

	.node-label:hover {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.node-row.selected > .node-label {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
		font-weight: 600;
	}

	:global([data-banto-preset='glass']) .node-row.selected > .node-label {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}

	.children {
		display: flex;
		flex-direction: column;
	}
</style>
