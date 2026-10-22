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
		expect(passwordStatusView({ passwordSet: true, passwordState: 'unreadable' })).toEqual({
			text: 'パスワード: 読めません（この PC で再入力してください）',
			unreadable: true
		});
		expect(PASSWORD_UNREADABLE_MESSAGE).toContain('読めません');
	});

	it('uses passwordState when present', () => {
		expect(passwordStatusView({ passwordSet: true, passwordState: 'set' }).text).toBe(
			'パスワード: 設定済み'
		);
		expect(passwordStatusView({ passwordSet: false, passwordState: 'none' }).text).toBe(
			'パスワード: 未設定'
		);
	});

	it('falls back to passwordSet for old responses without passwordState', () => {
		expect(passwordStatusView({ passwordSet: true })).toEqual({
			text: 'パスワード: 設定済み',
			unreadable: false
		});
		expect(passwordStatusView({ passwordSet: false }).text).toBe('パスワード: 未設定');
	});

	it('is 未設定 and not a warning without a connection', () => {
		expect(passwordStatusView(null)).toEqual({ text: 'パスワード: 未設定', unreadable: false });
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
