import { describe, expect, it } from 'vitest';
import type { CollectorStateView, ExclusionView } from '../banto/collectAdmin';
import {
	APPLY_LABELS,
	REFLECT_BUSY_TITLE,
	REFLECT_DIRTY_TITLE,
	REFLECT_RESTART_CONFIRM,
	REFLECT_START_CONFIRM,
	applyBand,
	isPenUnsaved,
	notRunningLabel,
	penApplyState,
	reflectGate,
	STARTING_RECHECK_MAX,
	nextStatusRecheckMs,
	STATUS_RECHECK_MS,
	STARTING_RECHECK_MS,
	type ApplyState
} from './applyState';

function excl(unit: ExclusionView['unit'], key: string): ExclusionView {
	return { unit, id: 1, key, name: 'x', reason: 'invalidAddress', message: 'm' };
}
function running(tagKeys: string[], exclusions: ExclusionView[] = []): CollectorStateView {
	return { state: 'running', groups: 1, tags: tagKeys.length, tagKeys, exclusions };
}
const on = { enabled: true, collectionGroupId: 5 };
const pen = { tagId: 1, hubTag: null };

describe('isPenUnsaved', () => {
	it('hubTag のペンと、基準に無い tagId のペンは未保存', () => {
		expect(isPenUnsaved({ tagId: null, hubTag: {} }, new Set([1]))).toBe(true);
		expect(isPenUnsaved({ tagId: 2, hubTag: null }, new Set([1]))).toBe(true);
		expect(isPenUnsaved({ tagId: 1, hubTag: null }, new Set([1]))).toBe(false);
		expect(isPenUnsaved({ tagId: null, hubTag: null }, new Set([1]))).toBe(false);
	});
});

describe('penApplyState', () => {
	const cases: [string, Parameters<typeof penApplyState>, string, string][] = [
		['未保存は他より優先', [pen, true, on, running(['tag:1'])], 'unsaved', '未保存'],
		['タグ未選択', [{ tagId: null, hubTag: null }, false, undefined, null], 'none', ''],
		['登録情報が無い', [pen, false, undefined, running([])], 'unknown', '不明'],
		[
			'無効（収集が止まっていても）',
			[pen, false, { enabled: false }, { state: 'stopped' }],
			'disabled',
			'無効'
		],
		['収集の状態が不明なら未反映と言わない', [pen, false, on, null], 'unknown', '不明'],
		['停止', [pen, false, on, { state: 'stopped' }], 'notRunning', '収集停止中'],
		['開始中', [pen, false, on, { state: 'starting' }], 'notRunning', '収集開始中'],
		[
			'対象なし',
			[pen, false, on, { state: 'noTargets', exclusions: [] }],
			'notRunning',
			'収集対象なし'
		],
		['開始失敗', [pen, false, on, { state: 'startFailed' }], 'notRunning', '収集開始に失敗'],
		['tagKeys にある', [pen, false, on, running(['tag:1'])], 'recording', '記録中'],
		[
			'tagKeys に無く exclusions にも無い',
			[pen, false, on, running(['tag:2'])],
			'pending',
			'保存済み・未反映'
		],
		[
			'tag の exclusions',
			[pen, false, on, running([], [excl('tag', 'tag:1')])],
			'excluded',
			'設定不正（収集対象外）'
		],
		[
			'グループの exclusions',
			[pen, false, on, running([], [excl('group', 'grp:5')])],
			'excluded',
			'設定不正（収集対象外）'
		],
		[
			'別タグの exclusions は無関係',
			[pen, false, on, running(['tag:1'], [excl('tag', 'tag:9')])],
			'recording',
			'記録中'
		],
		[
			'simulated が null は収集対象（記録状態不明）',
			[pen, false, { ...on, simulated: null }, running(['tag:1'])],
			'targetUnknown',
			'収集対象（記録状態不明）'
		],
		[
			'simulated が false は記録中',
			[pen, false, { ...on, simulated: false }, running(['tag:1'])],
			'recording',
			'記録中'
		],
		[
			'simulated が null でも tagKeys に無ければ未反映',
			[pen, false, { ...on, simulated: null }, running([])],
			'pending',
			'保存済み・未反映'
		],
		[
			'exclusions が tagKeys より優先',
			[pen, false, on, running(['tag:1'], [excl('tag', 'tag:1')])],
			'excluded',
			'設定不正（収集対象外）'
		]
	];
	for (const [name, args, kind, label] of cases) {
		it(name, () => {
			expect(penApplyState(...args)).toEqual({ kind, label });
		});
	}
	it('「記録中」の文言は受信異常を含めない（品質の列が受け持つ）', () => {
		expect(APPLY_LABELS.recording).toBe('記録中');
	});
	it('notRunningLabel は 4 状態で別々', () => {
		const labels = [
			notRunningLabel({ state: 'stopped' }),
			notRunningLabel({ state: 'starting' }),
			notRunningLabel({ state: 'noTargets', exclusions: [] }),
			notRunningLabel({ state: 'startFailed' })
		];
		expect(new Set(labels).size).toBe(4);
	});
});

const st = (kind: ApplyState['kind']): ApplyState => ({ kind, label: '' });
const base = { dirty: false, canWrite: true, busy: false };

describe('applyBand', () => {
	it('編集中が最優先', () => {
		const b = applyBand({ ...base, dirty: true, collect: running([]), states: [st('unsaved')] });
		expect(b.kind).toBe('editing');
		expect(b.text).toBe('編集中（未保存）');
		expect(b.reflect).toBeNull();
	});
	it('編集中で未反映があればボタンは無効（理由つき）', () => {
		const b = applyBand({ ...base, dirty: true, collect: running([]), states: [st('pending')] });
		expect(b.reflect).toMatchObject({ disabled: true, title: REFLECT_DIRTY_TITLE });
	});
	it('未反映 n 件 + 再起動ボタン', () => {
		const b = applyBand({
			...base,
			collect: running([]),
			states: [st('pending'), st('pending'), st('recording')]
		});
		expect(b.kind).toBe('pending');
		expect(b.text).toBe('保存済み・未反映 2 件');
		expect(b.reflect).toEqual({
			action: 'restart',
			label: '記録へ反映',
			disabled: false,
			title: null,
			confirm: REFLECT_RESTART_CONFIRM
		});
	});
	it('実行中は無効', () => {
		const b = applyBand({ ...base, busy: true, collect: running([]), states: [st('pending')] });
		expect(b.reflect).toMatchObject({ disabled: true, title: REFLECT_BUSY_TITLE });
	});
	it('閲覧者にはボタンを出さない', () => {
		const b = applyBand({
			...base,
			canWrite: false,
			collect: running([]),
			states: [st('pending')]
		});
		expect(b.text).toBe('保存済み・未反映 1 件');
		expect(b.reflect).toBeNull();
	});
	it('全部記録中なら「記録中」でボタン無し（excluded・disabled は数えない）', () => {
		const b = applyBand({
			...base,
			collect: running(['tag:1']),
			states: [st('recording'), st('excluded'), st('disabled')]
		});
		expect(b).toMatchObject({ kind: 'recording', text: '記録中', reflectCount: 0, reflect: null });
	});
	it('停止中で有効なペンがあれば「開始」で反映', () => {
		for (const collect of [
			{ state: 'stopped' },
			{ state: 'startFailed' },
			{ state: 'noTargets', exclusions: [] }
		] as CollectorStateView[]) {
			const b = applyBand({ ...base, collect, states: [st('notRunning')] });
			expect(b.kind).toBe('notRunning');
			expect(b.reflect).toMatchObject({ action: 'start', confirm: REFLECT_START_CONFIRM });
		}
	});
	it('シミュレーションのペンは未反映の件数に数えず、記録中の代表にもしない', () => {
		const b = applyBand({
			...base,
			collect: running(['tag:1']),
			states: [st('simulated'), st('pending')]
		});
		expect(b).toMatchObject({ kind: 'pending', text: '保存済み・未反映 1 件', reflectCount: 1 });
		const c = applyBand({ ...base, collect: running(['tag:1']), states: [st('simulated')] });
		expect(c).toMatchObject({
			kind: 'idle',
			text: APPLY_LABELS.simulated,
			reflectCount: 0,
			reflect: null
		});
	});
	it('noTargets で全部が除外なら、ボタンを出さず除外を代表にする', () => {
		const b = applyBand({
			...base,
			collect: { state: 'noTargets', exclusions: [] },
			states: [st('excluded'), st('excluded')]
		});
		expect(b).toMatchObject({
			kind: 'notRunning',
			text: APPLY_LABELS.excluded,
			reflectCount: 0,
			reflect: null
		});
	});
	it('noTargets で除外と有効が混在なら、有効なものだけ数えて開始で反映', () => {
		const b = applyBand({
			...base,
			collect: { state: 'noTargets', exclusions: [] },
			states: [st('excluded'), st('notRunning'), st('notRunning')]
		});
		expect(b.reflectCount).toBe(2);
		expect(b.text).toBe('収集対象なし（有効なタグ 2 件は開始すると記録されます）');
		expect(b.reflect).toMatchObject({ action: 'start' });
	});
	it('開始中はボタン無し', () => {
		const b = applyBand({ ...base, collect: { state: 'starting' }, states: [st('notRunning')] });
		expect(b.text).toBe('収集開始中');
		expect(b.reflect).toBeNull();
	});
	it('状態が不明ならそう言い、ボタンを出さない', () => {
		const b = applyBand({ ...base, collect: null, states: [st('unknown')] });
		expect(b).toMatchObject({ kind: 'unknown', text: '収集の状態: 不明', reflect: null });
	});
	it('状態が不明でも編集中は編集中', () => {
		const b = applyBand({ ...base, dirty: true, collect: null, states: [] });
		expect(b.kind).toBe('editing');
	});
});

describe('applyBand: 走行中で未反映が 0 件', () => {
	const cases: [string, ApplyState['kind'][], string, string][] = [
		['全部除外', ['excluded', 'excluded'], 'idle', APPLY_LABELS.excluded],
		['全部無効', ['disabled', 'disabled'], 'idle', APPLY_LABELS.disabled],
		['全部不明', ['unknown'], 'idle', APPLY_LABELS.unknown],
		['除外と無効の混在は除外', ['disabled', 'excluded'], 'idle', APPLY_LABELS.excluded],
		['収集対象（記録状態不明）のみ', ['targetUnknown'], 'idle', APPLY_LABELS.targetUnknown],
		[
			'シミュレーションと記録状態不明はシミュレーション',
			['targetUnknown', 'simulated'],
			'idle',
			APPLY_LABELS.simulated
		],
		[
			'記録状態不明と不明は記録状態不明',
			['unknown', 'targetUnknown'],
			'idle',
			APPLY_LABELS.targetUnknown
		],
		['記録中と記録状態不明の混在は記録中', ['targetUnknown', 'recording'], 'recording', '記録中'],
		['不明と無効の混在は不明', ['disabled', 'unknown'], 'idle', APPLY_LABELS.unknown],
		['1 本でも記録中なら記録中', ['excluded', 'recording', 'disabled'], 'recording', '記録中'],
		['ペン無し', [], 'idle', '記録するタグがありません'],
		['タグ未選択のみ', ['none'], 'idle', '記録するタグがありません']
	];
	it.each(cases)('%s', (_n, kinds, kind, text) => {
		const b = applyBand({ ...base, collect: running([]), states: kinds.map(st) });
		expect(b.kind).toBe(kind);
		expect(b.text).toBe(text);
		expect(b.reflect).toBeNull();
	});
});

describe('nextStatusRecheckMs', () => {
	const starting: CollectorStateView = { state: 'starting' };
	const cases: [string, CollectorStateView | null, number, boolean, number | null][] = [
		['starting は 1 秒', starting, 0, true, STARTING_RECHECK_MS],
		['starting の上限直前', starting, STARTING_RECHECK_MAX - 1, true, STARTING_RECHECK_MS],
		['starting の上限で通常の周期に戻る', starting, STARTING_RECHECK_MAX, true, STATUS_RECHECK_MS],
		['running は 5 秒', running([]), 0, true, STATUS_RECHECK_MS],
		['stopped は 5 秒', { state: 'stopped' }, 0, true, STATUS_RECHECK_MS],
		['取れていない（null）も 5 秒', null, 0, true, STATUS_RECHECK_MS],
		['見えていなければ予約しない', running([]), 0, false, null],
		['starting でも見えていなければ予約しない', starting, 0, false, null]
	];
	it.each(cases)('%s', (_n, collect, attempts, visible, expected) => {
		expect(nextStatusRecheckMs(collect, attempts, visible)).toBe(expected);
	});
});

describe('reflectGate', () => {
	const start = { action: 'start' as const };
	const restart = { action: 'restart' as const };
	it.each([
		['採用された・同じ前提', true, false, start, start, false, 'proceed'],
		['採用されなかった（追い越された）', false, false, start, start, false, 'unconfirmed'],
		['disposed', true, true, start, start, false, 'abort'],
		['disposed かつ未採用', false, true, start, start, false, 'abort'],
		['採用されたがボタンが無い', true, false, start, null, false, 'changed'],
		['採用されたが dirty', true, false, start, start, true, 'changed'],
		['採用されたが action が変わった', true, false, start, restart, false, 'changed']
	] as const)('%s', (_name, adopted, disposed, shown, now, dirty, expected) => {
		expect(reflectGate(adopted, disposed, shown, now, dirty)).toBe(expected);
	});
});
