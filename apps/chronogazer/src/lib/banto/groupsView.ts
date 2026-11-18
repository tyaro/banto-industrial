/**
 * #1179 PR6a（docs/chronogazer-ui-improvement-proposal.md §2.1・§4、2026-11-18）:
 * 「表示グループ」ページ（`(app)/groups/+page.svelte`）の「左の一覧で選んだ
 * 表示グループ」と「中央のタブ」を URL のクエリと行き来させる純関数。
 * 「記録」ページの `recordsView.ts` と同じ形にしてある。
 *
 * URL の形（既定の値は書かない）:
 *
 * | 画面                       | クエリ                         |
 * | -------------------------- | ------------------------------ |
 * | 未選択                     | （無し）                       |
 * | グループ・「ペン」         | `?group=<id>`                  |
 * | グループ・「設定」         | `?group=<id>&tab=settings`     |
 * | 新規作成中（編集者以上）   | `?group=new&tab=settings`      |
 *
 * - 読めない値（`group=foo`・`tab=bar`）と、読み込み済みの一覧に無い id は
 *   「未選択」・「ペン」へ倒す（{@link resolveGroupsView} の `missingGroup`）。
 *   一覧を読み終える前は存在を判定しない（読み込み前に消すと、正しい id の
 *   ディープリンクも未選択になる）。
 * - 閲覧者（`canCreate: false`）は新規作成できないので、`group=new` は未選択へ倒す。
 * - 新規作成の中身（下書き）は URL に持たない。再読み込みすると空の下書きに戻る。
 */

/** 選んでいる表示グループ。`'new'` = 新規作成中、`null` = 未選択。 */
export type GroupsSelection = number | 'new' | null;

/** 中央のタブ。`pens` = 「ペン」（既定）、`settings` = 「設定」。 */
export type GroupsTab = 'pens' | 'settings';

/** 画面の状態のうち URL に持つもの。 */
export interface GroupsView {
	group: GroupsSelection;
	tab: GroupsTab;
}

export const GROUPS_VIEW_GROUP_PARAM = 'group';
export const GROUPS_VIEW_TAB_PARAM = 'tab';
export const NEW_GROUP_PARAM_VALUE = 'new';

/**
 * クエリをそのまま読んだ結果（存在の確認の前）。`group` が `undefined` は
 * 「値はあるが読めない」。`group` が無いときは未選択（`null`）、`tab` が無い
 * ときは「ペン」。
 */
export interface ParsedGroupsView {
	group: GroupsSelection | undefined;
	tab: GroupsTab | null;
}

const GROUP_ID_PATTERN = /^[1-9][0-9]*$/;

export function parseGroupsView(params: Pick<URLSearchParams, 'get'>): ParsedGroupsView {
	const rawGroup = params.get(GROUPS_VIEW_GROUP_PARAM);
	let group: GroupsSelection | undefined;
	if (rawGroup === null) {
		group = null;
	} else if (rawGroup === NEW_GROUP_PARAM_VALUE) {
		group = 'new';
	} else {
		const id = GROUP_ID_PATTERN.test(rawGroup) ? Number(rawGroup) : NaN;
		group = Number.isSafeInteger(id) ? id : undefined;
	}
	const rawTab = params.get(GROUPS_VIEW_TAB_PARAM);
	const tab: GroupsTab | null =
		rawTab === null || rawTab === 'pens' ? 'pens' : rawTab === 'settings' ? 'settings' : null;
	return { group, tab };
}

/** URL のグループが見つからないときの案内。 */
export const MISSING_GROUP_MESSAGE = '指定された表示グループが見つかりません';

export interface ResolvedGroupsView {
	view: GroupsView;
	/**
	 * URL の `group` が読めない、または読み込み済みの一覧に無かった（未選択へ
	 * 倒した）。削除などで今のグループが消えた場合も立つ - 案内を出すかは呼び出し側が決める。
	 */
	missingGroup: boolean;
}

/**
 * URL の値を、読み込み済みの一覧に照らして画面の状態にする。未選択のときの
 * タブは常に「ペン」。読めない `tab` も「ペン」。
 */
export function resolveGroupsView(
	parsed: ParsedGroupsView,
	groups: readonly { id: number }[],
	options: { loaded: boolean; canCreate: boolean }
): ResolvedGroupsView {
	let group: GroupsSelection = parsed.group ?? null;
	let missingGroup = parsed.group === undefined;
	if (group === 'new' && !options.canCreate) {
		group = null;
		missingGroup = true;
	} else if (typeof group === 'number' && options.loaded) {
		const id = group;
		if (!groups.some((item) => item.id === id)) {
			group = null;
			missingGroup = true;
		}
	}
	const tab: GroupsTab = group === null ? 'pens' : (parsed.tab ?? 'pens');
	return { view: { group, tab }, missingGroup };
}

/** 画面の状態を URL のクエリ（先頭の `?` 付き、既定だけなら空文字）にする。 */
export function groupsViewSearch(view: GroupsView): string {
	if (view.group === null) return '';
	const value = view.group === 'new' ? NEW_GROUP_PARAM_VALUE : String(view.group);
	const group = `?${GROUPS_VIEW_GROUP_PARAM}=${value}`;
	return view.tab === 'settings' ? `${group}&${GROUPS_VIEW_TAB_PARAM}=settings` : group;
}

export function sameGroupsView(a: GroupsView, b: GroupsView): boolean {
	return a.group === b.group && a.tab === b.tab;
}
