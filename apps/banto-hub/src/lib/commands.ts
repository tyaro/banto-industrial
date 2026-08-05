/**
 * relay-wright の同名ファイルから複製。コマンドパレット（Ctrl+K）の
 * コマンド定義。navigation.ts の navItems からナビゲーションコマンドを
 * 自動導出する構造は無改変（差分は navItems 自体の中身のみ）。
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
			visible: () => !sessionStore.authDisabled,
			run: async () => {
				await getAuthProvider().logout();
				await goto('/login');
			}
		}
	];
}

/** 全パレットコマンド（ナビゲーション → テーマ → セッションの固定順）。 */
export function buildCommands(): PaletteCommand[] {
	return [...navigationCommands(), ...themeCommands(), ...sessionCommands()];
}

// --- 最近使ったコマンド（localStorage） -------------------------------------

const RECENT_KEY = 'banto.commandPaletteRecent';
const MAX_RECENT = 10;

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

export function recordRecentCommand(id: string): void {
	if (typeof localStorage === 'undefined') return;
	const next = [id, ...loadRecentCommandIds().filter((existing) => existing !== id)].slice(
		0,
		MAX_RECENT
	);
	try {
		localStorage.setItem(RECENT_KEY, JSON.stringify(next));
	} catch {
		// ベストエフォートの利便機能 - コマンド実行自体をブロックしない。
	}
}
