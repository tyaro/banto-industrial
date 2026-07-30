/**
 * Command palette (Ctrl+K / Cmd+K) command definitions (spec M16).
 *
 * `buildCommands()` derives one navigation command per `navItems` entry
 * (navigation.ts) - a page just needs an entry there to show up in the
 * palette too, no separate registration step - plus hand-written theme
 * (M12 settings API) and session (logout) commands. RBAC gating for nav
 * entries mirrors Sidebar.svelte's `visibleItems` condition exactly.
 *
 * Recency (spec M16: "最近使ったコマンドの並び上げ（localStorage）") is a
 * flat localStorage list of ids, same style as `banto.theme`/`banto.preset`
 * in app.html - deliberately NOT going through `UiSettingsProvider`
 * (non-scope: "履歴の設定DB保存"), so it stays a per-device convenience with
 * zero server round-trip.
 */
import { goto } from '$app/navigation';
import { getAuthProvider, type PaletteCommand } from '@banto/admin-core';
import { navItems } from './navigation';
import { settings } from './settings.svelte';
import { sessionStore } from './session.svelte';
import { isAdmin } from './permissions';

function navigationCommands(): PaletteCommand[] {
	return navItems.map((item) => ({
		id: `nav.${item.path}`,
		title: item.label,
		group: 'ナビゲーション',
		keywords: [item.path],
		// Spec M10 RBAC: same condition as Sidebar.svelte's `visibleItems`
		// (adminOnly entries hidden from non-admin roles).
		visible: item.adminOnly ? () => isAdmin(sessionStore.role) : undefined,
		run: () => {
			void goto(item.path);
		}
	}));
}

const THEME_GROUP = 'テーマ';

function themeCommands(): PaletteCommand[] {
	return [
		{
			id: 'theme.mode.light',
			title: 'ライトテーマにする',
			group: THEME_GROUP,
			keywords: ['light', 'theme', '明るい'],
			run: () => settings.setThemeMode('light')
		},
		{
			id: 'theme.mode.dark',
			title: 'ダークテーマにする',
			group: THEME_GROUP,
			keywords: ['dark', 'theme', '暗い'],
			run: () => settings.setThemeMode('dark')
		},
		{
			id: 'theme.mode.system',
			title: 'テーマをシステムに従う',
			group: THEME_GROUP,
			keywords: ['system', 'theme'],
			run: () => settings.setThemeMode('system')
		},
		{
			id: 'theme.preset.standard',
			title: 'スタンダードプリセットにする',
			group: THEME_GROUP,
			keywords: ['standard', 'preset'],
			run: () => settings.setThemePreset('standard')
		},
		{
			id: 'theme.preset.glass',
			title: 'ガラスプリセットにする',
			group: THEME_GROUP,
			keywords: ['glass', 'preset'],
			run: () => settings.setThemePreset('glass')
		}
	];
}

function sessionCommands(): PaletteCommand[] {
	return [
		{
			id: 'session.logout',
			title: 'ログアウト',
			group: 'セッション',
			keywords: ['logout', 'sign out'],
			// Same condition as Header.svelte's logout button: hidden in
			// login-not-required mode (spec M11 - there's no session to end).
			visible: () => !sessionStore.authDisabled,
			run: async () => {
				await getAuthProvider().logout();
				await goto('/login');
			}
		}
	];
}

/** All palette commands, in a fixed order (navigation, then theme, then session) - `searchCommands` re-sorts/filters this for display. */
export function buildCommands(): PaletteCommand[] {
	return [...navigationCommands(), ...themeCommands(), ...sessionCommands()];
}

// --- Recency (localStorage) -------------------------------------------------

const RECENT_KEY = 'banto.commandPaletteRecent';
const MAX_RECENT = 10;

/** Ordered most-recent-first; empty/corrupt storage yields `[]` rather than throwing. */
export function loadRecentCommandIds(): string[] {
	if (typeof localStorage === 'undefined') return [];
	try {
		const raw = localStorage.getItem(RECENT_KEY);
		if (!raw) return [];
		const parsed: unknown = JSON.parse(raw);
		return Array.isArray(parsed)
			? parsed.filter((entry): entry is string => typeof entry === 'string')
			: [];
	} catch {
		return [];
	}
}

/** Move `id` to the front (or insert it), capped at `MAX_RECENT`. Best-effort - a full/disabled localStorage is silently ignored. */
export function recordRecentCommand(id: string): void {
	if (typeof localStorage === 'undefined') return;
	const next = [id, ...loadRecentCommandIds().filter((existing) => existing !== id)].slice(
		0,
		MAX_RECENT
	);
	try {
		localStorage.setItem(RECENT_KEY, JSON.stringify(next));
	} catch {
		// Best-effort convenience feature - never block command execution on it.
	}
}
