/**
 * 最近使ったコマンド（コマンドパレット、#1147）。localStorage は使わない（origin 共通で、
 * 認証トークンがタブごとの sessionStorage にあるので、同じ origin の別タブに別の利用者が
 * いることがある）。履歴はこのモジュールのメモリと、`getUiSettings()`（Hub のサーバー、
 * 利用者ごと）の `commands.recent` だけに持つ:
 * - サーバーへは記録のたびに best-effort で書く（未ログインの失敗は握りつぶす）。
 * - ログイン後に `syncRecentFromProvider()` で読み直し、サーバーの値をメモリに当てる
 *   （`settings.syncFromProvider()` から呼ぶ）。
 * - 以前のビルドが残した localStorage `banto.commandPaletteRecent` は取り込みの開始時に消す。
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

/** 履歴（新しい順）。取り込みで得た値と、その後の記録。 */
let recentIds: string[] = [];

export function loadRecentCommandIds(): string[] {
	return [...recentIds];
}

/** 以前のビルドが localStorage に残した写しを消す（読まない・書かない）。 */
function removeLegacyCopy(): void {
	try {
		if (typeof localStorage !== 'undefined') localStorage.removeItem(RECENT_KEY);
	} catch {
		// ストレージが使えなくても影響しない。
	}
}
removeLegacyCopy();

/**
 * 取り込み（{@link syncRecentFromProvider}）の状態。メモリの履歴は取り込みが成功する
 * までサーバーの値ではないので、サーバーへ書いてよいのは取り込みが成功した後（`ready`）だけ。
 * - `idle`: まだ取り込みを始めていない（ログイン前）。記録はメモリにだけ書く。
 * - `syncing`: 取り込み中。メモリにだけ書き、ID を `pendingIds` に控える。
 * - `ready`: 成功済み。メモリとサーバーの両方へ書く。
 * - `failed`: 失敗。メモリにだけ書く（サーバーを短い並びで上書きしない）。
 */
let syncState: 'idle' | 'syncing' | 'ready' | 'failed' = 'idle';
/** 取り込み中に記録した ID（新しい順）。 */
let pendingIds: string[] = [];
/** 取り込みの世代。新しい取り込みが始まったら、古いものの結果は捨てる。 */
let syncSeq = 0;
/** 直近に始めた取り込みのセッションの鍵（同じセッションの重なり・やり直しを見分ける）。 */
let syncSession: { key: unknown } | undefined;

function pushFront(id: string, ids: string[]): string[] {
	return [id, ...ids.filter((existing) => existing !== id)].slice(0, MAX_RECENT);
}

function writeRemote(ids: string[]): void {
	// best-effort: 未認証/オフラインの失敗は想定内（メモリには入っている）。
	void getUiSettings()
		.set(RECENT_SETTING, JSON.stringify(ids))
		.catch(() => {});
}

export function recordRecentCommand(id: string): void {
	recentIds = pushFront(id, recentIds);
	if (syncState === 'syncing') pendingIds = pushFront(id, pendingIds);
	else if (syncState === 'ready') writeRemote(recentIds);
}

/**
 * ログイン後（セッション確定後）に呼ぶ。始めた時点でメモリを空にし（前の利用者の履歴を
 * 出さない）、成功したらサーバーの値をそのままメモリに当てる（null・壊れた値・空は空）。
 * 取り込み中の記録は、サーバー値の前に足して（重複を除き最大 {@link MAX_RECENT} 件）、
 * あったときだけサーバーへ書き戻す。失敗したらメモリの記録だけを持ち、サーバーへは書かない。
 *
 * `isCurrent` は「いまも取り込みを始めたセッションか」。始める前と応答の後に確かめ、
 * 古いセッションならメモリにもサーバーにも何もしない。
 *
 * `session` はセッションの鍵（`snapshot.generation` など）。同じセッションで取り込みが
 * 重なったとき（前が `syncing`）や、失敗の後にやり直すとき（前が `failed`）は、まだサーバーへ
 * 送っていない記録を捨てずに引き継ぐ（`syncing` は `pendingIds`、`failed` はその間にメモリへ
 * 記録した `recentIds`）。鍵が違えば（利用者が替わった）すべて空にする。`ready` なら記録は
 * サーバーへ書いてあり、`get` は書き込みを待つので、空にして読み直してよい。
 * 省略すると毎回別の鍵になり、常に空にする。
 */
export async function syncRecentFromProvider(
	isCurrent: () => boolean = () => true,
	session: unknown = Symbol('recent-sync-session')
): Promise<void> {
	if (!isCurrent()) return;
	const seq = ++syncSeq;
	const carried =
		syncSession !== undefined && Object.is(syncSession.key, session)
			? syncState === 'syncing'
				? pendingIds
				: syncState === 'failed'
					? recentIds
					: []
			: [];
	syncSession = { key: session };
	syncState = 'syncing';
	pendingIds = [...carried];
	recentIds = [...carried];
	removeLegacyCopy();
	let serverIds: string[];
	try {
		serverIds = parseRecentIds(await getUiSettings().get(RECENT_SETTING));
	} catch {
		// Best-effort: 失敗したらメモリの記録だけを持つ。次のセッションの取り込みでやり直す。
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
	recentIds = merged;
	syncState = 'ready';
	if (recorded.length > 0) writeRemote(merged);
}

/** テスト用: 取り込みの状態を最初（ログイン前）に戻す。 */
export function resetRecentSyncForTest(): void {
	syncState = 'idle';
	syncSession = undefined;
	pendingIds = [];
	recentIds = [];
	syncSeq++;
}
