<script lang="ts">
	/**
	 * Command palette modal (spec M16). Mounted by (app)/+layout.svelte only
	 * while `commandPaletteStore.open` is true (an `{#if}`, not a persistent
	 * `display: none`) so every open gets a fresh instance - query, selection,
	 * and the `recentIds` snapshot all reset for free, and `onMount` autofocus
	 * fires on every open, not just the first.
	 *
	 * Click-outside/Escape follow the same window-listener pattern as
	 * grid-svelte's FilterPopover.svelte; the Ctrl+K/Cmd+K toggle itself lives
	 * one level up ((app)/+layout.svelte), since it must also work to CLOSE
	 * this palette while its own input has focus.
	 */
	import { onMount } from 'svelte';
	import { isProviderError, notify, searchCommands, type PaletteCommand } from '@banto/admin-core';
	import { buildCommands, loadRecentCommandIds, recordRecentCommand } from '$lib/commands';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';

	// Built/read once per mount (i.e. once per open) - navItems is static and
	// recency only needs to reflect what was true when the palette opened.
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

	// Cluster the already-scored/sorted flat results by group, preserving
	// each group's first-appearance position, so commands from the same
	// group (「ナビゲーション」/「テーマ」/「セッション」) stay under one
	// heading instead of interleaving by score (spec M16: グループ見出し表示).
	// `index` here is the flattened, group-clustered position - the same
	// numbering used for keyboard selection below.
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

	// A fresh query means a fresh result set - keep selection pinned to the
	// top rather than pointing at whatever now occupies the old index.
	$effect(() => {
		// Bare read registers `query` as this effect's reactive dependency;
		// nothing else in the body reads it.
		// eslint-disable-next-line @typescript-eslint/no-unused-expressions
		query;
		selectedIndex = 0;
	});

	onMount(() => {
		inputEl?.focus();
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
			case 'Escape':
				event.preventDefault();
				commandPaletteStore.hide();
				break;
		}
	}

	function handleWindowPointerDown(event: PointerEvent): void {
		if (paletteEl && event.target instanceof Node && !paletteEl.contains(event.target)) {
			commandPaletteStore.hide();
		}
	}
</script>

<svelte:window onpointerdown={handleWindowPointerDown} />

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
		/* Glass preset (spec M12): no-op under standard (--banto-backdrop: none). */
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
