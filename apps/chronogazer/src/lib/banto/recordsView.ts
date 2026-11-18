/**
 * #1179 PR4b（docs/chronogazer-ui-improvement-proposal.md §2.1、2026-11-18）:
 * 「記録」ページ（`(app)/tags/+page.svelte`）の「左ツリーで選んだ対象」と
 * 「中央のタブ」を URL のクエリと行き来させる純関数。HUB のタグページの
 * `apps/banto-hub/src/lib/banto/tagsView.ts`（#1179 PR3a）と同じ形にしてある。
 *
 * URL の形（既定の値は書かない）:
 *
 * | 画面                           | クエリ                             |
 * | ------------------------------ | ---------------------------------- |
 * | すべて・記録項目               | （無し）                           |
 * | 接続先・記録項目               | `?target=connection:<id>`          |
 * | 収集グループ・設定             | `?target=group:<id>&tab=settings`  |
 *
 * - 「すべて」には「設定」タブが無い。閲覧者（`settingsAvailable: false`）にも
 *   「設定」タブは無いので、`tab=settings` は「記録項目」へ倒す。
 * - 読めない値（`target=foo`・`tab=bar`）と、読み込み済みの一覧に無い id は
 *   「すべて」・「記録項目」へ倒す（{@link resolveRecordsView} の `missingTarget`）。
 *   一覧を読み終える前は存在を判定しない（読み込み前に消すと、正しい id の
 *   ディープリンクも「すべて」になる）。
 */

/** ツリーで選んでいる対象。 */
export type RecordTarget =
	{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };

/** 中央のタブ。`list` = 「記録項目」（既定）、`settings` = 「設定」。 */
export type RecordsTab = 'list' | 'settings';

/** 画面の状態のうち URL に持つもの。 */
export interface RecordsView {
	target: RecordTarget;
	tab: RecordsTab;
}

export const RECORDS_VIEW_TARGET_PARAM = 'target';
export const RECORDS_VIEW_TAB_PARAM = 'tab';

/**
 * クエリをそのまま読んだ結果（存在の確認の前）。`null` は「値はあるが読めない」。
 * `target` が無いときは「すべて」、`tab` が無いときは「記録項目」。
 */
export interface ParsedRecordsView {
	target: RecordTarget | null;
	tab: RecordsTab | null;
}

const TARGET_PATTERN = /^(connection|group):([1-9][0-9]*)$/;

export function parseRecordsView(params: Pick<URLSearchParams, 'get'>): ParsedRecordsView {
	const rawTarget = params.get(RECORDS_VIEW_TARGET_PARAM);
	let target: RecordTarget | null;
	if (rawTarget === null) {
		target = { type: 'all' };
	} else {
		const match = TARGET_PATTERN.exec(rawTarget);
		const id = match ? Number(match[2]) : NaN;
		target =
			match && Number.isSafeInteger(id) ? { type: match[1] as 'connection' | 'group', id } : null;
	}
	const rawTab = params.get(RECORDS_VIEW_TAB_PARAM);
	const tab: RecordsTab | null =
		rawTab === null || rawTab === 'list' ? 'list' : rawTab === 'settings' ? 'settings' : null;
	return { target, tab };
}

/** URL の対象が見つからないときの案内。 */
export const MISSING_RECORD_TARGET_MESSAGE =
	'指定された接続先・収集グループが見つからないため、すべての記録項目を表示します';

export interface ResolvedRecordsView {
	view: RecordsView;
	/**
	 * URL の `target` が読めない、または読み込み済みの一覧に無かった（「すべて」へ
	 * 倒した）。削除などで今の対象が消えた場合も立つ - 案内を出すかは呼び出し側が
	 * 決める。
	 */
	missingTarget: boolean;
}

/**
 * URL の値を、読み込み済みの一覧に照らして画面の状態にする。対象が「すべて」、
 * または `settingsAvailable` が偽なら、タブは常に「記録項目」。読めない `tab` も
 * 「記録項目」。
 */
export function resolveRecordsView(
	parsed: ParsedRecordsView,
	connections: readonly { id: number }[],
	groups: readonly { id: number }[],
	options: { loaded: boolean; settingsAvailable: boolean }
): ResolvedRecordsView {
	let target: RecordTarget = parsed.target ?? { type: 'all' };
	if (options.loaded && target.type !== 'all') {
		const id = target.id;
		const list = target.type === 'connection' ? connections : groups;
		if (!list.some((item) => item.id === id)) target = { type: 'all' };
	}
	const missingTarget = parsed.target === null || target !== parsed.target;
	const tab: RecordsTab =
		target.type === 'all' || !options.settingsAvailable ? 'list' : (parsed.tab ?? 'list');
	return { view: { target, tab }, missingTarget };
}

/** 画面の状態を URL のクエリ（先頭の `?` 付き、既定だけなら空文字）にする。 */
export function recordsViewSearch(view: RecordsView): string {
	if (view.target.type === 'all') return '';
	// `URLSearchParams` は `:` を `%3A` にするので手で組む（値は英字・`:`・数字だけ）。
	const target = `?${RECORDS_VIEW_TARGET_PARAM}=${view.target.type}:${view.target.id}`;
	return view.tab === 'settings' ? `${target}&${RECORDS_VIEW_TAB_PARAM}=settings` : target;
}

export function sameRecordTarget(a: RecordTarget, b: RecordTarget): boolean {
	if (a.type === 'all' || b.type === 'all') return a.type === b.type;
	return a.type === b.type && a.id === b.id;
}

export function sameRecordsView(a: RecordsView, b: RecordsView): boolean {
	return a.tab === b.tab && sameRecordTarget(a.target, b.target);
}

/**
 * 記録項目の一覧をツリーの選択で絞る。「すべて」= 全件、接続先 = その配下の
 * 収集グループの記録項目、収集グループ = そのグループの記録項目。
 */
export function filterRecordsByTarget<T extends { collectionGroupId: number }>(
	items: readonly T[],
	target: RecordTarget,
	groups: readonly { id: number; plcConnectionId: number }[]
): T[] {
	if (target.type === 'all') return [...items];
	if (target.type === 'group') {
		const id = target.id;
		return items.filter((item) => item.collectionGroupId === id);
	}
	const connectionId = target.id;
	const groupIds = new Set(
		groups.filter((g) => g.plcConnectionId === connectionId).map((g) => g.id)
	);
	return items.filter((item) => groupIds.has(item.collectionGroupId));
}
