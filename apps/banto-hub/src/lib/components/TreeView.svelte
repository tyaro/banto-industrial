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
	 *
	 * T18-2e（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-G「キーボード・
	 * タッチでも使える常時表示の作成操作」）: マウス右クリックと同じ
	 * `oncontextmenu` コールバックを、キーボードの `Shift+F10`/メニュー
	 * キー（`event.key === 'ContextMenu'`）からも起動できるようにする。
	 * 座標が無いキー操作のため、押下されたノードラベル要素の直下（左下）を
	 * 疑似的な右クリック位置として渡す。
	 *
	 * T18-6c（2026-08-27 オーナー決定、TAG-UX-9 見た目刷新）: 構造だけを
	 * ここに足す - 「ルート行/子行で背景を変える段差」「枠線＋角丸コンテナに
	 * 入れて内側でスクロール」「子が0件のルートノード向けの空状態スロット」。
	 * アイコンや設定周期など banto-hub 固有の意匠は持ち込まない（呼び出し側の
	 * ConnectionTree.svelte の label/emptyState スナペットに任せる）。
	 */
	import type { Snippet } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { TreeNode } from './treeTypes';

	interface Props {
		nodes: TreeNode<T>[];
		selectedId?: string | null;
		onselect?: (node: TreeNode<T>) => void;
		ontoggle?: (node: TreeNode<T>, expanded: boolean) => void;
		oncontextmenu?: (node: TreeNode<T>, position: { x: number; y: number }) => void;
		label: Snippet<[TreeNode<T>]>;
		/**
		 * T18-6c: ルートノードの children が空配列（[]、undefined ではない）の
		 * ときだけ、その直下に呼び出し側が用意する「空状態」の行を差し込む。
		 * TreeView 自身は「グループ未登録」のような banto-hub 固有の文言を
		 * 知らない - ConnectionTree.svelte がテキストを決める。
		 */
		emptyState?: Snippet<[TreeNode<T>]>;
	}

	let {
		nodes,
		selectedId = null,
		onselect,
		ontoggle,
		oncontextmenu,
		label,
		emptyState
	}: Props = $props();

	/**
	 * 監査指摘（2026-08-08）: プレーンな `Set` は `$state()` で包んでも
	 * Svelte 5 はディーププロキシしない（配列/オブジェクトと違い
	 * Set/Map は素通し）ため、`.add()`/`.delete()` のミューテーションが
	 * リアクティビティに通知されず、展開/折りたたみをクリックしても
	 * `expanded.has(node.id)` を読むテンプレートが再評価されなかった
	 * （svelte-check では検出できない実行時バグ）。`apps/relay-wright/
	 * src/routes/(app)/tags/+page.svelte` の `selectedIds`（`SvelteSet`、
	 * add/delete のミューテーションが通知される）と同じ前例に倣い、
	 * `svelte/reactivity` の `SvelteSet` に置き換える。
	 */
	const expanded = new SvelteSet<string>();
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

	/** `Shift+F10`/メニューキーをマウス右クリックと同じ `oncontextmenu` へ変換する。 */
	function handleNodeKeydown(node: TreeNode<T>, event: KeyboardEvent): void {
		if (!oncontextmenu) return;
		const isContextMenuKey = event.key === 'ContextMenu' || (event.key === 'F10' && event.shiftKey);
		if (!isContextMenuKey) return;
		event.preventDefault();
		const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
		oncontextmenu(node, { x: rect.left, y: rect.bottom });
	}
</script>

<div class="tree-container">
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
						onkeydown={(event) => handleNodeKeydown(node, event)}
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
									onkeydown={(event) => handleNodeKeydown(child, event)}
								>
									{@render label(child)}
								</button>
							</div>
						{/each}
					</div>
				{:else if node.children && node.children.length === 0 && emptyState}
					<!--
						T18-6c: 子グループが1つも無い接続の下に淡色の案内行を出す
						（参考実装の empty-node 相当）。`node.children` が
						`undefined`（=そもそも子を持たない種類のノード。例:
						「すべて」ノード）のときは出さない - `children: []` と
						明示された場合のみ「子は0件」とみなす。
					-->
					<div class="node-row child empty">
						<span class="toggle-spacer"></span>
						<span class="node-label empty-label">{@render emptyState(node)}</span>
					</div>
				{/if}
			</div>
		{/each}
	</div>
</div>

<style>
	/*
	 * T18-6c: ツリー全体を枠線＋角丸のコンテナに収め、内側でスクロール
	 * させる。呼び出し元（SplitPane.svelte の左ペイン）は既に
	 * `overflow-y: auto` を持つが、このコンテナが利用可能高さいっぱいに
	 * 広がって自前でスクロールするため、実際にスクロールバーが出るのは
	 * 通常このコンテナ側になる。
	 */
	.tree-container {
		height: 100%;
		box-sizing: border-box;
		overflow-y: auto;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
	}

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
		border-radius: var(--banto-radius);
		/*
		 * T18-6c: 階層ごとに行の背景を変えて段差を出す。ルート行（接続/
		 * 「すべて」）はコンテナの基準面（--banto-surface）よりわずかに
		 * 沈んだ面 --banto-surface-subtle を敷き、子行（グループ）は
		 * コンテナ自体の基準面がそのまま透ける（`.node-row.child` で
		 * transparent に戻す）。新しい hex は増やさず、既存の
		 * --banto-surface 系トークンだけで表現する。
		 */
		background: var(--banto-surface-subtle);
	}

	.node-row.child {
		padding-left: 1.4rem;
		background: transparent;
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

	/*
	 * T18-6c: 「収集グループ未登録」のような空状態行。ボタンではなく
	 * 非対話なテキスト（クリックしても選択/コンテキストメニューは無い）
	 * なので、ホバー背景・カーソルは付けず、色だけ既存の
	 * --banto-text-muted に落として案内文だと分かるようにする。
	 */
	.empty-label {
		color: var(--banto-text-muted);
		font-style: italic;
		cursor: default;
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
