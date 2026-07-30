/**
 * App settings store (Svelte 5 runes).
 *
 * Persistence (spec §12.1, M12): TWO layers per setting -
 * - localStorage, written synchronously on every change. This is the FOUC
 *   cache: app.html's inline script restores `banto.theme`/`banto.preset`
 *   from it before first paint, and it is also the whole story in demo mode.
 * - the mode-matched `UiSettingsProvider` (`getUiSettings()`, setup.ts):
 *   Tauri settings DB / REST `settings` table, written fire-and-forget on
 *   every change (an unauthenticated write fails server-side and is
 *   swallowed - the local cache already has the value). Read back once per
 *   login via `syncFromProvider()` (called from the `(app)` route guard
 *   after `sessionStore.load()`), so a value saved from another
 *   client/session wins over this tab's stale localStorage.
 */
import {
	applyPreset,
	applyTheme,
	isThemeMode,
	isThemePreset,
	watchSystemTheme,
	type ThemeMode,
	type ThemePreset
} from '@banto/theme';
import { getUiSettings } from './banto/setup';

const THEME_KEY = 'banto.theme';
const PRESET_KEY = 'banto.preset';

/** `UiSettingsProvider` keys (wire contract, spec M12). */
const MODE_SETTING = 'theme.mode';
const PRESET_SETTING = 'theme.preset';

function loadThemeMode(): ThemeMode {
	if (typeof localStorage === 'undefined') return 'system';
	const stored = localStorage.getItem(THEME_KEY);
	return isThemeMode(stored) ? stored : 'system';
}

function loadThemePreset(): ThemePreset {
	if (typeof localStorage === 'undefined') return 'standard';
	const stored = localStorage.getItem(PRESET_KEY);
	return isThemePreset(stored) ? stored : 'standard';
}

/** Best-effort provider write: an unauthenticated/offline failure is expected and ignored (localStorage already holds the value). */
function persistRemote(key: string, value: string): void {
	void getUiSettings()
		.set(key, value)
		.catch(() => {});
}

class Settings {
	themeMode: ThemeMode = $state(loadThemeMode());
	themePreset: ThemePreset = $state(loadThemePreset());
	sidebarCollapsed = $state(false);

	#unwatchSystem: (() => void) | undefined;

	/** Apply + cache locally, WITHOUT the provider write (init/syncFromProvider must not echo values back). */
	#applyThemeMode(mode: ThemeMode) {
		this.themeMode = mode;
		localStorage.setItem(THEME_KEY, mode);
		applyTheme(mode);

		this.#unwatchSystem?.();
		this.#unwatchSystem = undefined;
		if (mode === 'system') {
			this.#unwatchSystem = watchSystemTheme(() => applyTheme('system'));
		}
	}

	#applyThemePreset(preset: ThemePreset) {
		this.themePreset = preset;
		localStorage.setItem(PRESET_KEY, preset);
		applyPreset(preset);
	}

	setThemeMode(mode: ThemeMode) {
		this.#applyThemeMode(mode);
		persistRemote(MODE_SETTING, mode);
	}

	setThemePreset(preset: ThemePreset) {
		this.#applyThemePreset(preset);
		persistRemote(PRESET_SETTING, preset);
	}

	/** Call once on app mount to sync the DOM and start OS-theme watching. No provider write - nothing changed yet. */
	init() {
		this.#applyThemeMode(this.themeMode);
		this.#applyThemePreset(this.themePreset);
	}

	/**
	 * Pull theme settings from the `UiSettingsProvider` and apply whatever it
	 * holds (updating the localStorage cache too). Called once per login from
	 * `routes/(app)/+layout.ts` - that's the earliest point the provider is
	 * guaranteed authenticated (`sessionStore.load()` just succeeded). A
	 * missing key (never saved) or any provider failure leaves the current
	 * (localStorage-seeded) values in place.
	 */
	async syncFromProvider(): Promise<void> {
		const ui = getUiSettings();
		try {
			const [mode, preset] = await Promise.all([ui.get(MODE_SETTING), ui.get(PRESET_SETTING)]);
			if (isThemeMode(mode)) this.#applyThemeMode(mode);
			if (isThemePreset(preset)) this.#applyThemePreset(preset);
		} catch {
			// Best-effort: offline/unauthenticated reads keep the local values.
		}
	}

	toggleSidebar() {
		this.sidebarCollapsed = !this.sidebarCollapsed;
	}
}

export const settings = new Settings();
