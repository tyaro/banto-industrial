/**
 * `storeRetentionForm.ts` のユニットテスト（`registryCascadeImpact.test.ts`
 * と同じスタイル、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	MAX_RETENTION_DAYS,
	formToRetentionDays,
	formatPruneConfirmMessage,
	formatPruneDoneMessage,
	formatRetentionSavedMessage,
	hasUnsavedRetentionChange,
	pruneDisabledReason,
	retentionDaysToForm,
	validateRetentionForm,
	type RetentionFormState
} from './storeRetentionForm';

describe('validateRetentionForm', () => {
	it('accepts any days value when unlimited is selected', () => {
		expect(validateRetentionForm({ unlimited: true, days: -5 })).toBeNull();
		expect(validateRetentionForm({ unlimited: true, days: 0 })).toBeNull();
	});

	it('accepts an integer within 1..=MAX_RETENTION_DAYS', () => {
		expect(validateRetentionForm({ unlimited: false, days: 1 })).toBeNull();
		expect(validateRetentionForm({ unlimited: false, days: 7 })).toBeNull();
		expect(validateRetentionForm({ unlimited: false, days: MAX_RETENTION_DAYS })).toBeNull();
	});

	it('rejects 0, negative, non-integer, and over-the-limit values', () => {
		for (const days of [0, -1, 1.5, MAX_RETENTION_DAYS + 1]) {
			const error = validateRetentionForm({ unlimited: false, days });
			expect(error, `days=${days} should be rejected`).not.toBeNull();
		}
	});
});

describe('formToRetentionDays / retentionDaysToForm', () => {
	it('unlimited maps to null and back (with a fallback days value)', () => {
		const form: RetentionFormState = { unlimited: true, days: 30 };
		expect(formToRetentionDays(form)).toBeNull();
		expect(retentionDaysToForm(null, 7)).toEqual({ unlimited: true, days: 7 });
	});

	it('a finite value round-trips as-is', () => {
		const form: RetentionFormState = { unlimited: false, days: 14 };
		expect(formToRetentionDays(form)).toBe(14);
		expect(retentionDaysToForm(14, 7)).toEqual({ unlimited: false, days: 14 });
	});
});

describe('hasUnsavedRetentionChange / pruneDisabledReason', () => {
	it('is false when the form matches the saved finite policy', () => {
		expect(hasUnsavedRetentionChange(7, { unlimited: false, days: 7 })).toBe(false);
		expect(pruneDisabledReason(false)).toBeNull();
	});

	it('is false when the form matches the saved unlimited policy', () => {
		expect(hasUnsavedRetentionChange(null, { unlimited: true, days: 999 })).toBe(false);
	});

	it('is true when the form days differ from the saved value', () => {
		expect(hasUnsavedRetentionChange(7, { unlimited: false, days: 30 })).toBe(true);
		expect(pruneDisabledReason(true)).toContain('先に保存してください');
	});

	it('is true when switching between finite and unlimited without saving', () => {
		expect(hasUnsavedRetentionChange(7, { unlimited: true, days: 7 })).toBe(true);
		expect(hasUnsavedRetentionChange(null, { unlimited: false, days: 7 })).toBe(true);
	});
});

describe('formatPruneConfirmMessage', () => {
	it('states the count and that the deletion is irreversible', () => {
		const message = formatPruneConfirmMessage(12);
		expect(message).toContain('12件');
		expect(message).toContain('戻せません');
	});
});

describe('toast copy', () => {
	it('the saved message makes clear saving is not immediate pruning', () => {
		const message = formatRetentionSavedMessage();
		expect(message).toContain('保存しました');
		expect(message).toContain('次回の自動剪定');
	});

	it('the prune-done message states the deleted count', () => {
		expect(formatPruneDoneMessage(3)).toContain('3件');
	});
});
