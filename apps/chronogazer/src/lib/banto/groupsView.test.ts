import { describe, expect, it } from 'vitest';
import {
	groupsViewSearch,
	parseGroupsView,
	resolveGroupsView,
	sameGroupsView,
	type GroupsView
} from './groupsView.js';

const groups = [{ id: 3 }, { id: 8 }];
const writer = { loaded: true, canCreate: true };

function resolve(search: string, options = writer) {
	return resolveGroupsView(parseGroupsView(new URLSearchParams(search)), groups, options);
}

describe('parseGroupsView / resolveGroupsView', () => {
	it('クエリが無ければ未選択・「ペン」', () => {
		const r = resolve('');
		expect(r.view).toEqual({ group: null, tab: 'pens' });
		expect(r.missingGroup).toBe(false);
	});

	it('group と tab を読む', () => {
		expect(resolve('?group=8').view).toEqual({ group: 8, tab: 'pens' });
		expect(resolve('?group=8&tab=settings').view).toEqual({ group: 8, tab: 'settings' });
		expect(resolve('?group=8&tab=pens').view).toEqual({ group: 8, tab: 'pens' });
	});

	it('読めない group は未選択へ倒し、missingGroup を立てる', () => {
		for (const q of ['?group=foo', '?group=0', '?group=-1', '?group=1.5', '?group=']) {
			const r = resolve(q);
			expect(r.view, q).toEqual({ group: null, tab: 'pens' });
			expect(r.missingGroup, q).toBe(true);
		}
	});

	it('一覧に無い id は未選択へ倒す（読み込み前は判定しない）', () => {
		const missing = resolve('?group=99&tab=settings');
		expect(missing.view).toEqual({ group: null, tab: 'pens' });
		expect(missing.missingGroup).toBe(true);
		const notLoaded = resolve('?group=99&tab=settings', { loaded: false, canCreate: true });
		expect(notLoaded.view).toEqual({ group: 99, tab: 'settings' });
		expect(notLoaded.missingGroup).toBe(false);
	});

	it('読めない tab は「ペン」へ倒す。未選択のときのタブは常に「ペン」', () => {
		expect(resolve('?group=3&tab=bar').view).toEqual({ group: 3, tab: 'pens' });
		expect(resolve('?tab=settings').view).toEqual({ group: null, tab: 'pens' });
	});

	it('group=new は編集者以上だけ。閲覧者は未選択へ倒す', () => {
		expect(resolve('?group=new&tab=settings').view).toEqual({ group: 'new', tab: 'settings' });
		const viewer = resolve('?group=new&tab=settings', { loaded: true, canCreate: false });
		expect(viewer.view).toEqual({ group: null, tab: 'pens' });
		expect(viewer.missingGroup).toBe(true);
	});
});

describe('groupsViewSearch', () => {
	it('既定の値は書かない', () => {
		expect(groupsViewSearch({ group: null, tab: 'pens' })).toBe('');
		expect(groupsViewSearch({ group: 3, tab: 'pens' })).toBe('?group=3');
		expect(groupsViewSearch({ group: 3, tab: 'settings' })).toBe('?group=3&tab=settings');
		expect(groupsViewSearch({ group: 'new', tab: 'settings' })).toBe('?group=new&tab=settings');
	});

	it('書いて読むと元に戻る', () => {
		const views: GroupsView[] = [
			{ group: null, tab: 'pens' },
			{ group: 8, tab: 'pens' },
			{ group: 8, tab: 'settings' },
			{ group: 'new', tab: 'settings' }
		];
		for (const view of views) {
			const back = resolve(groupsViewSearch(view)).view;
			expect(sameGroupsView(back, view)).toBe(true);
		}
	});
});

describe('sameGroupsView', () => {
	it('group と tab の両方で比べる', () => {
		expect(sameGroupsView({ group: 3, tab: 'pens' }, { group: 3, tab: 'pens' })).toBe(true);
		expect(sameGroupsView({ group: 3, tab: 'pens' }, { group: 3, tab: 'settings' })).toBe(false);
		expect(sameGroupsView({ group: 3, tab: 'pens' }, { group: 8, tab: 'pens' })).toBe(false);
		expect(sameGroupsView({ group: null, tab: 'pens' }, { group: 'new', tab: 'pens' })).toBe(false);
	});
});
