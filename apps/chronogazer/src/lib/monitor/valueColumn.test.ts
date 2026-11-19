import { describe, expect, it } from 'vitest';
import type { Tag } from '../banto/tagRegistryAdmin';
import type { CurrentSampleView } from '../banto/collectAdmin';
import {
	UNREGISTERED_VALUE_LABEL,
	VALUES_NOT_RUNNING_NOTE,
	valueCell,
	valuesNote,
	type ValueTarget
} from './valueColumn';

const tags = new Map<number, Pick<Tag, 'name' | 'unit' | 'decimals' | 'dataType'>>([
	[1, { name: '温度', unit: '℃', decimals: 1, dataType: 'f32' }],
	[2, { name: '運転', unit: null, decimals: 0, dataType: 'bit' }],
	[3, { name: '圧力', unit: null, decimals: 2, dataType: 'f32' }]
]);

function s(quality: CurrentSampleView['quality'], value: number | null): CurrentSampleView {
	return { value, ptimeMs: 10, quality, lastGoodMs: value === null ? null : 10 };
}

const values: Record<string, CurrentSampleView> = {
	'tag:1': s('good', 12.345),
	'tag:2': s('good', 1),
	'tag:3': s('bad', null),
	'tag:4': s('stale', null),
	'tag:5': s('invalid', null)
};

describe('valueCell', () => {
	const cases: [string, ValueTarget, typeof values | null, string, string | null][] = [
		['良好の数値は小数桁と単位つき', { kind: 'tag', tagId: 1 }, values, '12.3 ℃', '正常'],
		['bit は True/False', { kind: 'tag', tagId: 2 }, values, 'True', '正常'],
		['通信エラーは値を出さない', { kind: 'tag', tagId: 3 }, values, '—', '通信エラー'],
		['更新停止', { kind: 'tag', tagId: 4 }, values, '—', '更新停止'],
		[
			'設定不正（タグ情報なしでも出す）',
			{ kind: 'tag', tagId: 5 },
			values,
			'—',
			'設定不正（収集対象外）'
		],
		['表にキーが無ければ「—」だけ', { kind: 'tag', tagId: 9 }, values, '—', null],
		['値の表が無ければ「—」だけ', { kind: 'tag', tagId: 1 }, null, '—', null],
		['未登録の Hub のタグは「未登録」', { kind: 'hub' }, values, '—', UNREGISTERED_VALUE_LABEL],
		['未登録は値の表が無くても「未登録」', { kind: 'hub' }, null, '—', UNREGISTERED_VALUE_LABEL],
		['タグ未選択・登録できない行は空', { kind: 'none' }, values, '—', null]
	];
	it.each(cases)('%s', (_name, target, vals, text, quality) => {
		const cell = valueCell(target, tags, vals);
		expect(cell.text).toBe(text);
		expect(cell.quality).toBe(quality);
	});

	it('状態の区別（色分け用）', () => {
		expect(valueCell({ kind: 'hub' }, tags, values).state).toBe('unregistered');
		expect(valueCell({ kind: 'tag', tagId: 1 }, tags, values).state).toBe('good');
		expect(valueCell({ kind: 'tag', tagId: 3 }, tags, values).state).toBe('bad');
		expect(valueCell({ kind: 'tag', tagId: 9 }, tags, values).state).toBe('none');
	});
});

describe('valuesNote', () => {
	const label = (ms: number): string => `t${ms}`;
	const cases: [
		string,
		boolean,
		{ phase: 'loading' | 'notRunning' | 'ready'; lastOkAt: number | null },
		boolean,
		string | null
	][] = [
		['ポーリングしていなければ出さない', false, { phase: 'notRunning', lastOkAt: 1 }, true, null],
		['読み込み中は出さない', true, { phase: 'loading', lastOkAt: null }, false, null],
		['読めていれば出さない', true, { phase: 'ready', lastOkAt: 5 }, false, null],
		[
			'収集が動いていない',
			true,
			{ phase: 'notRunning', lastOkAt: 5 },
			false,
			VALUES_NOT_RUNNING_NOTE
		],
		[
			'連続失敗（取得履歴あり）は時刻つき',
			true,
			{ phase: 'ready', lastOkAt: 5 },
			true,
			'現在値を取得できていません。下の表示はt5に取得したもので、最新ではありません。'
		],
		[
			'連続失敗（一度も取れていない）',
			true,
			{ phase: 'loading', lastOkAt: null },
			true,
			'現在値を取得できていません（まだ一度も取得できていません）。'
		]
	];
	it.each(cases)('%s', (_name, active, state, stale, expected) => {
		expect(valuesNote(active, state, stale, label)).toBe(expected);
	});
});
