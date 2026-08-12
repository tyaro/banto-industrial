<script lang="ts">
	/**
	 * 汎用部品（T18-2e、docs/banto-hub-t18-design.md「T18-2e T13-3 移管」、
	 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A/TAG-UX-G）: `TreeView.svelte`
	 * の `oncontextmenu`（マウス右クリック・`Shift+F10`/メニューキーのどちらも
	 * ここに集約済み）が渡す座標に浮かべる、ARIA `menu` パターンの小さな
	 * ポップアップ。`CommandPalette.svelte`（ウィンドウ外クリックで閉じる・
	 * `svelte:window` の `pointerdown` を使う）と `Drawer.svelte`（開いたら
	 * 最初のフォーカス可能要素へフォーカス・Esc で閉じる）の既存パターンを
	 * 踏襲する。banto-hub の具体的な型には依存しない（呼び出し側が
	 * `items`（ラベルと選択時コールバック）を渡すだけ）。
	 *
	 * 過剰実装を避け最小限にする（実装指示）: 項目は縦一列、矢印キー/Home/End
	 * で移動、Tab はメニュー内で循環させる（フォーカストラップ - 項目数が
	 * 少ないうちはこれで十分。将来項目が増えても roving tabindex のまま
	 * 破綻しない）。サブメニュー・アイコン等は持たない。
	 */
	interface ContextMenuItem {
		id: string;
		label: string;
		onSelect: () => void;
	}

	interface Props {
		/** 右クリック位置 / キーボードトリガ時はトリガ要素の直下（クライアント座標）。 */
		x: number;
		y: number;
		items: ContextMenuItem[];
		onClose: () => void;
	}

	let { x, y, items, onClose }: Props = $props();

	let menuEl: HTMLDivElement | undefined = $state();
	let itemEls: (HTMLButtonElement | undefined)[] = [];
	let activeIndex = $state(0);

	/**
	 * ビューポート右端/下端からのはみ出しだけ最小限に補正する（凝った
	 * 衝突回避アルゴリズムは持たない - 実装指示「過剰実装は避け最小限」）。
	 */
	function clampPosition(node: HTMLElement): void {
		const rect = node.getBoundingClientRect();
		const overflowX = rect.right - window.innerWidth;
		const overflowY = rect.bottom - window.innerHeight;
		if (overflowX > 0) node.style.left = `${Math.max(0, x - overflowX)}px`;
		if (overflowY > 0) node.style.top = `${Math.max(0, y - overflowY)}px`;
	}

	function focusFirst(node: HTMLDivElement): void {
		menuEl = node;
		clampPosition(node);
		itemEls[0]?.focus();
	}

	function focusIndex(index: number): void {
		const count = items.length;
		if (count === 0) return;
		activeIndex = ((index % count) + count) % count;
		itemEls[activeIndex]?.focus();
	}

	function activate(item: ContextMenuItem): void {
		item.onSelect();
		onClose();
	}

	function handleKeydown(event: KeyboardEvent): void {
		switch (event.key) {
			case 'Escape':
				event.preventDefault();
				onClose();
				break;
			case 'ArrowDown':
				event.preventDefault();
				focusIndex(activeIndex + 1);
				break;
			case 'ArrowUp':
				event.preventDefault();
				focusIndex(activeIndex - 1);
				break;
			case 'Home':
				event.preventDefault();
				focusIndex(0);
				break;
			case 'End':
				event.preventDefault();
				focusIndex(items.length - 1);
				break;
			case 'Tab':
				// フォーカストラップ: メニュー外へ Tab で抜けさせない（Shift+Tab は逆方向）。
				event.preventDefault();
				focusIndex(activeIndex + (event.shiftKey ? -1 : 1));
				break;
			case 'Enter':
			case ' ':
				event.preventDefault();
				if (items[activeIndex]) activate(items[activeIndex]);
				break;
		}
	}

	function handleWindowPointerDown(event: PointerEvent): void {
		if (menuEl && event.target instanceof Node && !menuEl.contains(event.target)) {
			onClose();
		}
	}
</script>

<svelte:window onpointerdown={handleWindowPointerDown} />

<div
	class="context-menu"
	role="menu"
	tabindex="-1"
	aria-label="作成メニュー"
	style:left={`${x}px`}
	style:top={`${y}px`}
	use:focusFirst
	onkeydown={handleKeydown}
>
	{#each items as item, i (item.id)}
		<button
			type="button"
			role="menuitem"
			tabindex={i === activeIndex ? 0 : -1}
			bind:this={itemEls[i]}
			onmouseenter={() => (activeIndex = i)}
			onclick={() => activate(item)}
		>
			{item.label}
		</button>
	{/each}
</div>

<style>
	.context-menu {
		position: fixed;
		z-index: 1000;
		display: flex;
		flex-direction: column;
		min-width: 200px;
		max-width: min(320px, calc(100vw - 1rem));
		padding: 0.3rem;
		background: var(--banto-surface-raised, var(--banto-surface));
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		box-shadow: 0 8px 28px rgba(0, 0, 0, 0.28);
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.context-menu button {
		display: block;
		width: 100%;
		box-sizing: border-box;
		padding: 0.5rem 0.65rem;
		border: none;
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text);
		font-size: 0.85rem;
		text-align: left;
		cursor: pointer;
	}

	.context-menu button:hover,
	.context-menu button:focus-visible {
		background: color-mix(in srgb, var(--banto-primary) 12%, transparent);
		color: var(--banto-primary);
		outline: none;
	}

	:global([data-banto-preset='glass']) .context-menu button:hover,
	:global([data-banto-preset='glass']) .context-menu button:focus-visible {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}
</style>
