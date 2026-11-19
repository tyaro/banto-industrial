/**
 * 最近使ったコマンド（コマンドパレット、#1147）。永続化は設定と同じ二層
 * （`settings.svelte.ts` の冒頭コメント参照）:
 * - localStorage `banto.commandPaletteRecent`: 変更のたびに同期で書く写し。
 * - `getUiSettings()`（Hub のサーバー、利用者ごと）の `commands.recent`: best-effort で
 *   書く（未ログインの失敗は握りつぶす）。ログイン後に `syncRecentFromProvider()` で
 *   読み直し、サーバーの値が写しを上書きする（`settings.syncFromProvider()` から呼ぶ）。
 *
 * `commands.ts`（goto・各種ストアを引く）から分けてあるのは、`settings.svelte.ts` が
 * 取り込みのためにこのモジュールを読んでも循環しないようにするため。値は ID の
 * JSON 配列（新しい順、最大 {@link MAX_RECENT} 件）。
 */
import { getUiSettings } from './banto/setup';

export const RECENT_KEY = 'banto.commandPaletteRecent';
/** `UiSettingsProvider` のキー（`[A-Za-z0-9._-]{1,64}`）。 */
export const RECENT_SETTING = 'commands.recent';
export const MAX_RECENT = 10;

/** JSON 配列の文字列から ID の並びを取り出す。壊れた値・配列以外は `[]`。 */
export function parseRecentIds(raw: string | null | undefined): string[] {
	if (!raw) return [];
	try {
		const parsed: unknown = JSON.parse(raw);
		return Array.isArray(parsed)
			? parsed.filter((entry): entry is string => typeof entry === 'string').slice(0, MAX_RECENT)
			: [];
	} catch {
		return [];
	}
}

export function loadRecentCommandIds(): string[] {
	if (typeof localStorage === 'undefined') return [];
	try {
		return parseRecentIds(localStorage.getItem(RECENT_KEY));
	} catch {
		return [];
	}
}

function writeLocal(ids: string[]): void {
	if (typeof localStorage === 'undefined') return;
	try {
		localStorage.setItem(RECENT_KEY, JSON.stringify(ids));
	} catch {
		// ベストエフォートの利便機能 - コマンド実行自体をブロックしない。
	}
}

export function recordRecentCommand(id: string): void {
	if (typeof localStorage === 'undefined') return;
	const next = [id, ...loadRecentCommandIds().filter((existing) => existing !== id)].slice(
		0,
		MAX_RECENT
	);
	writeLocal(next);
	// best-effort: 未認証/オフラインの失敗は想定内（写しには入っている）。
	void getUiSettings()
		.set(RECENT_SETTING, JSON.stringify(next))
		.catch(() => {});
}

/**
 * ログイン後に一度呼ぶ: サーバーの値があれば写しを上書きする。未保存（null）・
 * 壊れた値・失敗は、いまの写しのままにする（空配列でサーバーの値を消さない）。
 */
export async function syncRecentFromProvider(): Promise<void> {
	try {
		const raw = await getUiSettings().get(RECENT_SETTING);
		const ids = parseRecentIds(raw);
		if (ids.length > 0) writeLocal(ids);
	} catch {
		// Best-effort: オフライン/未認証時は現在値のまま。
	}
}
