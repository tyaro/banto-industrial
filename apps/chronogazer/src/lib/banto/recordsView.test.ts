import { describe, expect, it } from 'vitest';
import {
	filterRecordsByTarget,
	parseRecordsView,
	recordsViewSearch,
	resolveRecordsView,
	sameRecordsView,
	type RecordsView
} from './recordsView.js';

const connections = [{ id: 1 }, { id: 2 }];
const groups = [
	{ id: 10, plcConnectionId: 1 },
	{ id: 11, plcConnectionId: 1 },
	{ id: 12, plcConnectionId: 2 }
];
const writer = { loaded: true, settingsAvailable: true };

function resolve(search: string, options = writer) {
	return resolveRecordsView(
		parseRecordsView(new URLSearchParams(search)),
		connections,
		groups,
		options
	);
}

describe('parseRecordsView', () => {
	it('クエリが無ければ「すべて」・「記録項目」', () => {
		expect(parseRecordsView(new URLSearchParams(''))).toEqual({
			target: { type: 'all' },
			tab: 'list'
		});
	});

	it('接続先・収集グループと設定タブを読む', () => {
		expect(parseRecordsView(new URLSearchParams('target=connection:2'))).toEqual({
			target: { type: 'connection', id: 2 },
			tab: 'list'
		});
		expect(parseRecordsView(new URLSearchParams('target=group:10&tab=settings'))).toEqual({
			target: { type: 'group', id: 10 },
			tab: 'settings'
		});
	});

	it('読めない値は null（0・負数・小数・余計な文字・別の種類）', () => {
		for (const raw of ['group:0', 'group:-1', 'group:1.5', 'group:1x', 'tag:1', 'foo', '']) {
			expect(parseRecordsView(new URLSearchParams({ target: raw })).target).toBeNull();
		}
		expect(parseRecordsView(new URLSearchParams('tab=bar')).tab).toBeNull();
	});
});

describe('resolveRecordsView', () => {
	it('一覧にある対象とタブをそのまま使う', () => {
		expect(resolve('target=group:10&tab=settings')).toEqual({
			view: { target: { type: 'group', id: 10 }, tab: 'settings' },
			missingTarget: false
		});
		expect(resolve('target=connection:2')).toEqual({
			view: { target: { type: 'connection', id: 2 }, tab: 'list' },
			missingTarget: false
		});
	});

	it('一覧に無い id・読めない値は「すべて」・「記録項目」へ倒す', () => {
		for (const search of ['target=group:99&tab=settings', 'target=connection:9', 'target=foo']) {
			expect(resolve(search)).toEqual({
				view: { target: { type: 'all' }, tab: 'list' },
				missingTarget: true
			});
		}
	});

	it('読み込み前は id の存在を判定しない（ディープリンクを消さない）', () => {
		expect(
			resolve('target=group:99&tab=settings', { loaded: false, settingsAvailable: true })
		).toEqual({
			view: { target: { type: 'group', id: 99 }, tab: 'settings' },
			missingTarget: false
		});
	});

	it('「すべて」と閲覧者に設定タブは無い。読めない tab は「記録項目」', () => {
		expect(resolve('tab=settings').view.tab).toBe('list');
		expect(
			resolve('target=group:10&tab=settings', { loaded: true, settingsAvailable: false }).view.tab
		).toBe('list');
		expect(resolve('target=group:10&tab=bar').view.tab).toBe('list');
	});
});

describe('recordsViewSearch', () => {
	it('既定の値は書かない', () => {
		expect(recordsViewSearch({ target: { type: 'all' }, tab: 'list' })).toBe('');
		expect(recordsViewSearch({ target: { type: 'connection', id: 1 }, tab: 'list' })).toBe(
			'?target=connection:1'
		);
		expect(recordsViewSearch({ target: { type: 'group', id: 10 }, tab: 'settings' })).toBe(
			'?target=group:10&tab=settings'
		);
	});

	it('書いた URL を読み直すと同じ状態になる', () => {
		const views: RecordsView[] = [
			{ target: { type: 'all' }, tab: 'list' },
			{ target: { type: 'connection', id: 2 }, tab: 'settings' },
			{ target: { type: 'group', id: 11 }, tab: 'list' }
		];
		for (const view of views) {
			const search = recordsViewSearch(view).replace(/^\?/, '');
			expect(sameRecordsView(resolve(search).view, view)).toBe(true);
		}
	});
});

describe('sameRecordsView', () => {
	it('対象の種類・id・タブを比べる', () => {
		const a: RecordsView = { target: { type: 'group', id: 1 }, tab: 'list' };
		expect(sameRecordsView(a, { target: { type: 'group', id: 1 }, tab: 'list' })).toBe(true);
		expect(sameRecordsView(a, { target: { type: 'connection', id: 1 }, tab: 'list' })).toBe(false);
		expect(sameRecordsView(a, { target: { type: 'group', id: 2 }, tab: 'list' })).toBe(false);
		expect(sameRecordsView(a, { target: { type: 'group', id: 1 }, tab: 'settings' })).toBe(false);
	});
});

describe('filterRecordsByTarget', () => {
	const tags = [
		{ id: 1, collectionGroupId: 10 },
		{ id: 2, collectionGroupId: 11 },
		{ id: 3, collectionGroupId: 12 }
	];

	it('すべて = 全件、接続先 = 配下のグループ、グループ = そのグループ', () => {
		expect(filterRecordsByTarget(tags, { type: 'all' }, groups).map((t) => t.id)).toEqual([
			1, 2, 3
		]);
		expect(
			filterRecordsByTarget(tags, { type: 'connection', id: 1 }, groups).map((t) => t.id)
		).toEqual([1, 2]);
		expect(filterRecordsByTarget(tags, { type: 'group', id: 12 }, groups).map((t) => t.id)).toEqual(
			[3]
		);
		expect(filterRecordsByTarget(tags, { type: 'connection', id: 9 }, groups)).toEqual([]);
	});
});
