/**
 * relay-wright の同名ファイルから複製。永続化の二層構成（localStorage の
 * FOUC キャッシュ + UiSettingsProvider）はそのまま — banto-hub では
 * `getUiSettings()` が `createLocalUiSettings()`（$lib/banto/setup.ts 参照:
 * バックエンドに /api/ui-settings が無いため）に固定されるので、
 * `persistRemote`/`syncFromProvider` は実質 localStorage への二重書き込み
 * になるが、将来サーバー側 UiSettingsProvider を追加したときに無改変で
 * 効くようこのまま残す。
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

/** `UiSettingsProvider` keys. */
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

/** Best-effort provider write: 未認証/オフラインの失敗は想定内で無視する。 */
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

	/** アプリマウント時に一度呼ぶ: DOM に適用 + OS テーマ監視を開始。 */
	init() {
		this.#applyThemeMode(this.themeMode);
		this.#applyThemePreset(this.themePreset);
	}

	/** ログイン直後に一度呼ぶ: UiSettingsProvider から読み直す。 */
	async syncFromProvider(): Promise<void> {
		const ui = getUiSettings();
		try {
			const [mode, preset] = await Promise.all([ui.get(MODE_SETTING), ui.get(PRESET_SETTING)]);
			if (isThemeMode(mode)) this.#applyThemeMode(mode);
			if (isThemePreset(preset)) this.#applyThemePreset(preset);
		} catch {
			// Best-effort: オフライン/未認証時は現在値のまま。
		}
	}

	toggleSidebar() {
		this.sidebarCollapsed = !this.sidebarCollapsed;
	}
}

export const settings = new Settings();
