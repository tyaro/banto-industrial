/**
 * Command palette open/close state (Svelte 5 runes), spec M16. A tiny module
 * singleton - same pattern as `toast.svelte.ts` - so both the Ctrl+K listener
 * ((app)/+layout.svelte) and the header's search button (Header.svelte) can
 * toggle the same `CommandPalette` instance without prop drilling.
 */
class CommandPaletteStore {
	open = $state(false);

	toggle(): void {
		this.open = !this.open;
	}

	show(): void {
		this.open = true;
	}

	hide(): void {
		this.open = false;
	}
}

export const commandPaletteStore = new CommandPaletteStore();
