/**
 * `streamClose.ts` のユニットテスト（#441）。
 *
 * 守りたいこと: close `1008` は再接続しない（`session_revoked` /
 * `commissioning_ended` はログイン状態の確認、それ以外は理由の表示）。
 * `1008` 以外は理由文が何であっても従来どおり再接続する。
 */
import { describe, expect, it } from 'vitest';
import {
	classifyStreamClose,
	createSingleFlight,
	REVOKED_CLOSE_CODE,
	unknownRevocationMessage,
	type StreamCloseAction
} from './streamClose';

describe('classifyStreamClose', () => {
	const cases: Array<[number, string, StreamCloseAction['kind'], string?]> = [
		// [close コード, 理由文, 扱い, halt のときの理由文]
		[1008, 'session_revoked', 'recheckSession'],
		[1008, 'commissioning_ended', 'recheckSession'],
		[1008, 'api_key_revoked', 'halt', 'api_key_revoked'],
		[1008, 'api_key_expired', 'halt', 'api_key_expired'],
		[1008, 'api_key_tripped', 'halt', 'api_key_tripped'],
		[1008, 'api_key_not_found', 'halt', 'api_key_not_found'],
		[1008, 'something_new', 'halt', 'something_new'],
		[1008, '', 'halt', ''],
		// 大文字小文字や前後の空白は別の理由文（サーバーは固定の小文字を送る）。
		[1008, 'Session_Revoked', 'halt', 'Session_Revoked'],
		[1006, '', 'reconnect'],
		[1000, '', 'reconnect'],
		[1001, '', 'reconnect'],
		[1011, '', 'reconnect'],
		[1013, '', 'reconnect'],
		// 1008 以外は理由文が失効を示していても再接続（コードで判断する）。
		[1006, 'session_revoked', 'reconnect'],
		[1000, 'api_key_revoked', 'reconnect']
	];

	it.each(cases)('close %i / 理由 "%s" は %s', (code, reason, kind, haltReason) => {
		const action = classifyStreamClose(code, reason);
		expect(action.kind).toBe(kind);
		if (action.kind === 'recheckSession') expect(action.reason).toBe(reason);
		if (action.kind === 'halt') {
			expect(action.reason).toBe(haltReason);
			expect(action.message).not.toBe('');
		}
	});

	it('REVOKED_CLOSE_CODE はサーバー（stream.rs）と同じ 1008', () => {
		expect(REVOKED_CLOSE_CODE).toBe(1008);
	});

	it('api_key_* はそれぞれ別の説明を出す', () => {
		const messages = [
			'api_key_revoked',
			'api_key_expired',
			'api_key_tripped',
			'api_key_not_found'
		].map((reason) => {
			const action = classifyStreamClose(1008, reason);
			return action.kind === 'halt' ? action.message : '';
		});
		expect(new Set(messages).size).toBe(4);
		for (const message of messages) expect(message).toContain('API キー');
	});

	it('未知の理由文は理由文をそのまま画面に添える（空なら記載なしと出す）', () => {
		const action = classifyStreamClose(1008, 'something_new');
		expect(action).toEqual({
			kind: 'halt',
			reason: 'something_new',
			message: unknownRevocationMessage('something_new')
		});
		expect(unknownRevocationMessage('something_new')).toContain('something_new');
		expect(unknownRevocationMessage('')).toContain('理由の記載なし');
	});

	it('Object のプロトタイプの名前を理由文にしても api_key の説明に化けない', () => {
		const action = classifyStreamClose(1008, 'constructor');
		expect(action).toEqual({
			kind: 'halt',
			reason: 'constructor',
			message: unknownRevocationMessage('constructor')
		});
	});
});

describe('createSingleFlight', () => {
	it('実行中の呼び出しには相乗りし、1 回しか実行しない', async () => {
		let runs = 0;
		let release: () => void = () => {};
		const once = createSingleFlight(async () => {
			runs += 1;
			await new Promise<void>((resolve) => {
				release = resolve;
			});
		});
		const a = once();
		const b = once();
		const c = once();
		expect(a).toBe(b);
		expect(b).toBe(c);
		release();
		await Promise.all([a, b, c]);
		expect(runs).toBe(1);
	});

	it('終わった後の呼び出しは新しく実行する（失敗の後も）', async () => {
		let runs = 0;
		const once = createSingleFlight(async () => {
			runs += 1;
			if (runs === 1) throw new Error('boom');
		});
		await expect(once()).rejects.toThrow('boom');
		await once();
		expect(runs).toBe(2);
	});
});
