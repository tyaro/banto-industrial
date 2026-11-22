/**
 * 値を見張る窓（追加パネル `watch`、#1254。docs/hub/banto-hub-ui-improvement-proposal.md
 * §値を見張る窓）が持つ「見張っているタグの一覧」の純粋なロジックと保存値の形。
 * runes を使わないので単体テストできる（`watchListStore.ts` が localStorage と
 * `UiSettingsProvider` への配線を持つ。`dockLayout.ts` と `dockLayoutStore.ts` の分け方と同じ）。
 *
 * - 一覧はタグの id の配列（順序つき・重複なし・最大 {@link WATCH_LIMIT} 件）。窓は 1 つだけ。
 * - 保存は利用者ごと（サーバーは `hub.watch.v1`、localStorage の写しは持ち主ごと）。ドックの
 *   配置とは別のキーで持つ（配置の版を上げずに済む）。
 * - 一覧の変更は利用者の「操作」（{@link WatchOp}: 足す・外す・すべて外す）で表し、
 *   {@link applyWatchOps} で基準の一覧に当てる。保存の口は、PUT が成功するまで操作を捨てず、
 *   周回のたびにサーバーの最新の値へ当て直す（`watchListStore.ts` 冒頭）。
 * - 消されたタグは操作にしない（PR #1282 の再レビュー P2-2。古いタグ一覧を根拠に、別のタブで
 *   足した実在のタグを保存から消さないため）。消えたかは ID ごとの照会（`GET /api/tags/{id}` の
 *   404。ID は再利用されない）で確かめ、口は表示から外し、利用者の操作で保存するついでにだけ
 *   保存の一覧から落とす（4 回目のレビュー P2、`watchListStore.ts` 冒頭）。
 * - 購読はタグ ID で行う（WS の `subscribe` の `ids`、2026-11-22 オーナー決定。
 *   `tagValueStream.svelte.ts` 冒頭）。外部名の形・使い回しに左右されない。
 */

/** 見張れるタグの上限（2026-11-21 オーナー決定）。 */
export const WATCH_LIMIT = 100;

/** 保存の形の版。形を変えるときに上げ、前の版のキーは読まない。 */
export const WATCH_KEY_VERSION = 1;

export interface WatchKeys {
	/** localStorage の写し。持ち主が分からなければ `undefined`（写しを使わない）。 */
	local: string | undefined;
	/** `UiSettingsProvider`（Hub のサーバー、利用者ごと）のキー。本体。 */
	setting: string;
}

/** 保存先のキー。持ち主の包み方は `dockLayoutKeys` と同じ。 */
export function watchListKeys(owner: string | null): WatchKeys {
	const local =
		owner === null || owner === ''
			? undefined
			: `banto.watch.v${WATCH_KEY_VERSION}.${encodeURIComponent(owner).replace(/\./g, '%2E')}`;
	return { local, setting: `hub.watch.v${WATCH_KEY_VERSION}` };
}

export interface AddResult {
	/** 足した後の一覧。 */
	list: number[];
	/** 実際に足した件数。 */
	added: number;
	/** すでに一覧にあった（足さなかった）件数。 */
	duplicates: number;
	/** 上限を超えて足せなかった件数。 */
	overLimit: number;
}

/** 一覧の後ろへ足す。重複は足さず、上限を超える分は足さない。入力の順を保つ。 */
export function addToWatch(
	list: readonly number[],
	ids: readonly number[],
	limit: number = WATCH_LIMIT
): AddResult {
	const next = [...list];
	const have = new Set(next);
	let added = 0;
	let duplicates = 0;
	let overLimit = 0;
	for (const id of ids) {
		if (have.has(id)) {
			duplicates++;
			continue;
		}
		if (next.length >= limit) {
			overLimit++;
			continue;
		}
		next.push(id);
		have.add(id);
		added++;
	}
	return { list: next, added, duplicates, overLimit };
}

/** 1 件外す。 */
export function removeFromWatch(list: readonly number[], id: number): number[] {
	return list.filter((x) => x !== id);
}

/** 与えた id を落とす（残りの順は変えない）。 */
export function withoutIds(list: readonly number[], drop: ReadonlySet<number>): number[] {
	return drop.size === 0 ? [...list] : list.filter((id) => !drop.has(id));
}

/** id ごとの在否の照会を同時に走らせる数（{@link checkIdsConcurrently} の既定）。 */
export const TAG_CHECK_CONCURRENCY = 4;

/**
 * id ごとの照会 `checkOne` を、同時に `concurrency` 本までに絞って走らせる（PR #1282 の 4 回目の
 * レビュー P2。消えたかの判定は ID ごとの `GET /api/tags/{id}`、`watchListStore.ts` 冒頭）。
 * 結果には確定した id だけを入れる: `checkOne` が値を返せばその値、reject した id は入れない
 * （不明）。全体は reject しない。
 */
export async function checkIdsConcurrently<T>(
	ids: readonly number[],
	checkOne: (id: number) => Promise<T>,
	concurrency: number = TAG_CHECK_CONCURRENCY
): Promise<Map<number, T>> {
	const result = new Map<number, T>();
	let next = 0;
	async function worker(): Promise<void> {
		while (next < ids.length) {
			const id = ids[next++];
			try {
				result.set(id, await checkOne(id));
			} catch {
				// 不明（結果に入れない）。
			}
		}
	}
	const workers = Math.max(1, Math.min(concurrency, ids.length));
	await Promise.all(Array.from({ length: workers }, () => worker()));
	return result;
}

/**
 * 利用者の一覧への 1 回の操作。保存の口は PUT が成功するまで溜めておき、周回のたびに
 * サーバーの値へ当て直す（`watchListStore.ts`）。当て直しても同じ結果になる形だけにする
 * （足すは重複を足さない、外すは無ければ何もしない）。
 */
export type WatchOp =
	{ kind: 'add'; ids: readonly number[] } | { kind: 'remove'; id: number } | { kind: 'clear' };

/**
 * 1 つの操作を当てる。足すときの上限・重複の扱いは {@link addToWatch} と同じ。足す以外の
 * 操作の件数（`added` など）は 0。
 */
export function applyWatchOp(
	list: readonly number[],
	op: WatchOp,
	limit: number = WATCH_LIMIT
): AddResult {
	switch (op.kind) {
		case 'add':
			return addToWatch(list, op.ids, limit);
		case 'remove':
			return { list: removeFromWatch(list, op.id), added: 0, duplicates: 0, overLimit: 0 };
		case 'clear':
			return { list: [], added: 0, duplicates: 0, overLimit: 0 };
	}
}

/** 操作を順に当てた一覧（基準は書き換えない）。 */
export function applyWatchOps(
	base: readonly number[],
	ops: readonly WatchOp[],
	limit: number = WATCH_LIMIT
): number[] {
	let list: number[] = [...base];
	for (const op of ops) list = applyWatchOp(list, op, limit).list;
	return list;
}

/** 同じ一覧か（順序も含む）。 */
export function sameWatchList(a: readonly number[], b: readonly number[]): boolean {
	return a.length === b.length && a.every((id, i) => id === b[i]);
}

/**
 * 保存値（JSON の文字列）-> 一覧。未保存・壊れた値・知らない形は `undefined`。整数でない・
 * 重複する要素は落とし、上限を超える分は切る（手で書き換えられた値でも窓が壊れないように）。
 */
export function parseWatchList(raw: string | null | undefined): number[] | undefined {
	if (typeof raw !== 'string' || raw === '') return undefined;
	let value: unknown;
	try {
		value = JSON.parse(raw);
	} catch {
		return undefined;
	}
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
	const { schema, tags } = value as { schema?: unknown; tags?: unknown };
	if (schema !== 1 || !Array.isArray(tags)) return undefined;
	const ids = tags.filter((id): id is number => Number.isInteger(id));
	return addToWatch([], ids).list;
}

/** 一覧 -> 保存する文字列（同じ一覧なら同じ文字列）。 */
export function serializeWatchList(list: readonly number[]): string {
	return JSON.stringify({ schema: 1, tags: list });
}
