/**
 * `monitorStreamView.ts` のユニットテスト（#441 のレビュー対応）。
 *
 * 守りたいこと: close `1008` で止まっている間（手動の再開待ち・ログイン
 * 状態の確認中）は、表の値を「最終受信値」、品質を「陳腐化（受信時: …）」
 * として見せ、品質「良好」をそのまま出さない。再開の直後はスナップショット
 * まで `--`、届いたら通常の表示へ戻る。通常の再接続中の表示は変えない。
 */
import { describe, expect, it } from 'vitest';
import {
	cellDisplayMode,
	initialStreamView,
	monitorCellDisplay,
	monitorColumnLabels,
	RECHECK_FAILED_MESSAGE,
	streamViewReducer,
	type CellDisplayMode,
	type StreamViewEvent,
	type StreamViewState
} from './monitorStreamView';

const UNKNOWN_HALT = { kind: 'halt', reason: 'x', message: 'm' } as const;
const RECHECK = { kind: 'recheckSession', reason: 'session_revoked' } as const;

function run(events: StreamViewEvent[], from: StreamViewState = initialStreamView()) {
	return events.reduce(streamViewReducer, from);
}

describe('cellDisplayMode', () => {
	const cases: Array<[string, StreamViewState, CellDisplayMode]> = [
		['初期（未接続・最初の値の待ち）', initialStreamView(), 'awaitingSnapshot'],
		[
			'接続中・値を受けた',
			{ connected: true, backpressure: false, awaitingSnapshot: false, halt: null },
			'live'
		],
		[
			'接続中・最初の値の待ち',
			{ connected: true, backpressure: false, awaitingSnapshot: true, halt: null },
			'awaitingSnapshot'
		],
		[
			'通常の再接続中（値は受けた後）',
			{ connected: false, backpressure: false, awaitingSnapshot: false, halt: null },
			'live'
		],
		[
			'1008 で停止（手動の再開待ち）',
			{ connected: false, backpressure: false, awaitingSnapshot: false, halt: UNKNOWN_HALT },
			'halted'
		],
		[
			'1008 で停止（ログイン状態の確認中）',
			{ connected: false, backpressure: false, awaitingSnapshot: false, halt: RECHECK },
			'halted'
		],
		[
			'1008 で停止・最初の値の待ちでも停止を優先',
			{ connected: false, backpressure: false, awaitingSnapshot: true, halt: UNKNOWN_HALT },
			'halted'
		]
	];
	it.each(cases)('%s', (_label, state, mode) => {
		expect(cellDisplayMode(state)).toBe(mode);
	});
});

describe('monitorCellDisplay', () => {
	const good = { v: 42, q: 'good', unit: 'mm' };
	const bad = { v: 7, q: 'bad', unit: null };

	it.each([
		['live', good, { value: '42 mm', qualityClass: 'good', qualityLabel: '良好' }],
		['live', bad, { value: '--', qualityClass: 'bad', qualityLabel: '不良' }],
		['awaitingSnapshot', good, { value: '--', qualityClass: 'stale', qualityLabel: '陳腐化' }],
		[
			'halted',
			good,
			{ value: '42 mm', qualityClass: 'stale', qualityLabel: '陳腐化（受信時: 良好）' }
		],
		['halted', bad, { value: '--', qualityClass: 'stale', qualityLabel: '陳腐化（受信時: 不良）' }]
	] as const)('%s / 品質 %o', (mode, row, expected) => {
		expect(monitorCellDisplay(row, mode)).toEqual(expected);
	});

	it('停止中は品質「良好」をそのまま出さない', () => {
		const cell = monitorCellDisplay(good, 'halted');
		expect(cell.qualityLabel).not.toBe('良好');
		expect(cell.qualityClass).not.toBe('good');
	});

	it('列名は停止中だけ「最終受信値」「品質（受信時）」', () => {
		expect(monitorColumnLabels('halted')).toEqual({
			value: '最終受信値',
			quality: '品質（受信時）'
		});
		expect(monitorColumnLabels('live')).toEqual({ value: '値', quality: '品質' });
		expect(monitorColumnLabels('awaitingSnapshot')).toEqual({ value: '値', quality: '品質' });
	});
});

describe('streamViewReducer', () => {
	it('良好な値 → 未知の理由の 1008 → 停止中は最終受信値 → 手動で再開 → スナップショットで通常へ', () => {
		const row = { v: 42, q: 'good', unit: null };
		let state = run([{ type: 'connected' }, { type: 'data' }]);
		expect(monitorCellDisplay(row, cellDisplayMode(state)).qualityLabel).toBe('良好');

		state = run(
			[
				{ type: 'disconnected', code: 1008 },
				{ type: 'halted', action: UNKNOWN_HALT }
			],
			state
		);
		expect(cellDisplayMode(state)).toBe('halted');
		expect(monitorCellDisplay(row, cellDisplayMode(state))).toEqual({
			value: '42',
			qualityClass: 'stale',
			qualityLabel: '陳腐化（受信時: 良好）'
		});

		// 再開の直後（まだ繋がっていない・繋がった直後）は今の値に見せない。
		state = run([{ type: 'resumed' }], state);
		expect(cellDisplayMode(state)).toBe('awaitingSnapshot');
		state = run([{ type: 'connected' }], state);
		expect(cellDisplayMode(state)).toBe('awaitingSnapshot');

		state = run([{ type: 'data' }], state);
		expect(cellDisplayMode(state)).toBe('live');
		expect(monitorCellDisplay({ v: 43, q: 'good', unit: null }, 'live').qualityLabel).toBe('良好');
	});

	it('通常の切断では停止の表示にしない（従来どおり）', () => {
		const state = run([
			{ type: 'connected' },
			{ type: 'data' },
			{ type: 'disconnected', code: 1006 }
		]);
		expect(state.halt).toBeNull();
		expect(cellDisplayMode(state)).toBe('live');
	});

	it('1013 はバックプレッシャの表示、接続で解除', () => {
		let state = run([{ type: 'connected' }, { type: 'disconnected', code: 1013 }]);
		expect(state.backpressure).toBe(true);
		state = run([{ type: 'connected' }], state);
		expect(state.backpressure).toBe(false);
	});

	it('ログイン状態の確認が失敗したら、理由と再接続ボタンの停止へ', () => {
		const state = run([
			{ type: 'halted', action: RECHECK },
			{ type: 'recheckFailed', reason: 'session_revoked' }
		]);
		expect(state.halt).toEqual({
			kind: 'halt',
			reason: 'session_revoked',
			message: RECHECK_FAILED_MESSAGE
		});
	});

	it('購読の変更で最初の値の待ちに戻る', () => {
		const state = run([{ type: 'connected' }, { type: 'data' }, { type: 'resubscribed' }]);
		expect(cellDisplayMode(state)).toBe('awaitingSnapshot');
	});
});
