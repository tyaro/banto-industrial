/**
 * `modeSwitch.ts`（「運用モード」カテゴリ、#623 PR-4）のテスト。文言の決まり（切り替えの間は
 * 収集が止まることを必ず出す・「サービス」「デスクトップ」を前に出さない・自動開始は既定で
 * チェック）と、Tauri のコマンドの呼び方を固定する。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));

import {
	AUTO_START_DEFAULT,
	AUTO_START_LABEL,
	CURRENT_MODE_NOTE,
	OPERATION_MODE_NOTE,
	OPERATION_MODE_TITLE,
	PAUSE_NOTE,
	SETTING_MODE_TITLE,
	SWITCHING_MESSAGE,
	autoStartNote,
	canRequest,
	confirmMessage,
	requestSwitchToOperation
} from './modeSwitch';

beforeEach(() => {
	invokeMock.mockReset();
});

describe('文言', () => {
	it('自動開始は既定でチェックし、推奨と書く', () => {
		expect(AUTO_START_DEFAULT).toBe(true);
		expect(AUTO_START_LABEL).toContain('推奨');
	});

	it('確認の文は、切り替えの間に収集が止まることを必ず含む', () => {
		for (const autoStart of [true, false]) {
			expect(confirmMessage(autoStart)).toContain(PAUSE_NOTE);
		}
		expect(PAUSE_NOTE).toBe('切り替えの間は収集が止まります。');
	});

	it('自動開始のときだけ、管理者の許可を求めると知らせる', () => {
		expect(confirmMessage(true)).toContain('管理者の許可');
		expect(confirmMessage(false)).not.toContain('管理者の許可');
		expect(autoStartNote(true)).toContain('管理者の許可');
		expect(autoStartNote(false)).toContain('運用を再開');
	});

	it('利用者向けの文に「サービス」「デスクトップ」の語を出さない', () => {
		const texts = [
			OPERATION_MODE_TITLE,
			SETTING_MODE_TITLE,
			PAUSE_NOTE,
			AUTO_START_LABEL,
			CURRENT_MODE_NOTE,
			OPERATION_MODE_NOTE,
			SWITCHING_MESSAGE,
			autoStartNote(true),
			autoStartNote(false),
			confirmMessage(true),
			confirmMessage(false)
		];
		for (const text of texts) {
			expect(text).not.toMatch(/サービス|デスクトップ/);
		}
		expect(OPERATION_MODE_TITLE).toBe('このPCで常時記録する（運用モード）');
	});
});

describe('canRequest', () => {
	it('受け付けた後と頼んでいる間は押させない（失敗の後はもう一度押せる）', () => {
		expect(canRequest({ kind: 'idle' })).toBe(true);
		expect(canRequest({ kind: 'failed', message: 'x' })).toBe(true);
		expect(canRequest({ kind: 'requesting' })).toBe(false);
		expect(canRequest({ kind: 'accepted' })).toBe(false);
	});
});

describe('requestSwitchToOperation', () => {
	it('自動開始の指定をそのまま渡す', async () => {
		invokeMock.mockResolvedValue(null);
		await requestSwitchToOperation(true);
		await requestSwitchToOperation(false);
		expect(invokeMock.mock.calls).toEqual([
			['switch_to_operation_mode', { autoStart: true }],
			['switch_to_operation_mode', { autoStart: false }]
		]);
	});

	it('断られたら reject する（理由は呼び出し側が出す）', async () => {
		invokeMock.mockRejectedValue(new Error('管理者の許可を得られませんでした'));
		await expect(requestSwitchToOperation(true)).rejects.toThrow('管理者の許可');
	});
});
