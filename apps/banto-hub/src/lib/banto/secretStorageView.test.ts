/**
 * R2b-1e: `secretStorageView.ts`（秘密値まわりの表示文言）のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import {
	PASSWORD_UNREADABLE_MESSAGE,
	passwordStatusView,
	secretStorageWarnings
} from './secretStorageView';

describe('passwordStatusView', () => {
	it('warns when the password cannot be read', () => {
		expect(passwordStatusView('unreadable')).toEqual({
			text: 'パスワード: 読めません（この PC で再入力してください）',
			unreadable: true
		});
		expect(PASSWORD_UNREADABLE_MESSAGE).toContain('読めません');
	});

	it('reads the $secret state: set / none', () => {
		expect(passwordStatusView('set')).toEqual({ text: 'パスワード: 設定済み', unreadable: false });
		expect(passwordStatusView('none').text).toBe('パスワード: 未設定');
	});

	it('is 未設定 and not a warning without a state', () => {
		expect(passwordStatusView(null)).toEqual({ text: 'パスワード: 未設定', unreadable: false });
		expect(passwordStatusView(undefined).unreadable).toBe(false);
	});
});

describe('secretStorageWarnings', () => {
	it('is empty when the entry is missing (old server) or has no warnings', () => {
		expect(secretStorageWarnings(undefined)).toEqual([]);
		expect(secretStorageWarnings(null)).toEqual([]);
		expect(
			secretStorageWarnings({ plaintext_warning: null, plaintext_remaining_warning: null })
		).toEqual([]);
	});

	it('returns only the non-null warnings, plaintext first', () => {
		expect(
			secretStorageWarnings({
				plaintext_warning: '秘密値を平文で保存しています（Windows 以外）',
				plaintext_remaining_warning: null
			})
		).toEqual(['秘密値を平文で保存しています（Windows 以外）']);
		expect(
			secretStorageWarnings({
				plaintext_warning: null,
				plaintext_remaining_warning: '移行待ちの秘密値が平文で残っています（3 件）'
			})
		).toEqual(['移行待ちの秘密値が平文で残っています（3 件）']);
		expect(
			secretStorageWarnings({ plaintext_warning: 'a', plaintext_remaining_warning: 'b' })
		).toEqual(['a', 'b']);
	});
});
