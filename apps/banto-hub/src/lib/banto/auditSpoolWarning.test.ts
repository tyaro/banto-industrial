/**
 * #437: `auditSpoolWarning.ts`（状態画面の監査の保留の注意書き）のユニットテスト。
 */
import { describe, expect, it } from 'vitest';
import { auditSpoolWarning, type AuditSpoolStatus } from './auditSpoolWarning';

const empty: AuditSpoolStatus = {
	audit_pending_count: 0,
	audit_pending_oldest_ts: null,
	audit_dropped_count: 0,
	audit_failed_count: 0
};

describe('auditSpoolWarning', () => {
	it('is null when nothing is spooled or lost', () => {
		expect(auditSpoolWarning(empty)).toBeNull();
	});

	it('states the pending count and the oldest time', () => {
		expect(
			auditSpoolWarning({
				...empty,
				audit_pending_count: 2,
				audit_pending_oldest_ts: '2026-10-10 01:02:03'
			})
		).toBe(
			'DB に書けなかった監査 2 件をデータディレクトリに保留しています（最も古いもの: 2026-10-10 01:02:03 UTC）。DB が戻ると自動で監査ログに書き込みます。'
		);
	});

	it('omits the oldest time when the server sent none', () => {
		expect(auditSpoolWarning({ ...empty, audit_pending_count: 1 })).toBe(
			'DB に書けなかった監査 1 件をデータディレクトリに保留しています。DB が戻ると自動で監査ログに書き込みます。'
		);
	});

	it('reports lost entries even when nothing is pending any more', () => {
		expect(auditSpoolWarning({ ...empty, audit_dropped_count: 3, audit_failed_count: 1 })).toBe(
			'保留の上限（10,000 件）を超えたため、監査 3 件を記録できませんでした。保留ファイルにも書けなかったため、監査 1 件を記録できませんでした。'
		);
	});
});
