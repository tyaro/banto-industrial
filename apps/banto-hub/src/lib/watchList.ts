/**
 * 値を見張る窓（追加パネル `watch`、#1254。docs/hub/banto-hub-ui-improvement-proposal.md
 * §値を見張る窓）が持つ「見張っているタグの一覧」の純粋なロジックと保存値の形。
 * runes を使わないので単体テストできる（`watchListStore.ts` が localStorage と
 * `UiSettingsProvider` への配線を持つ。`dockLayout.ts` と `dockLayoutStore.ts` の分け方と同じ）。
 *
 * - 一覧はタグの id の配列（順序つき・重複なし・最大 {@link WATCH_LIMIT} 件）。窓は 1 つだけ。
 * - 保存は利用者ごと（サーバーは `hub.watch.v1`、localStorage の写しは持ち主ごと）。ドックの
 *   配置とは別のキーで持つ（配置の版を上げずに済む）。
 * - 一覧の変更は「操作」（{@link WatchOp}: 足す・外す・すべて外す・消えた id の除去）で表し、
 *   {@link applyWatchOps} で基準の一覧に当てる。サーバーの保存値が確定する前の操作を溜めて、
 *   確定した基準に当て直すため（端末の写しから作った一覧でサーバーの一覧を消さない。
 *   `watchListStore.ts` 冒頭）。
 * - 消されたタグの id は、サーバーの基準が確定し、タグ一覧を取得した後に `prune` の操作で落とす。
 * - 購読のパターンは見張っているタグの収集グループのワイルドカード（{@link watchSubscriptionPatterns}）。
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

/** 存在しなくなったタグの id を落とす（残りの順は変えない）。 */
export function pruneMissing(list: readonly number[], existing: ReadonlySet<number>): number[] {
	return list.filter((id) => existing.has(id));
}

/**
 * 一覧への 1 回の操作。サーバーの基準が確定する前は溜めておき、確定した基準に当て直す
 * （`watchListStore.ts`）。`prune` の `existing` はその時点のタグ一覧の id。
 */
export type WatchOp =
	| { kind: 'add'; ids: readonly number[] }
	| { kind: 'remove'; id: number }
	| { kind: 'clear' }
	| { kind: 'prune'; existing: ReadonlySet<number> };

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
		case 'prune':
			return { list: pruneMissing(list, op.existing), added: 0, duplicates: 0, overLimit: 0 };
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

/**
 * 見張りの購読のパターン: 見張っているタグの属する収集グループのワイルドカード
 * `{接続名}.{グループ名}.*` の重複なしの一覧（見張りの順に初めて出た順）。
 *
 * 外部名の具体名で送らないのは、サーバーが未知の具体名を 1 件でも含む購読を全体ごと
 * `unknown_tag` で拒否し（`core/src/stream.rs`）、ページの一覧と実行中のカタログの差
 * （作ったばかり・別の所で消した）で窓全体が止まるため。ワイルドカードは 0 件でも拒否され
 * ず、タグ名に `*` があっても関係ない。グループの外のタグの値も届くので、受けた値は見張って
 * いる外部名だけを残す（`createTagValueStream` の `accept`）。`prefixByGroupId` に無い
 * グループ（一覧の取得のずれ）のタグは外す（引けるまで値は出ない）。
 */
export function watchSubscriptionPatterns(
	tags: readonly { collectionGroupId: number }[],
	prefixByGroupId: ReadonlyMap<number, string>
): string[] {
	const patterns: string[] = [];
	const seen = new Set<string>();
	for (const tag of tags) {
		const prefix = prefixByGroupId.get(tag.collectionGroupId);
		if (prefix === undefined) continue;
		const pattern = `${prefix}.*`;
		if (seen.has(pattern)) continue;
		seen.add(pattern);
		patterns.push(pattern);
	}
	return patterns;
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
