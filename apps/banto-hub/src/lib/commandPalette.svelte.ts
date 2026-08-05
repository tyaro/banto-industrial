/**
 * relay-wright の同名ファイルから無改変で複製。コマンドパレットの
 * 開閉状態（Svelte 5 runes）。
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
