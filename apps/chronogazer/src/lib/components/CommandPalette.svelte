<script lang="ts">
	/**
	 * Command palette (spec M16) - the app half (banto #220 phase 3,
	 * 2026-10-08). The dialog itself (search input, grouped list, keyboard,
	 * focus trap/restore, window-level Escape, outside click, look) is
	 * `@banto/ui`'s CommandPalette (banto ADR-0018 §8, phase 2b). What stays
	 * here is what knows about this app: the command list (`#lib/commands`),
	 * admin-core's scored search, the recent history and failure
	 * notifications.
	 *
	 * Mounted by (app)/+layout.svelte only while `commandPaletteStore.open`
	 * is true (an `{#if}`), so every open gets a fresh instance - the command
	 * list and the recent-history snapshot reset for free. The Ctrl+K/Cmd+K
	 * toggle lives one level up ((app)/+layout.svelte), since it must also
	 * work to CLOSE this palette while its own input has focus.
	 */
	import { CommandPalette } from '@banto/ui';
	import { isProviderError, notify, searchCommands, type PaletteCommand } from '@banto/admin-core';
	import { buildCommands, loadRecentCommandIds, recordRecentCommand } from '#lib/commands.js';
	import { commandPaletteStore } from '#lib/commandPalette.svelte.js';

	// Built/read once per mount (i.e. once per open) - navItems is static and
	// recency only needs to reflect what was true when the palette opened.
	const commands = buildCommands();
	const recentIds = loadRecentCommandIds();

	// admin-core's scored search (prefix > word-start > substring; recent
	// first as the tie-breaker and for the empty query). Recency is an
	// ordering here, not a separate section, so the package's `recentIds`
	// prop (a "recent" heading) is not used - same list as before.
	function search(query: string, items: readonly PaletteCommand[]): PaletteCommand[] {
		return searchCommands([...items], query, recentIds);
	}

	// The package disables the rows while this runs and closes afterwards
	// (onClose below), so errors are handled here, never rethrown.
	async function execute(command: PaletteCommand): Promise<void> {
		try {
			await command.run();
		} catch (err) {
			notify('error', isProviderError(err) ? err.message : String(err));
		}
		recordRecentCommand(command.id);
	}
</script>

<CommandPalette
	open
	items={commands}
	{search}
	onExecute={execute}
	onClose={() => commandPaletteStore.hide()}
/>
