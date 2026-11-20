import { describe, expect, it } from 'vitest';
import { dockLayoutKeys, parseStoredDockLayout, serializeDockLayout } from './dockLayout';

const SAVED = {
	schema: 1 as const,
	panels: ['left', 'center'],
	layout: { version: 3, floating: [], docked: null }
};

describe('dockLayoutKeys', () => {
	it('ページごとに localStorage とサーバーのキーを分ける', () => {
		expect(dockLayoutKeys('monitor')).toEqual({
			local: 'banto.dock.monitor',
			setting: 'hub.dock.monitor'
		});
		expect(dockLayoutKeys('tags').setting).toBe('hub.dock.tags');
	});

	it.each(['', 'Monitor', 'a.b', 'a/b', 'x'.repeat(41)])('不正なページ名 %j は例外', (page) => {
		expect(() => dockLayoutKeys(page)).toThrow();
	});
});

describe('parseStoredDockLayout', () => {
	it('自分が書いた形をそのまま読む', () => {
		expect(parseStoredDockLayout(serializeDockLayout(SAVED))).toEqual(SAVED);
	});

	it.each([
		['未保存', null],
		['undefined', undefined],
		['空', ''],
		['JSON でない', '{'],
		['配列', '[]'],
		['数値', '1'],
		['schema が違う', JSON.stringify({ ...SAVED, schema: 2 })],
		['schema が無い（素の DockLayout）', JSON.stringify(SAVED.layout)],
		['panels が配列でない', JSON.stringify({ ...SAVED, panels: 'left' })],
		['panels に文字列でないもの', JSON.stringify({ ...SAVED, panels: ['left', 1] })],
		['layout が無い', JSON.stringify({ schema: 1, panels: [] })],
		['layout が null', JSON.stringify({ ...SAVED, layout: null })],
		['layout が配列', JSON.stringify({ ...SAVED, layout: [] })]
	])('%s は undefined（部品の既定に任せる）', (_name, raw) => {
		expect(parseStoredDockLayout(raw)).toBeUndefined();
	});

	it('中身の検証は部品に任せる（知らない id もそのまま返す）', () => {
		const raw = JSON.stringify({ ...SAVED, panels: ['left', 'center', 'gone'] });
		expect(parseStoredDockLayout(raw)?.panels).toEqual(['left', 'center', 'gone']);
	});
});

describe('serializeDockLayout', () => {
	it('同じ配置は同じ文字列（書き込みを省く比較に使う）', () => {
		expect(serializeDockLayout(SAVED)).toBe(serializeDockLayout(structuredClone(SAVED)));
	});
});
