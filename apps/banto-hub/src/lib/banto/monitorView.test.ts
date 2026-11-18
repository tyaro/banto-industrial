import { describe, expect, it } from 'vitest';
import { monitorViewSearch, parseMonitorFocus } from './monitorView.js';
import { parseTreeTarget } from './tagsView.js';

describe('monitorViewSearch（#1179 PR3b）', () => {
	it('「すべて」で focus も無ければ空', () => {
		expect(monitorViewSearch({ type: 'all' })).toBe('');
		expect(monitorViewSearch({ type: 'all' }, [])).toBe('');
	});

	it('対象はタグ登録と同じ target の形、tab は持たない', () => {
		expect(monitorViewSearch({ type: 'group', id: 5 })).toBe('?target=group:5');
		expect(monitorViewSearch({ type: 'connection', id: 3 })).toBe('?target=connection:3');
	});

	it('focus は区切りのカンマを生のまま、要素だけ encode する', () => {
		expect(monitorViewSearch({ type: 'group', id: 5 }, ['a 1', 'b&2'])).toBe(
			'?target=group:5&focus=a%201,b%262'
		);
		expect(monitorViewSearch({ type: 'all' }, ['x'])).toBe('?focus=x');
	});

	it('作ったクエリを読み直すと同じ対象・focus に戻る', () => {
		const focus = ['plc1.g.tag 1', '50%開度'];
		const params = new URLSearchParams(monitorViewSearch({ type: 'group', id: 7 }, focus));
		expect(parseTreeTarget(params)).toEqual({ type: 'group', id: 7 });
		expect(parseMonitorFocus(params)).toEqual(focus);
	});
});

describe('parseMonitorFocus', () => {
	it('無い・空なら空配列、空の要素は捨てる', () => {
		expect(parseMonitorFocus(new URLSearchParams(''))).toEqual([]);
		expect(parseMonitorFocus(new URLSearchParams('focus='))).toEqual([]);
		expect(parseMonitorFocus(new URLSearchParams('focus=a,,b'))).toEqual(['a', 'b']);
	});

	it('以前の ?group= は読まない（target だけが対象）', () => {
		expect(parseTreeTarget(new URLSearchParams('group=5'))).toEqual({ type: 'all' });
	});
});
