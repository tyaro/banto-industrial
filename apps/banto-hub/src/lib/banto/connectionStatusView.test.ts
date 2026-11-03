import { describe, expect, it } from 'vitest';
import { connectionStatusView } from './connectionStatusView';

describe('connection status uses its owning runtime (#1009)', () => {
	const connection = { id: 7, status: 'stopped' as const, attempt: null };
	it.each([
		['connected', '接続中', 'ok', null],
		['backoff', '再接続待ち', 'warn', 3],
		['error', 'エラー', 'bad', null],
		['stopped', '停止中（収集停止）', 'muted', null],
		['disabled', '無効（構成）', 'muted', null],
		['future-state', 'future-state', 'bad', null]
	] as const)(
		'DB Source %s overrides the stopped PLC broker row',
		(state, label, tone, attempt) => {
			expect(
				connectionStatusView(connection, [{ connection_id: 7, state, consecutive_failures: 3 }], [])
			).toEqual({ label, tone, attempt });
		}
	);
	it('matches by stable id, and preserves PLC/unused and push labels', () => {
		const other = [{ connection_id: 8, state: 'connected', consecutive_failures: 0 }];
		expect(connectionStatusView(connection, other, [])).toEqual({
			label: '停止中',
			tone: 'bad',
			attempt: null
		});
		expect(connectionStatusView({ ...connection, status: 'unused' }, [], []).label).toBe(
			'未使用（タグ未登録）'
		);
		expect(
			connectionStatusView({ ...connection, status: 'reconnecting', attempt: 2 }, [], []).label
		).toBe('再接続中');
		expect(
			connectionStatusView(
				{ ...connection, status: 'reconnecting', attempt: 0 },
				[],
				[{ connection_id: 7 }]
			).label
		).toBe('送り手待ち');
		expect(
			connectionStatusView({ ...connection, status: 'connected' }, [], [{ connection_id: 7 }]).label
		).toBe('受信中');
	});
});
