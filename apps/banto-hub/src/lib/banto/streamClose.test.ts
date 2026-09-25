/**
 * `streamClose.ts` のユニットテスト（#441）。
 *
 * 守りたいこと: close `1008` は再接続しない（`session_revoked` /
 * `commissioning_ended` はログイン状態の確認、それ以外は理由の表示）。
 * `1008` 以外は理由文が何であっても従来どおり再接続する。
 * #445: 再接続が続けて失敗したときの確認（いつ確かめるか・結果の扱い）。
 */
import { describe, expect, it } from 'vitest';
import {
	classifySessionCheckResponse,
	classifyStreamClose,
	createSingleFlight,
	decideAfterSessionProbe,
	RECONNECT_FAILURES_BEFORE_SESSION_PROBE,
	REVOKED_CLOSE_CODE,
	shouldProbeSession,
	unknownRevocationMessage,
	type SessionProbeResult,
	type SessionProbeStep,
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

describe('再接続が続けて失敗したときの確認（#445）', () => {
	it('2 回続けて失敗したら確かめる（1 回だけの失敗では確かめない）', () => {
		expect(RECONNECT_FAILURES_BEFORE_SESSION_PROBE).toBe(2);
		expect(shouldProbeSession(0)).toBe(false);
		expect(shouldProbeSession(1)).toBe(false);
		expect(shouldProbeSession(2)).toBe(true);
		expect(shouldProbeSession(3)).toBe(true);
	});

	// [確認を始めたときの失敗の数, 結果が返ったときの数, 確認の結果, 次の扱い]
	const cases: Array<[number, number, SessionProbeResult, SessionProbeStep]> = [
		[2, 2, 'login', { kind: 'recheckSession', reason: 'reconnect_rejected' }],
		[5, 6, 'login', { kind: 'recheckSession', reason: 'reconnect_rejected' }],
		// 有効: 確認の前の失敗は説明済み。次の確認はまた 2 回失敗してから。
		[2, 2, 'session', { kind: 'reconnect', consecutiveFailures: 0, probeAgain: false }],
		[7, 7, 'session', { kind: 'reconnect', consecutiveFailures: 0, probeAgain: false }],
		// 有効でも、確認の最中に起きた失敗は打ち消さない（#447 のレビューで洗い直し）。
		[2, 3, 'session', { kind: 'reconnect', consecutiveFailures: 1, probeAgain: false }],
		[2, 4, 'session', { kind: 'reconnect', consecutiveFailures: 2, probeAgain: true }],
		// 照合できない: 数はそのまま（次の失敗 = 次のバックオフの後にまた確かめる）。
		[2, 2, 'unverified', { kind: 'reconnect', consecutiveFailures: 2, probeAgain: false }],
		[6, 6, 'unverified', { kind: 'reconnect', consecutiveFailures: 6, probeAgain: false }],
		// 確認の最中に失敗した（確認中なので重ねなかった）: すぐにもう一度。
		[2, 3, 'unverified', { kind: 'reconnect', consecutiveFailures: 3, probeAgain: true }]
	];

	it.each(cases)('始め %i 回・終わり %i 回 × 確認 %s', (atStart, now, result, expected) => {
		expect(decideAfterSessionProbe(result, atStart, now)).toEqual(expected);
	});

	it('確認の後の数で、次に確かめるかが決まる（有効なら 2 回待ち、照合できなければ次の失敗で）', () => {
		const afterSession = decideAfterSessionProbe('session', 2, 2);
		const afterUnverified = decideAfterSessionProbe('unverified', 2, 2);
		if (afterSession.kind !== 'reconnect' || afterUnverified.kind !== 'reconnect') {
			throw new Error('reconnect のはず');
		}
		expect(shouldProbeSession(afterSession.consecutiveFailures + 1)).toBe(false);
		expect(shouldProbeSession(afterSession.consecutiveFailures + 2)).toBe(true);
		expect(shouldProbeSession(afterUnverified.consecutiveFailures + 1)).toBe(true);
	});

	it.each(['reconnect_rejected', 'token_cleared'])(
		'クライアントの理由 %s をサーバーの 1008 の理由文としては受け付けない',
		(reason) => {
			expect(classifyStreamClose(1008, reason).kind).toBe('halt');
		}
	);
});

describe('classifySessionCheckResponse（/api/auth/check の分類。check() と同じ、副作用なし）', () => {
	const cases: Array<[number, unknown, SessionProbeResult]> = [
		[200, true, 'session'],
		[200, false, 'login'],
		[401, undefined, 'login'],
		[500, undefined, 'unverified'],
		[503, undefined, 'unverified'],
		[403, undefined, 'unverified'],
		[200, undefined, 'unverified'],
		[200, 'true', 'unverified'],
		[200, null, 'unverified']
	];
	it.each(cases)('%i / 本文 %j → %s', (status, body, expected) => {
		expect(classifySessionCheckResponse(status, body)).toBe(expected);
	});
});

describe('createSingleFlight', () => {
	it('結果を相乗りした全員に返す（#445 の確認の結果）', async () => {
		let runs = 0;
		const once = createSingleFlight(async () => {
			runs += 1;
			return 'login' as const;
		});
		expect(await Promise.all([once(), once()])).toEqual(['login', 'login']);
		expect(runs).toBe(1);
	});

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
