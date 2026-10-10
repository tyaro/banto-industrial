import { describe, expect, it } from 'vitest';
import {
	AUDIT_SPOOL_UNREADABLE_MESSAGE,
	auditSpoolNotice,
	auditSpoolWarning
} from './auditSpoolWarning';

const none = { pendingCount: 0, pendingOldestTs: null, droppedCount: 0, failedCount: 0 };

describe('auditSpoolWarning', () => {
	it('is null when nothing is pending or lost', () => {
		expect(auditSpoolWarning(none)).toBeNull();
	});

	it('names the pending count and the oldest time', () => {
		const text = auditSpoolWarning({
			...none,
			pendingCount: 3,
			pendingOldestTs: '2026-10-10 01:02:03'
		});
		expect(text).toContain('3 件');
		expect(text).toContain('2026-10-10 01:02:03 UTC');
		expect(text).toContain('自動で監査ログに書き込みます');
	});

	it('omits the oldest time when it is unknown', () => {
		const text = auditSpoolWarning({ ...none, pendingCount: 1 });
		expect(text).toContain('1 件');
		expect(text).not.toContain('最も古い');
	});

	it('reports dropped and failed entries even with nothing pending', () => {
		const text = auditSpoolWarning({ ...none, droppedCount: 2, failedCount: 5 });
		expect(text).toContain('上限（10,000 件）');
		expect(text).toContain('2 件');
		expect(text).toContain('保留ファイルにも書けなかった');
		expect(text).toContain('5 件');
	});
});

describe('auditSpoolNotice', () => {
	it('is null when the status was read and nothing is pending', () => {
		expect(auditSpoolNotice({ kind: 'ok', status: none })).toBeNull();
	});

	it('shows the backlog warning when the status was read', () => {
		const text = auditSpoolNotice({ kind: 'ok', status: { ...none, pendingCount: 2 } });
		expect(text).toContain('2 件');
	});

	it('says the status could not be read, which is not the same as zero', () => {
		const text = auditSpoolNotice({ kind: 'failed' });
		expect(text).toBe(AUDIT_SPOOL_UNREADABLE_MESSAGE);
		expect(text).toContain('読めませんでした');
		expect(text).not.toBeNull();
	});
});
