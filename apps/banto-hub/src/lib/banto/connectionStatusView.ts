/** The connection list must read the runtime owning each connection, not the PLC broker for DB Source. */
import type { ConnectionStatusEntry, DbSourceStatusEntry, IngestStatusEntry } from './hubStatus';
import { pushConnectionStatusLabel } from './ingestStatusView';

export interface ConnectionStatusView {
	label: string;
	tone: 'ok' | 'warn' | 'muted' | 'bad';
	attempt: number | null;
}

export function connectionStatusView(
	connection: Pick<ConnectionStatusEntry, 'id' | 'status' | 'attempt'>,
	dbSources: readonly Pick<
		DbSourceStatusEntry,
		'connection_id' | 'state' | 'consecutive_failures'
	>[],
	ingest: readonly Pick<IngestStatusEntry, 'connection_id'>[]
): ConnectionStatusView {
	const db = dbSources.find((source) => source.connection_id === connection.id);
	if (db) {
		const labels: Record<string, string> = {
			connected: '接続中',
			backoff: '再接続待ち',
			error: 'エラー',
			stopped: '停止中（収集停止）',
			disabled: '無効（構成）'
		};
		return {
			label: labels[db.state] ?? db.state,
			tone:
				db.state === 'connected'
					? 'ok'
					: db.state === 'backoff'
						? 'warn'
						: db.state === 'stopped' || db.state === 'disabled'
							? 'muted'
							: 'bad',
			attempt: db.state === 'backoff' ? db.consecutive_failures : null
		};
	}
	const labels: Record<string, string> = {
		connected: '接続中',
		reconnecting: '再接続中',
		stopped: '停止中',
		unused: '未使用（タグ未登録）'
	};
	const isPush = ingest.some((source) => source.connection_id === connection.id);
	return {
		label:
			(isPush ? pushConnectionStatusLabel(connection.status) : null) ??
			labels[connection.status] ??
			connection.status,
		tone:
			connection.status === 'connected'
				? 'ok'
				: connection.status === 'reconnecting'
					? 'warn'
					: connection.status === 'unused'
						? 'muted'
						: 'bad',
		attempt: connection.attempt
	};
}
