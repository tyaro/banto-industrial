import { describe, expect, it } from 'vitest';
import {
	clockReadResultLabel,
	clockSyncResultLabel,
	deviceCodeSuffix,
	formatOffset,
	isClockSyncFormDirty,
	mapClockUnavailable,
	plcTimeText,
	readingDiffText,
	scheduleSummary,
	settingsToForm,
	validateClockSyncForm,
	type ClockReading,
	type ClockSyncForm,
	type ClockSyncSettings
} from './plcClockAdmin';

describe('結果のラベル', () => {
	it.each([
		['ok', '成功'],
		['unsupported', 'この機種は非対応'],
		['rejected', 'ドライバが断った'],
		['device', '機器のエラー'],
		['verificationFailed', '確かめられなかった'],
		['unavailable', '送っていない（未接続など）'],
		['unknown', '結果不明'],
		['notOffered', 'このドライバは時計を扱わない'],
		['busy', '実行中']
	] as const)('時計合わせ %s', (result, label) => {
		expect(clockSyncResultLabel(result)).toBe(label);
	});

	it.each([
		['ok', '読めた'],
		['unsupported', 'この機種は非対応'],
		['rejected', 'ドライバが断った'],
		['device', '機器のエラー'],
		['verificationFailed', '値が時計として不正'],
		['unavailable', '送っていない（未接続など）'],
		['unknown', '結果不明'],
		['notOffered', 'このドライバは時計を扱わない']
	] as const)('読取り %s', (result, label) => {
		expect(clockReadResultLabel(result)).toBe(label);
	});

	it.each([
		['device', 0x55, '（終了コード 0x0055）'],
		['device', 0xc051, '（終了コード 0xC051）'],
		['device', 0, '（終了コード 0x0000）'],
		['device', null, ''],
		['rejected', 0x55, ''],
		['ok', null, '']
	] as const)('終了コード %s %s', (result, code, expected) => {
		expect(deviceCodeSuffix(result, code)).toBe(expected);
	});
});

describe('formatOffset', () => {
	it.each([
		[0, '0 秒'],
		[3, '+3 秒（PLC が進んでいる）'],
		[-1, '-1 秒（PLC が遅れている）'],
		[-59, '-59 秒（PLC が遅れている）'],
		[60, '+1 分 0 秒（PLC が進んでいる）'],
		[75, '+1 分 15 秒（PLC が進んでいる）'],
		[-75, '-1 分 15 秒（PLC が遅れている）'],
		[3600, '+60 分 0 秒（PLC が進んでいる）']
	])('%s 秒', (seconds, expected) => {
		expect(formatOffset(seconds)).toBe(expected);
	});
});

describe('scheduleSummary・plcTimeText', () => {
	it('時刻表の要約', () => {
		expect(scheduleSummary({ kind: 'hourly', minute: 5 })).toBe('毎時 5 分');
		expect(scheduleSummary({ kind: 'daily', hour: 3, minute: 30 })).toBe('毎日 03:30');
		expect(scheduleSummary({ kind: 'daily', hour: 13, minute: 0 })).toBe('毎日 13:00');
	});

	it('T を空白にするだけで変換しない', () => {
		expect(plcTimeText('2026-11-07T10:00:03')).toBe('2026-11-07 10:00:03');
	});
});

describe('readingDiffText', () => {
	const base: ClockReading = {
		at: '2026-11-07T01:00:00Z',
		result: 'ok',
		code: null,
		message: '',
		plcTime: '2026-11-07T10:00:03',
		pcTime: '2026-11-07T10:00:00',
		offsetSeconds: 3
	};
	it('ok は差、それ以外はラベル（device は終了コード付き）', () => {
		expect(readingDiffText(base)).toBe('+3 秒（PLC が進んでいる）');
		expect(
			readingDiffText({
				...base,
				result: 'device',
				code: 0xc051,
				plcTime: null,
				offsetSeconds: null
			})
		).toBe('機器のエラー（終了コード 0xC051）');
		expect(
			readingDiffText({ ...base, result: 'rejected', plcTime: null, offsetSeconds: null })
		).toBe('ドライバが断った');
	});
});

describe('validateClockSyncForm', () => {
	const form = (over: Partial<ClockSyncForm>): ClockSyncForm => ({
		enabled: true,
		kind: 'daily',
		hour: 3,
		minute: 30,
		...over
	});

	it('毎日・毎時の設定を作る（毎時は時を見ない）', () => {
		expect(validateClockSyncForm(form({}))).toEqual({
			ok: true,
			settings: { enabled: true, schedule: { kind: 'daily', hour: 3, minute: 30 } }
		});
		expect(validateClockSyncForm(form({ kind: 'hourly', hour: null, enabled: false }))).toEqual({
			ok: true,
			settings: { enabled: false, schedule: { kind: 'hourly', minute: 30 } }
		});
	});

	it.each([
		[{ minute: 60 }, 'minute'],
		[{ minute: -1 }, 'minute'],
		[{ minute: 1.5 }, 'minute'],
		[{ minute: null }, 'minute'],
		[{ hour: 24 }, 'hour'],
		[{ hour: -1 }, 'hour'],
		[{ hour: null }, 'hour']
	] as const)('範囲外・未入力 %j', (over, key) => {
		const result = validateClockSyncForm(form(over));
		expect(result.ok).toBe(false);
		if (!result.ok) expect(result.errors[key]).toMatch(/整数/);
	});

	it('毎時のとき時が範囲外でもエラーにしない', () => {
		expect(validateClockSyncForm(form({ kind: 'hourly', hour: 99 })).ok).toBe(true);
	});

	it('境界値 0:00・23:59', () => {
		expect(validateClockSyncForm(form({ hour: 0, minute: 0 })).ok).toBe(true);
		expect(validateClockSyncForm(form({ hour: 23, minute: 59 })).ok).toBe(true);
	});
});

describe('isClockSyncFormDirty', () => {
	const base: ClockSyncSettings = { enabled: false, schedule: { kind: 'hourly', minute: 5 } };
	it('読み込んだ設定と同じなら dirty でない', () => {
		expect(isClockSyncFormDirty(base, settingsToForm(base))).toBe(false);
	});
	it('毎時のとき時を触っても dirty でない', () => {
		expect(isClockSyncFormDirty(base, { ...settingsToForm(base), hour: 9 })).toBe(false);
	});
	it('有効・種類・時・分の変更は dirty', () => {
		const f = settingsToForm(base);
		expect(isClockSyncFormDirty(base, { ...f, enabled: true })).toBe(true);
		expect(isClockSyncFormDirty(base, { ...f, kind: 'daily' })).toBe(true);
		expect(isClockSyncFormDirty(base, { ...f, minute: 6 })).toBe(true);
	});
	it('検証に通らない入力は dirty', () => {
		expect(isClockSyncFormDirty(base, { ...settingsToForm(base), minute: null })).toBe(true);
	});
});

describe('mapClockUnavailable', () => {
	const body = { status: 'unavailable', reason: 'x' };
	it('404 は用途で文言を分ける', () => {
		expect(mapClockUnavailable('run')(body, 404)?.message).toBe(
			'この接続は収集中の構成にありません（変更を適用してから実行してください）'
		);
		expect(mapClockUnavailable('saved')(body, 404)?.message).toBe('接続が見つかりません');
	});
	it('503 は終了中', () => {
		expect(mapClockUnavailable('run')(body, 503)?.message).toBe(
			'Hub の終了中のため実行しませんでした'
		);
	});
	it.each([
		[{ kind: 'not_found', message: 'm' }, 404],
		[{ status: 'unavailable' }, 404],
		[{ status: 'unavailable', reason: 1 }, 503],
		[body, 500],
		[null, 404],
		['text', 503]
	])('対象外は undefined %j %s', (b, status) => {
		expect(mapClockUnavailable('run')(b, status)).toBeUndefined();
	});
});
