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

/**
 * 取り込み（{@link syncRecentFromProvider}）の状態。写しはセッションをまたぐ値として
 * 信用しない（localStorage は利用者で共有される）ので、サーバーへ書いてよいのは
 * 取り込みが成功した後（`ready`）だけ。
 * - `idle`: まだ取り込みを始めていない（ログイン前）。記録は写しにだけ書く。
 * - `syncing`: 取り込み中。写しにだけ書き、ID を `pendingIds` に控える。
 * - `ready`: 成功済み。写しとサーバーの両方へ書く。
 * - `failed`: 失敗。写しにだけ書く（サーバーを短い並びで上書きしない）。
 */
let syncState: 'idle' | 'syncing' | 'ready' | 'failed' = 'idle';
/** 取り込み中に記録した ID（新しい順）。 */
let pendingIds: string[] = [];
/** 取り込みの世代。新しい取り込みが始まったら、古いものの結果は捨てる。 */
let syncSeq = 0;

function pushFront(id: string, ids: string[]): string[] {
	return [id, ...ids.filter((existing) => existing !== id)].slice(0, MAX_RECENT);
}

function writeRemote(ids: string[]): void {
	// best-effort: 未認証/オフラインの失敗は想定内（写しには入っている）。
	void getUiSettings()
		.set(RECENT_SETTING, JSON.stringify(ids))
		.catch(() => {});
}

export function recordRecentCommand(id: string): void {
	if (typeof localStorage === 'undefined') return;
	const next = pushFront(id, loadRecentCommandIds());
	writeLocal(next);
	if (syncState === 'syncing') pendingIds = pushFront(id, pendingIds);
	else if (syncState === 'ready') writeRemote(next);
}

/**
 * ログイン後（セッション確定後）に呼ぶ。始めた時点で写しを空にし（前の利用者の履歴を
 * 出さない）、成功したらサーバーの値をそのまま写しに当てる（null・壊れた値・空は空）。
 * 取り込み中の記録は、サーバー値の前に足して（重複を除き最大 {@link MAX_RECENT} 件）、
 * あったときだけサーバーへ書き戻す。失敗したら写しの記録だけを持ち、サーバーへは書かない。
 *
 * `isCurrent` は「いまも取り込みを始めたセッションか」。始める前と応答の後に確かめ、
 * 古いセッションなら写しにもサーバーにも何もしない。
 */
export async function syncRecentFromProvider(isCurrent: () => boolean = () => true): Promise<void> {
	if (!isCurrent()) return;
	const seq = ++syncSeq;
	syncState = 'syncing';
	pendingIds = [];
	writeLocal([]);
	let serverIds: string[];
	try {
		serverIds = parseRecentIds(await getUiSettings().get(RECENT_SETTING));
	} catch {
		// Best-effort: 失敗したら写しの記録だけを持つ。次のセッションの取り込みでやり直す。
		if (seq === syncSeq && isCurrent()) syncState = 'failed';
		return;
	}
	if (seq !== syncSeq || !isCurrent()) return;
	const recorded = pendingIds;
	pendingIds = [];
	const merged = [...recorded, ...serverIds.filter((id) => !recorded.includes(id))].slice(
		0,
		MAX_RECENT
	);
	writeLocal(merged);
	syncState = 'ready';
	if (recorded.length > 0) writeRemote(merged);
}

/** テスト用: 取り込みの状態を最初（ログイン前）に戻す。 */
export function resetRecentSyncForTest(): void {
	syncState = 'idle';
	pendingIds = [];
	syncSeq++;
}
