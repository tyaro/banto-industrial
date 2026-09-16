<script lang="ts">
	// relay-wright の同名コンポーネントから無改変で複製。
	import { onMount } from 'svelte';
	import { isProviderError, notify, searchCommands, type PaletteCommand } from '@banto/admin-core';
	import { buildCommands, loadRecentCommandIds, recordRecentCommand } from '$lib/commands';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';
	import { handleTrapKeydown } from './focusTrap';
	import { restoreFocus } from './focusRestore';

	const commands = buildCommands();
	const recentIds = loadRecentCommandIds();

	let query = $state('');
	let selectedIndex = $state(0);
	let executing = $state(false);
	let inputEl: HTMLInputElement | undefined = $state();
	let paletteEl: HTMLDivElement | undefined = $state();

	const flatResults = $derived(searchCommands(commands, query, recentIds));

	interface DisplayItem {
		command: PaletteCommand;
		index: number;
	}
	interface DisplayGroup {
		group: string;
		items: DisplayItem[];
	}

	const displayGroups = $derived.by((): DisplayGroup[] => {
		const groupOrder: string[] = [];
		const byGroup = new Map<string, PaletteCommand[]>();
		for (const command of flatResults) {
			if (!byGroup.has(command.group)) {
				byGroup.set(command.group, []);
				groupOrder.push(command.group);
			}
			byGroup.get(command.group)!.push(command);
		}
		let index = 0;
		return groupOrder.map((group) => ({
			group,
			items: byGroup.get(group)!.map((command) => ({ command, index: index++ }))
		}));
	});

	const orderedCommands = $derived(
		displayGroups.flatMap((g) => g.items.map((item) => item.command))
	);
	const selectedCommand = $derived(orderedCommands[selectedIndex]);

	$effect(() => {
		// eslint-disable-next-line @typescript-eslint/no-unused-expressions
		query;
		selectedIndex = 0;
	});

	/**
	 * #381 レビュー対応9回目: パレットを開く前にフォーカスがあった要素。閉じる
	 * ときにここへ戻す（層の約束・項目6、`escLayering.ts`）。`Ctrl+K` は Drawer や
	 * コンテキストメニューの中からでも効くので、戻さないとフォーカスが
	 * `<body>` に落ち、**次の Tab が Drawer のトラップをすり抜ける**（body 起点の
	 * Tab はパネルの keydown を通らない）。`$state` にしない（描画に使わない）。
	 */
	let triggerEl: HTMLElement | null = null;

	onMount(() => {
		const active = document.activeElement;
		triggerEl = active instanceof HTMLElement ? active : null;
		inputEl?.focus();
		// アンマウント時（＝閉じたとき）に開いた元へ戻す。戻り先が消えている /
		// `inert` の中なら何もしない（`focusRestore.ts` - `<body>` へは落とさない）。
		return () => {
			const previous = triggerEl;
			triggerEl = null;
			restoreFocus(previous);
		};
	});

	/**
	 * #381 レビュー対応9回目: `aria-modal="true"` を名乗る層は必ずフォーカス
	 * トラップを持つ（層の約束・項目4）。`Drawer.svelte`/`Modal.svelte` と同じ
	 * 張り方（`focusTrap.ts`）。この部品は開いている間だけマウントされる。
	 */
	$effect(() => {
		const node = paletteEl;
		if (!node) return;
		const onKeydown = (event: KeyboardEvent): void => handleTrapKeydown(node, event);
		node.addEventListener('keydown', onKeydown);
		return () => node.removeEventListener('keydown', onKeydown);
	});

	function clampIndex(next: number): number {
		const count = orderedCommands.length;
		if (count === 0) return 0;
		return ((next % count) + count) % count;
	}

	async function executeCommand(command: PaletteCommand): Promise<void> {
		executing = true;
		try {
			await command.run();
		} catch (err) {
			notify('error', isProviderError(err) ? err.message : String(err));
		} finally {
			executing = false;
		}
		recordRecentCommand(command.id);
		commandPaletteStore.hide();
	}

	function handleKeydown(event: KeyboardEvent): void {
		switch (event.key) {
			case 'ArrowDown':
				event.preventDefault();
				selectedIndex = clampIndex(selectedIndex + 1);
				break;
			case 'ArrowUp':
				event.preventDefault();
				selectedIndex = clampIndex(selectedIndex - 1);
				break;
			case 'Enter':
				event.preventDefault();
				if (selectedCommand) void executeCommand(selectedCommand);
				break;
			// Escape は**window 側**（`handleWindowKeydown`）で処理する -
			// 層の約束（`escLayering.ts` の doc、項目3）。ここ（検索 input の
			// `onkeydown`）だけで閉じていると、フォーカスがパレットの外へ出た
			// 状態（Shift+Tab 等。この部品はフォーカストラップを持たない）の
			// Esc でパレットが閉じず、かつ下の層はみな「可視な上位層がある」と
			// 見て譲るので、**Esc が何も閉じない**状態になる。
		}
	}

	/**
	 * #381 レビュー対応6回目: フォーカス位置に依存しない Esc（層の約束・項目3、
	 * `escLayering.ts`）。閉じるときは `preventDefault` して下の層へ伝える
	 * （項目1）ところは `Drawer.svelte`/`Modal.svelte` と同じ。
	 *
	 * **譲る相手は見ない**（`hasVisibleLayerAbove` を使わない）: パレットは
	 * この app の**最上位層**（z-index 1000。同じ 1000 の `TreeContextMenu` は
	 * パレットの外側クリックで閉じるため同時に開かない）なので、自分より手前の
	 * 層が存在しない。`hasVisibleLayerAbove({ except: 自分 })` は「自分以外の
	 * 可視な層」しか見ないので、**下にある Drawer を「手前の層」と誤認して譲り、
	 * Esc で何も閉じなくなる**（E2E で実測）。パレットより手前に出る UI を将来
	 * 足すなら、ここに上下関係の判定を入れること。
	 *
	 * この部品は `commandPaletteStore.open` のときだけマウントされるので
	 * `open` の判定は不要。
	 */
	function handleWindowKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Escape' || event.defaultPrevented) return;
		event.preventDefault();
		commandPaletteStore.hide();
	}

	function handleWindowPointerDown(event: PointerEvent): void {
		if (paletteEl && event.target instanceof Node && !paletteEl.contains(event.target)) {
			commandPaletteStore.hide();
		}
	}
</script>

<svelte:window onpointerdown={handleWindowPointerDown} onkeydown={handleWindowKeydown} />

<div class="overlay">
	<div
		class="palette"
		role="dialog"
		aria-modal="true"
		aria-label="コマンドパレット"
		bind:this={paletteEl}
	>
		<input
			type="text"
			class="search"
			placeholder="コマンドを検索…"
			autocomplete="off"
			spellcheck="false"
			role="combobox"
			aria-expanded="true"
			aria-controls="command-palette-list"
			aria-activedescendant={selectedCommand
				? `command-palette-item-${selectedCommand.id}`
				: undefined}
			bind:value={query}
			bind:this={inputEl}
			onkeydown={handleKeydown}
		/>

		<div class="results" id="command-palette-list" role="listbox" aria-label="コマンド一覧">
			{#if orderedCommands.length === 0}
				<p class="empty">一致するコマンドがありません</p>
			{/if}
			{#each displayGroups as group (group.group)}
				<div class="group-heading">{group.group}</div>
				{#each group.items as item (item.command.id)}
					<button
						id={`command-palette-item-${item.command.id}`}
						type="button"
						class="result"
						class:selected={item.index === selectedIndex}
						role="option"
						aria-selected={item.index === selectedIndex}
						disabled={executing}
						onmouseenter={() => (selectedIndex = item.index)}
						onclick={() => executeCommand(item.command)}
					>
						{item.command.title}
					</button>
				{/each}
			{/each}
		</div>
	</div>
</div>

<style>
	.overlay {
		position: fixed;
		inset: 0;
		z-index: 1000;
		display: flex;
		justify-content: center;
		align-items: flex-start;
		padding-top: 12vh;
		background: rgba(0, 0, 0, 0.35);
	}

	.palette {
		display: flex;
		flex-direction: column;
		width: min(560px, calc(100vw - 2rem));
		max-height: min(60vh, 480px);
		background: var(--banto-surface-raised, var(--banto-surface));
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3);
		overflow: hidden;
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.search {
		flex: 0 0 auto;
		width: 100%;
		box-sizing: border-box;
		padding: 0.9rem 1rem;
		border: none;
		border-bottom: 1px solid var(--banto-border);
		background: transparent;
		color: var(--banto-text);
		font-size: 1rem;
	}

	.search:focus {
		outline: none;
	}

	.results {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 0.4rem;
	}

	.empty {
		margin: 0;
		padding: 1rem;
		text-align: center;
		color: var(--banto-text-muted);
		font-size: 0.85rem;
	}

	.group-heading {
		padding: 0.5rem 0.6rem 0.25rem;
		color: var(--banto-text-muted);
		font-size: 0.7rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.result {
		display: block;
		width: 100%;
		box-sizing: border-box;
		padding: 0.55rem 0.7rem;
		border: none;
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text);
		font-size: 0.875rem;
		text-align: left;
		cursor: pointer;
	}

	.result:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.result.selected {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
	}

	:global([data-banto-preset='glass']) .result.selected {
		background: var(--banto-accent-gradient);
		color: var(--banto-text-inverse);
	}
</style>
