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
 *   once the SessionController confirmed the session), so a value saved from another
 *   client/session wins over this tab's stale localStorage. The late response is
 *   guarded twice (#1215): `isCurrent` drops a response that belongs to a session that
 *   is no longer live, and a per-key edit generation keeps a value the user changed
 *   after the read began from being rolled back.
 *
 * #1159（banto v7.2.0 のメニューバー、nav-menubar-plan.md §3）: admin-template の
 * `navLayout`（サイドバー / ドック / メニューバー）と `launcherPosition`（ドックの
 * 画面の端）を写した（banto-hub と同じ）。保存先のキーは admin-template と同じ
 * （localStorage `banto.navLayout`・`banto.launcherPosition`、プロバイダ
 * `shell.navLayout`・`shell.launcherPosition`）。**ChronoGazer の既定はメニューバー**
 * （admin-template はサイドバー）。不正な保存値は `isNavLayout()` /
 * `isLauncherPosition()` が既定へ戻す。
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
import {
	isLauncherPosition,
	isNavLayout,
	LAUNCHER_POSITION_DEFAULT,
	NAV_LAYOUT_DEFAULT,
	parseLauncherPosition,
	parseNavLayout,
	type LauncherPosition,
	type NavLayout
} from './navLayout';

export type { LauncherPosition, NavLayout };

const THEME_KEY = 'banto.theme';
const PRESET_KEY = 'banto.preset';
const NAV_LAYOUT_KEY = 'banto.navLayout';
const LAUNCHER_POSITION_KEY = 'banto.launcherPosition';

/** `UiSettingsProvider` keys (wire contract, spec M12). */
const MODE_SETTING = 'theme.mode';
const PRESET_SETTING = 'theme.preset';
const NAV_LAYOUT_SETTING = 'shell.navLayout';
const LAUNCHER_POSITION_SETTING = 'shell.launcherPosition';

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

function loadNavLayout(): NavLayout {
	if (typeof localStorage === 'undefined') return NAV_LAYOUT_DEFAULT;
	return parseNavLayout(localStorage.getItem(NAV_LAYOUT_KEY));
}

function loadLauncherPosition(): LauncherPosition {
	if (typeof localStorage === 'undefined') return LAUNCHER_POSITION_DEFAULT;
	return parseLauncherPosition(localStorage.getItem(LAUNCHER_POSITION_KEY));
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
	navLayout: NavLayout = $state(loadNavLayout());
	/** ドックの画面の端（`navLayout === 'launcher'` のときだけ使う）。 */
	launcherPosition: LauncherPosition = $state(loadLauncherPosition());

	#unwatchSystem: (() => void) | undefined;
	/** Per-key edit generation (bumped by every setter): a read that started before an edit must not roll it back. */
	#edits = new Map<string, number>();

	#editGen(key: string): number {
		return this.#edits.get(key) ?? 0;
	}

	#touch(key: string) {
		this.#edits.set(key, this.#editGen(key) + 1);
	}

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

	/** ローカルへ適用 + キャッシュ（プロバイダへは書かない）。 */
	#applyNavLayout(layout: NavLayout) {
		this.navLayout = layout;
		localStorage.setItem(NAV_LAYOUT_KEY, layout);
	}

	#applyLauncherPosition(position: LauncherPosition) {
		this.launcherPosition = position;
		localStorage.setItem(LAUNCHER_POSITION_KEY, position);
	}

	setThemeMode(mode: ThemeMode) {
		this.#touch(MODE_SETTING);
		this.#applyThemeMode(mode);
		persistRemote(MODE_SETTING, mode);
	}

	setThemePreset(preset: ThemePreset) {
		this.#touch(PRESET_SETTING);
		this.#applyThemePreset(preset);
		persistRemote(PRESET_SETTING, preset);
	}

	setNavLayout(layout: NavLayout) {
		this.#touch(NAV_LAYOUT_SETTING);
		this.#applyNavLayout(layout);
		persistRemote(NAV_LAYOUT_SETTING, layout);
	}

	setLauncherPosition(position: LauncherPosition) {
		this.#touch(LAUNCHER_POSITION_SETTING);
		this.#applyLauncherPosition(position);
		persistRemote(LAUNCHER_POSITION_SETTING, position);
	}

	/** Call once on app mount to sync the DOM and start OS-theme watching. No provider write - nothing changed yet. */
	init() {
		this.#applyThemeMode(this.themeMode);
		this.#applyThemePreset(this.themePreset);
		this.#applyNavLayout(this.navLayout);
		this.#applyLauncherPosition(this.launcherPosition);
	}

	/**
	 * Pull theme settings from the `UiSettingsProvider` and apply whatever it
	 * holds (updating the localStorage cache too). Called once per login from
	 * `routes/(app)/+layout.ts` - that's the earliest point the provider is
	 * guaranteed authenticated (the controller just confirmed the session). A
	 * missing key (never saved) or any provider failure leaves the current
	 * (localStorage-seeded) values in place.
	 *
	 * Two guards against a late response (#1215):
	 * - `isCurrent`: is the session that started the read still the live one?
	 *   Checked after the response; a stale session's response is applied to nothing.
	 * - edit generation: a key the user changed after the read began is not
	 *   rolled back by the (older) value that came back.
	 */
	async syncFromProvider(isCurrent: () => boolean = () => true): Promise<void> {
		const ui = getUiSettings();
		const started = {
			mode: this.#editGen(MODE_SETTING),
			preset: this.#editGen(PRESET_SETTING),
			navLayout: this.#editGen(NAV_LAYOUT_SETTING),
			launcherPosition: this.#editGen(LAUNCHER_POSITION_SETTING)
		};
		try {
			const [mode, preset, navLayout, launcherPosition] = await Promise.all([
				ui.get(MODE_SETTING),
				ui.get(PRESET_SETTING),
				ui.get(NAV_LAYOUT_SETTING),
				ui.get(LAUNCHER_POSITION_SETTING)
			]);
			if (!isCurrent()) return;
			if (isThemeMode(mode) && this.#editGen(MODE_SETTING) === started.mode)
				this.#applyThemeMode(mode);
			if (isThemePreset(preset) && this.#editGen(PRESET_SETTING) === started.preset)
				this.#applyThemePreset(preset);
			if (isNavLayout(navLayout) && this.#editGen(NAV_LAYOUT_SETTING) === started.navLayout)
				this.#applyNavLayout(navLayout);
			if (
				isLauncherPosition(launcherPosition) &&
				this.#editGen(LAUNCHER_POSITION_SETTING) === started.launcherPosition
			)
				this.#applyLauncherPosition(launcherPosition);
		} catch {
			// Best-effort: offline/unauthenticated reads keep the local values.
		}
	}

	toggleSidebar() {
		this.sidebarCollapsed = !this.sidebarCollapsed;
	}
}

export const settings = new Settings();
