/**
 * App settings store (Svelte 5 runes)。ChronoGazer の同名ファイルと同じ形。
 *
 * 永続化は設定ごとに二層（#1147）:
 * - localStorage: 変更のたびに同期で書く。FOUC 防止の写しで（app.html の inline script が
 *   `banto.theme`/`banto.preset` を最初の描画の前に戻す）、ログイン前の表示もこれで決まる。
 * - `getUiSettings()`（setup.ts）= Hub のサーバー（`/api/ui-settings/{key}`、ログインした
 *   利用者ごと）: 変更のたびに best-effort で書く（未ログインの失敗は握りつぶす。写しには
 *   もう入っている）。デスクトップ版は起動ごとに origin が変わり localStorage が消えるため、
 *   本体はサーバー側。ログイン後に `syncFromProvider()`（`(app)/+layout.ts` のガードが、
 *   セッションを確かめた直後に呼ぶ）で読み直し、サーバーの値が写しを上書きする。
 *   再起動直後のログイン画面は既定のテーマで出てよい（2026-11-19 オーナー決定、
 *   docs/hub/hub-desktop-tls-design.md §7）。
 *
 * #1159（banto v7.2.0 のメニューバー、nav-menubar-plan.md §3）: admin-template の
 * `navLayout`（サイドバー / ドック / メニューバー）と `launcherPosition`（ドックの
 * 画面の端）を写した。保存先のキーは admin-template と同じ（localStorage
 * `banto.navLayout`・`banto.launcherPosition`、プロバイダ `shell.navLayout`・
 * `shell.launcherPosition`）。**Hub の既定はメニューバー**（admin-template は
 * サイドバー）。ドックの透け具合のスライダーは写していない（テーマの既定 50%）。
 * 不正な保存値は `isNavLayout()` / `isLauncherPosition()` が既定へ戻す。
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
import { syncRecentFromProvider } from './recentCommands';
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

/** `UiSettingsProvider` keys. */
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

/** Best-effort provider write: 未認証/オフラインの失敗は想定内で無視する（写しには入っている）。 */
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
	/** キーごとの編集の世代（setter のたびに増える）。読み込み中の編集を巻き戻さないため。 */
	#edits = new Map<string, number>();

	#editGen(key: string): number {
		return this.#edits.get(key) ?? 0;
	}

	#touch(key: string) {
		this.#edits.set(key, this.#editGen(key) + 1);
	}

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

	/** アプリマウント時に一度呼ぶ: DOM に適用 + OS テーマ監視を開始。 */
	init() {
		this.#applyThemeMode(this.themeMode);
		this.#applyThemePreset(this.themePreset);
		this.#applyNavLayout(this.navLayout);
		this.#applyLauncherPosition(this.launcherPosition);
	}

	/**
	 * ログイン直後に一度呼ぶ: UiSettingsProvider（サーバー）から読み直して適用する（写しも更新）。
	 * 未保存のキーや失敗は、いまの値（localStorage 由来）のままにする。
	 *
	 * 遅れて返った応答で壊さないための 2 つの守り:
	 * - `isCurrent`: 「いまも読み込みを始めたセッションか」。応答の後に確かめ、古いセッションの
	 *   応答は何にも当てない（最近のコマンドも同じ境界）。
	 * - `session`: セッションの鍵（`snapshot.generation`）。最近のコマンドの取り込みが同じ
	 *   セッションで重なっても、未送信の記録を捨てない。
	 * - 編集の世代: 読み込みの開始後に利用者が変えたキーは、遅れて返った古い値で巻き戻さない。
	 */
	async syncFromProvider(isCurrent: () => boolean = () => true, session?: unknown): Promise<void> {
		const ui = getUiSettings();
		// 最近使ったコマンド（#1147）も同じ境界で並行して取り込む。
		const recent =
			session === undefined
				? syncRecentFromProvider(isCurrent)
				: syncRecentFromProvider(isCurrent, session);
		const started = {
			mode: this.#editGen(MODE_SETTING),
			preset: this.#editGen(PRESET_SETTING),
			navLayout: this.#editGen(NAV_LAYOUT_SETTING),
			launcherPosition: this.#editGen(LAUNCHER_POSITION_SETTING)
		};
		try {
			const [modeSnap, presetSnap, navLayoutSnap, launcherPositionSnap] = await Promise.all([
				ui.get(MODE_SETTING),
				ui.get(PRESET_SETTING),
				ui.get(NAV_LAYOUT_SETTING),
				ui.get(LAUNCHER_POSITION_SETTING)
			]);
			// v8: get は { value, revision } を返す。
			const mode = modeSnap.value;
			const preset = presetSnap.value;
			const navLayout = navLayoutSnap.value;
			const launcherPosition = launcherPositionSnap.value;
			if (isCurrent()) {
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
			}
		} catch {
			// Best-effort: オフライン/未認証時は現在値のまま。
		}
		await recent;
	}

	toggleSidebar() {
		this.sidebarCollapsed = !this.sidebarCollapsed;
	}
}

export const settings = new Settings();
