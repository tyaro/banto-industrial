import { describe, expect, it } from 'vitest';
import {
	parseTagsView,
	resolveTagsView,
	sameTagsView,
	tagsViewSearch,
	type TagsView
} from './tagsView.js';

const connections = [{ id: 1 }, { id: 2 }];
const groups = [{ id: 10 }, { id: 11 }];
const loaded = { loaded: true };

function resolve(search: string, options = loaded) {
	return resolveTagsView(parseTagsView(new URLSearchParams(search)), connections, groups, options);
}

describe('parseTagsView', () => {
	it('クエリが無ければ「すべて」・「タグ一覧」', () => {
		expect(parseTagsView(new URLSearchParams(''))).toEqual({
			target: { type: 'all' },
			tab: 'list'
		});
	});

	it('接続先・収集グループと設定タブを読む', () => {
		expect(parseTagsView(new URLSearchParams('target=connection:2'))).toEqual({
			target: { type: 'connection', id: 2 },
			tab: 'list'
		});
		expect(parseTagsView(new URLSearchParams('target=group:10&tab=settings'))).toEqual({
			target: { type: 'group', id: 10 },
			tab: 'settings'
		});
	});

	it('読めない値は null（0・負数・小数・余計な文字・別の種類）', () => {
		for (const raw of ['foo', 'group:', 'group:0', 'group:-1', 'group:1.5', 'group:01', 'tag:1']) {
			expect(parseTagsView(new URLSearchParams({ target: raw })).target).toBeNull();
		}
		expect(parseTagsView(new URLSearchParams('target=group:1&tab=bar')).tab).toBeNull();
	});

	it('以前の groupId は読まない（「すべて」）', () => {
		expect(parseTagsView(new URLSearchParams('groupId=10')).target).toEqual({ type: 'all' });
	});
});

describe('resolveTagsView', () => {
	it('一覧にある対象とタブをそのまま使う', () => {
		expect(resolve('target=group:11&tab=settings')).toEqual({
			view: { target: { type: 'group', id: 11 }, tab: 'settings' },
			missingTarget: false
		});
	});

	it('一覧に無い id・読めない値は「すべて」・「タグ一覧」へ倒す', () => {
		for (const search of ['target=group:99&tab=settings', 'target=connection:99', 'target=x']) {
			expect(resolve(search)).toEqual({
				view: { target: { type: 'all' }, tab: 'list' },
				missingTarget: true
			});
		}
	});

	it('読み込み前は id の存在を判定しない（ディープリンクを消さない）', () => {
		expect(resolve('target=group:99&tab=settings', { loaded: false })).toEqual({
			view: { target: { type: 'group', id: 99 }, tab: 'settings' },
			missingTarget: false
		});
	});

	it('「すべて」に設定タブは無い', () => {
		expect(resolve('tab=settings').view).toEqual({ target: { type: 'all' }, tab: 'list' });
	});

	it('読めない tab は「タグ一覧」', () => {
		expect(resolve('target=connection:1&tab=bar').view.tab).toBe('list');
	});
});

describe('tagsViewSearch', () => {
	it('既定の値は書かない', () => {
		expect(tagsViewSearch({ target: { type: 'all' }, tab: 'list' })).toBe('');
		expect(tagsViewSearch({ target: { type: 'all' }, tab: 'settings' })).toBe('');
		expect(tagsViewSearch({ target: { type: 'connection', id: 2 }, tab: 'list' })).toBe(
			'?target=connection:2'
		);
		expect(tagsViewSearch({ target: { type: 'group', id: 10 }, tab: 'settings' })).toBe(
			'?target=group:10&tab=settings'
		);
	});

	it('書いた URL を読み直すと同じ状態になる', () => {
		const views: TagsView[] = [
			{ target: { type: 'all' }, tab: 'list' },
			{ target: { type: 'connection', id: 1 }, tab: 'settings' },
			{ target: { type: 'group', id: 10 }, tab: 'list' }
		];
		for (const view of views) {
			const back = resolve(tagsViewSearch(view).replace(/^\?/, '')).view;
			expect(sameTagsView(back, view)).toBe(true);
		}
	});
});

describe('sameTagsView', () => {
	it('対象の種類・id・タブを比べる', () => {
		const a: TagsView = { target: { type: 'group', id: 10 }, tab: 'list' };
		expect(sameTagsView(a, { target: { type: 'group', id: 10 }, tab: 'list' })).toBe(true);
		expect(sameTagsView(a, { target: { type: 'group', id: 11 }, tab: 'list' })).toBe(false);
		expect(sameTagsView(a, { target: { type: 'connection', id: 10 }, tab: 'list' })).toBe(false);
		expect(sameTagsView(a, { target: { type: 'group', id: 10 }, tab: 'settings' })).toBe(false);
	});
});
