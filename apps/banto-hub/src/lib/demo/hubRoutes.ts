/**
 * Hub の口の模擬（設計 docs/demo-pages-design.md §3.3）。今は `/tags` を開いたときの起動で
 * 呼ばれる口に、空の画面が出る応答を返すだけ（登録・値・履歴は後の PR）。
 *
 * どの口が要るかは、デモのビルドで `/tags` を開き、模擬の「口がありません」の警告から
 * 拾った（2026-11-22）。応答の型は各 `*Admin.ts` から取る（模擬の側で書き直さない）。
 */
import { json, type DemoHttpRoute } from '@banto-industrial/demo-backend';
import type { CommissioningStatus } from '#lib/banto/commissioning.js';
import type { RawStatusResponse } from '#lib/banto/hubStatus.js';
import type { PendingChange } from '#lib/banto/pendingChangesAdmin.js';
import type {
	CollectionGroup,
	DriverInfo,
	PlcConnection,
	Tag
} from '#lib/banto/tagRegistryAdmin.js';

/** `GET /api/status` の空の Hub（接続 0・収集は動いている・外部への出力は無効）。 */
function emptyStatus(): RawStatusResponse {
	return {
		version: 'demo',
		revision: 0,
		lastConfigError: null,
		connections: [],
		writeEnabled: false,
		writeWasEnabledBeforeRestart: false,
		mqtt: { enabled: false, connected: false },
		grpc: { enabled: false, port: 0 },
		collectionState: 'running',
		collectionMode: 'configured',
		lastRuntimeError: null,
		system: {
			cpuPercent: 0,
			processMemoryBytes: 0,
			hostMemoryUsedBytes: 0,
			hostMemoryTotalBytes: 0
		},
		dbSource: [],
		ingest: [],
		sink: { sidecar: { state: 'unknown', lastSeenAt: null }, groups: [] }
	};
}

export function createHubRoutes(): DemoHttpRoute[] {
	const plcConnections: PlcConnection[] = [];
	const collectionGroups: CollectionGroup[] = [];
	const tags: Tag[] = [];
	const drivers: DriverInfo[] = [];
	const pendingChanges: PendingChange[] = [];
	// ログイン済みの利用者として入るので、試運転（ロックダウン前）の表示は出さない。
	const commissioning: CommissioningStatus = { lockedDown: true };
	return [
		{ method: 'GET', path: '/api/plc-connections', handle: () => json(plcConnections) },
		{ method: 'GET', path: '/api/collection-groups', handle: () => json(collectionGroups) },
		{ method: 'GET', path: '/api/tags', handle: () => json(tags) },
		{ method: 'GET', path: '/api/drivers', handle: () => json(drivers) },
		{ method: 'GET', path: '/api/pending-changes', handle: () => json(pendingChanges) },
		{ method: 'GET', path: '/api/commissioning/status', handle: () => json(commissioning) },
		{ method: 'GET', path: '/api/status', handle: () => json(emptyStatus()) }
	];
}
