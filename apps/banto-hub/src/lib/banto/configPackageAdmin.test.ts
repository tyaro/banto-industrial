/**
 * 監査③（2026-08-12）是正のユニットテスト: `applyConfigPackage`
 * （`configPackageAdmin.ts`）が、import ループ中に
 * `QueuedWhileRunningError`（収集稼働中に mutating エンドポイントが 202
 * queued を返した場合 - `tagRegistryAdmin.ts` 参照）を検知したとき、
 * 未解決の warning を積んで続行する（旧・サイレントスキップ）のではなく
 * `ConfigPackageImportAbortedError` で reject して import 全体を中断した
 * ことを呼び出し元に伝えることを固定する。
 *
 * `./tagRegistryAdmin`/`./grpcSettingsAdmin`/`./mqttSettingsAdmin` を
 * `vi.mock` で丸ごと差し替え、実 HTTP も `@banto/admin-core`（Svelte rune
 * 依存で最小 vitest 構成では読み込めない - `tagRegistryAdmin.test.ts` の
 * doc comment参照）も一切経由しない。`configPackageAdmin.ts` は
 * `isQueuedWhileRunningError` を `./tagRegistryAdmin` から import して自ら
 * 呼ぶため、モックの `isQueuedWhileRunningError` はモックの
 * `QueuedWhileRunningError` インスタンスだけを判別できれば十分
 * （このテストが検証したいのは `configPackageAdmin.ts` 自身の
 * catch/変換ロジックであって、`tagRegistryAdmin.ts` の型ガード実装その
 * ものは `tagRegistryAdmin.test.ts` 側で別途固定済み）。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import type { ConfigPackage } from './configPackage';

// `vi.mock` ファクトリはファイル先頭に hoist されるため、参照するクラスは
// `vi.hoisted` で一緒に巻き上げる必要がある（通常の top-level const/class
// 宣言だと TDZ で `Cannot access before initialization` になる）。
const { MockQueuedWhileRunningError, isMockQueuedWhileRunningError } = vi.hoisted(() => {
	class MockQueuedWhileRunningError extends Error {
		constructor(message: string) {
			super(message);
			this.name = 'QueuedWhileRunningError';
		}
	}
	function isMockQueuedWhileRunningError(error: unknown): error is MockQueuedWhileRunningError {
		return error instanceof MockQueuedWhileRunningError;
	}
	return { MockQueuedWhileRunningError, isMockQueuedWhileRunningError };
});

vi.mock('./tagRegistryAdmin', () => ({
	QueuedWhileRunningError: MockQueuedWhileRunningError,
	isQueuedWhileRunningError: isMockQueuedWhileRunningError,
	isVirtualConnection: (c: { protocol: string }) => c.protocol === 'virtual',
	listPlcConnections: vi.fn(),
	createPlcConnection: vi.fn(),
	updatePlcConnection: vi.fn(),
	listCollectionGroups: vi.fn(),
	createCollectionGroup: vi.fn(),
	updateCollectionGroup: vi.fn(),
	listTags: vi.fn(),
	createTag: vi.fn(),
	updateTag: vi.fn()
}));

vi.mock('./grpcSettingsAdmin', () => ({
	getGrpcSettings: vi.fn(),
	saveGrpcSettings: vi.fn()
}));

vi.mock('./mqttSettingsAdmin', () => ({
	getMqttSettings: vi.fn(),
	saveMqttSettings: vi.fn()
}));

// S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6）: 上の
// `tagRegistryAdmin`/`grpcSettingsAdmin`/`mqttSettingsAdmin` と同じ理由
// （`@banto/admin-core`を経由させない）でモックする。
vi.mock('./sinkGroupsAdmin', () => ({
	listSinkGroups: vi.fn(),
	createSinkGroup: vi.fn(),
	updateSinkGroup: vi.fn(),
	deleteSinkGroup: vi.fn()
}));

import {
	listPlcConnections,
	createPlcConnection,
	updatePlcConnection,
	listCollectionGroups,
	createCollectionGroup,
	updateCollectionGroup,
	listTags,
	createTag,
	updateTag
} from './tagRegistryAdmin';
import { getGrpcSettings, saveGrpcSettings } from './grpcSettingsAdmin';
import { getMqttSettings, saveMqttSettings } from './mqttSettingsAdmin';
import { listSinkGroups, createSinkGroup, updateSinkGroup } from './sinkGroupsAdmin';
import {
	applyConfigPackage,
	inspectConfigPackage,
	isConfigPackageImportAbortedError,
	ConfigPackageImportAbortedError,
	configPackageExportFilename
} from './configPackageAdmin';

const pkg: ConfigPackage = {
	schemaVersion: 1,
	product: 'banto-hub',
	exportedAt: '2026-08-12T00:00:00.000Z',
	excludedSecrets: [],
	plcConnections: [
		{
			name: 'plc1',
			protocol: 'modbus-tcp',
			host: '192.168.11.200',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high'
		}
	],
	collectionGroups: [
		{
			name: 'group1',
			plcConnectionName: 'plc1',
			periodMs: 1000,
			enabled: true,
			defaultWritable: true
		}
	],
	tags: [
		{
			name: 'tag1',
			collectionGroupName: 'group1',
			address: 'D3000',
			dataType: 'i16',
			stringLength: null,
			stringEncoding: 'utf8',
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
			unit: null,
			decimals: 0,
			thresholdH: null,
			thresholdHh: null,
			thresholdL: null,
			thresholdLl: null,
			enabled: true,
			writable: false,
			tagKind: 'plc',
			expression: null,
			retain: false
		}
	],
	sinkGroups: [],
	mqtt: {
		enabled: false,
		host: '',
		port: 1883,
		clientId: 'banto-hub',
		prefix: 'banto',
		qos: 1,
		minIntervalMs: 1000
	},
	grpc: { enabled: false, bind: '127.0.0.1', port: 50051 }
};

const currentMqtt = {
	enabled: false,
	host: '',
	port: 1883,
	clientId: 'banto-hub',
	username: null,
	prefix: 'banto',
	qos: 1 as const,
	minIntervalMs: 1000
};

beforeEach(() => {
	vi.mocked(listPlcConnections).mockReset().mockResolvedValue([]);
	vi.mocked(createPlcConnection).mockReset();
	vi.mocked(updatePlcConnection).mockReset();
	vi.mocked(listCollectionGroups).mockReset().mockResolvedValue([]);
	vi.mocked(createCollectionGroup).mockReset();
	vi.mocked(updateCollectionGroup).mockReset();
	vi.mocked(listTags).mockReset().mockResolvedValue([]);
	vi.mocked(createTag).mockReset();
	vi.mocked(updateTag).mockReset();
	vi.mocked(listSinkGroups).mockReset().mockResolvedValue([]);
	vi.mocked(createSinkGroup).mockReset();
	vi.mocked(updateSinkGroup).mockReset();
	vi.mocked(getGrpcSettings)
		.mockReset()
		.mockResolvedValue({ enabled: false, bind: '127.0.0.1', port: 50051 });
	vi.mocked(saveGrpcSettings)
		.mockReset()
		.mockResolvedValue({ enabled: false, bind: '127.0.0.1', port: 50051 });
	vi.mocked(getMqttSettings).mockReset().mockResolvedValue(currentMqtt);
	vi.mocked(saveMqttSettings)
		.mockReset()
		.mockResolvedValue({ ...currentMqtt, username: null });
});

describe('applyConfigPackage: 収集稼働中の QueuedWhileRunningError を検知した場合', () => {
	it('createPlcConnection が QueuedWhileRunningError を投げたら ConfigPackageImportAbortedError で reject する（summary を resolve しない）', async () => {
		vi.mocked(createPlcConnection).mockRejectedValue(
			new MockQueuedWhileRunningError('収集稼働中のためキュー投入されました')
		);

		await expect(applyConfigPackage(pkg)).rejects.toSatisfy((err: unknown) => {
			expect(isConfigPackageImportAbortedError(err)).toBe(true);
			expect(err).toBeInstanceOf(ConfigPackageImportAbortedError);
			expect((err as Error).message).toBe(
				'収集が稼働中のため構成パッケージの取り込みを中断しました。収集を停止してから再実行してください。'
			);
			return true;
		});
	});

	it('updateCollectionGroup が QueuedWhileRunningError を投げても同様に中断する（ループ中どこで起きても検知する）', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'plc1',
				protocol: 'modbus-tcp',
				host: '192.168.11.200',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: null,
				username: null,
				passwordSet: false
			}
		]);
		vi.mocked(updatePlcConnection).mockResolvedValue({
			id: 1,
			name: 'plc1',
			protocol: 'modbus-tcp',
			host: '192.168.11.200',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: null,
			username: null,
			passwordSet: false
		});
		vi.mocked(listCollectionGroups).mockResolvedValue([
			{
				id: 1,
				name: 'group1',
				plcConnectionId: 1,
				periodMs: 500,
				enabled: true,
				defaultWritable: true,
				querySql: null
			}
		]);
		vi.mocked(updateCollectionGroup).mockRejectedValue(
			new MockQueuedWhileRunningError('収集稼働中のためキュー投入されました')
		);

		await expect(applyConfigPackage(pkg)).rejects.toSatisfy((err: unknown) => {
			expect(isConfigPackageImportAbortedError(err)).toBe(true);
			return true;
		});
	});

	it('QueuedWhileRunningError 以外のエラーはそのまま素通しする（無関係なエラーを誤って ConfigPackageImportAbortedError に変換しない）', async () => {
		const boom = new Error('boom');
		vi.mocked(createPlcConnection).mockRejectedValue(boom);

		await expect(applyConfigPackage(pkg)).rejects.toBe(boom);
	});
});

describe('configPackageExportFilename（T19 S4、UX-42: settings/+page.svelte ローカル関数から移設）', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	it('banto-hub-config-YYYY-MM-DD.json 形式で、月・日をゼロ埋めする', () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(2026, 0, 5, 12, 0, 0));

		expect(configPackageExportFilename()).toBe('banto-hub-config-2026-01-05.json');
	});

	it('2桁の月・日でもゼロ埋めが二重にならない', () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date(2026, 10, 23, 0, 0, 0));

		expect(configPackageExportFilename()).toBe('banto-hub-config-2026-11-23.json');
	});
});

describe('applyConfigPackage: 全件成功する通常の import（回帰ガード）', () => {
	it('summary（ConfigPackageImportSummary）を resolve する', async () => {
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'plc1',
			protocol: 'modbus-tcp',
			host: '192.168.11.200',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: null,
			username: null,
			passwordSet: false
		});
		vi.mocked(createCollectionGroup).mockResolvedValue({
			id: 1,
			name: 'group1',
			plcConnectionId: 1,
			periodMs: 1000,
			enabled: true,
			defaultWritable: true,
			querySql: null
		});
		vi.mocked(createTag).mockResolvedValue({
			id: 1,
			name: 'tag1',
			collectionGroupId: 1,
			address: 'D3000',
			dataType: 'i16',
			stringLength: null,
			stringEncoding: 'utf8',
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
			unit: null,
			decimals: 0,
			thresholdH: null,
			thresholdHh: null,
			thresholdL: null,
			thresholdLl: null,
			enabled: true,
			writable: false,
			tagKind: 'plc',
			expression: null,
			retain: false,
			revision: 1
		});

		const summary = await applyConfigPackage(pkg);
		expect(summary.counts.plcConnections).toEqual({ create: 1, update: 0 });
		expect(summary.counts.collectionGroups).toEqual({ create: 1, update: 0 });
		expect(summary.counts.tags).toEqual({ create: 1, update: 0 });
		expect(summary.mqttApplied).toBe(true);
		expect(summary.grpcApplied).toBe(true);
		expect(summary.warnings).toEqual([]);
	});

	// stringEncoding が config package のバックアップ/復元経路だけ未対応で、
	// shift_jis のタグを復元すると暗黙的に utf8 へ戻ってしまう潜在バグの
	// 回帰ガード（2026-09-05）: pkg.tags[].stringEncoding が createTag に渡す
	// TagInput までそのまま伝播することを固定する。
	it('tag の stringEncoding が createTag に渡す TagInput にそのまま伝播する（shift_jis が utf8 に落ちない）', async () => {
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'plc1',
			protocol: 'modbus-tcp',
			host: '192.168.11.200',
			port: 502,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: null,
			username: null,
			passwordSet: false
		});
		vi.mocked(createCollectionGroup).mockResolvedValue({
			id: 1,
			name: 'group1',
			plcConnectionId: 1,
			periodMs: 1000,
			enabled: true,
			defaultWritable: true,
			querySql: null
		});
		vi.mocked(createTag).mockResolvedValue({
			id: 1,
			name: 'tag1',
			collectionGroupId: 1,
			address: 'D3000',
			dataType: 'i16',
			stringLength: null,
			stringEncoding: 'shift_jis',
			rawLo: null,
			rawHi: null,
			engLo: null,
			engHi: null,
			unit: null,
			decimals: 0,
			thresholdH: null,
			thresholdHh: null,
			thresholdL: null,
			thresholdLl: null,
			enabled: true,
			writable: false,
			tagKind: 'plc',
			expression: null,
			retain: false,
			revision: 1
		});

		const pkgWithShiftJis: ConfigPackage = {
			...pkg,
			tags: pkg.tags.map((tag) => ({ ...tag, stringEncoding: 'shift_jis' }))
		};

		await applyConfigPackage(pkgWithShiftJis);

		expect(createTag).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'tag1', stringEncoding: 'shift_jis' })
		);
	});
});

// --- S1（docs/banto-hub-external-db-design.md §4.1・§2.2、実装指示5）:
// 構成パッケージは postgres 接続のパスワードを一切含まない
// （`configPackage.test.ts`側で export 自体を固定済み）ので、import 経路
// （`applyConfigPackage`）が実際に「パスワード無しで作成する」呼び出しに
// なることをここで固定する。

describe('applyConfigPackage: postgres（DB Source）接続の import はパスワード無しで作成する', () => {
	it('createPlcConnection にはパッケージが持つ database/username だけを渡し、password キー自体を渡さない', async () => {
		const pkgWithDbConnection: ConfigPackage = {
			...pkg,
			plcConnections: [
				{
					name: 'pg1',
					protocol: 'postgres',
					host: '10.0.0.5',
					port: 5432,
					unitId: 1,
					enabled: true,
					simulation: false,
					wordOrder: 'low_high',
					database: 'appdb',
					username: 'appuser'
				}
			],
			// このパッケージの postgres 接続にグループを繋げると
			// `applyConfigPackageInner`が接続解決に成功してしまい紛らわしい
			// ので、この回帰テストでは接続だけを見る（グループ/タグは空）。
			collectionGroups: [],
			tags: []
		};
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'pg1',
			protocol: 'postgres',
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: 'appdb',
			username: 'appuser',
			passwordSet: false
		});

		await applyConfigPackage(pkgWithDbConnection);

		expect(createPlcConnection).toHaveBeenCalledTimes(1);
		const calledWith = vi.mocked(createPlcConnection).mock.calls[0][0];
		expect(calledWith).toEqual({
			name: 'pg1',
			protocol: 'postgres',
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: 'appdb',
			username: 'appuser'
		});
		expect(calledWith).not.toHaveProperty('password');
	});
});

// --- S3（docs/banto-hub-external-db-design.md §7 row S3）: 収集グループの
// `querySql`（postgres 配下限定）が import 経路（`applyConfigPackage`）で
// `createCollectionGroup`/`updateCollectionGroup` へそのまま渡ることを固定
// する - S1 の database/username と同じ「pkg の値を転記するだけ」の経路。

describe('applyConfigPackage: postgres（DB Source）グループの querySql を CollectionGroupInput へ転記する', () => {
	it('createCollectionGroup には querySql をそのまま渡す', async () => {
		const pkgWithDbGroup: ConfigPackage = {
			...pkg,
			plcConnections: [
				{
					name: 'pg1',
					protocol: 'postgres',
					host: '10.0.0.5',
					port: 5432,
					unitId: 1,
					enabled: true,
					simulation: false,
					wordOrder: 'low_high',
					database: 'appdb',
					username: 'appuser'
				}
			],
			collectionGroups: [
				{
					name: 'group-pg',
					plcConnectionName: 'pg1',
					periodMs: 1000,
					enabled: true,
					defaultWritable: false,
					querySql: 'SELECT id, temperature FROM sensors'
				}
			],
			tags: []
		};
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'pg1',
			protocol: 'postgres',
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: 'appdb',
			username: 'appuser',
			passwordSet: false
		});
		vi.mocked(createCollectionGroup).mockResolvedValue({
			id: 1,
			name: 'group-pg',
			plcConnectionId: 1,
			periodMs: 1000,
			enabled: true,
			defaultWritable: false,
			querySql: 'SELECT id, temperature FROM sensors'
		});

		await applyConfigPackage(pkgWithDbGroup);

		expect(createCollectionGroup).toHaveBeenCalledWith(
			expect.objectContaining({
				name: 'group-pg',
				plcConnectionId: 1,
				querySql: 'SELECT id, temperature FROM sensors'
			})
		);
	});
});

// --- 2026-09 レビュー是正: `dbConnectionsPasswordRequired` の過剰報告修正 ---
// apply 側は既存接続を `password` 省略で update すると保存済みパスワードを
// 維持する（S1a tri-state）ため、既に `passwordSet: true` の既存 postgres
// 接続を更新するだけの場合は再入力不要 - 通知は「新規作成される postgres
// 接続」と「既存 postgres 接続で現在 `passwordSet: false` のもの」だけに
// 絞られることを固定する（`configPackageAdmin.ts` の `inspectConfigPackage`
// 参照）。

describe('inspectConfigPackage: dbConnectionsPasswordRequired は再設定が必要な postgres 接続だけを列挙する', () => {
	function pkgWithConnections(connections: ConfigPackage['plcConnections']): ConfigPackage {
		return { ...pkg, plcConnections: connections, collectionGroups: [], tags: [] };
	}

	const pgConnection = {
		name: 'pg1',
		protocol: 'postgres' as const,
		host: '10.0.0.5',
		port: 5432,
		unitId: 1,
		enabled: true,
		simulation: false,
		wordOrder: 'low_high' as const,
		database: 'appdb',
		username: 'appuser'
	};

	it('新規作成される postgres 接続（既存に同名が無い）は列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual(['pg1']);
	});

	it('既存 postgres 接続を更新するだけで、現在 passwordSet: true なら列挙されない', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'pg1',
				protocol: 'postgres',
				host: '10.0.0.5',
				port: 5432,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: 'appdb',
				username: 'appuser',
				passwordSet: true
			}
		]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual([]);
	});

	it('既存 postgres 接続を更新するだけでも、現在 passwordSet: false なら列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'pg1',
				protocol: 'postgres',
				host: '10.0.0.5',
				port: 5432,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: 'appdb',
				username: 'appuser',
				passwordSet: false
			}
		]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual(['pg1']);
	});

	it('postgres 以外の接続は create/update いずれでも列挙されない', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'plc1',
				protocol: 'modbus-tcp',
				host: '192.168.11.200',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: null,
				username: null,
				passwordSet: false
			}
		]);

		const inspection = await inspectConfigPackage(
			pkgWithConnections([
				{
					name: 'plc1',
					protocol: 'modbus-tcp',
					host: '192.168.11.200',
					port: 502,
					unitId: 1,
					enabled: true,
					simulation: false,
					wordOrder: 'low_high'
				},
				{
					name: 'plc2',
					protocol: 'modbus-tcp',
					host: '192.168.11.201',
					port: 502,
					unitId: 1,
					enabled: true,
					simulation: false,
					wordOrder: 'low_high'
				}
			])
		);

		expect(inspection.dbConnectionsPasswordRequired).toEqual([]);
	});
});

// --- S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6）: sink group
// の import/inspect - 接続名・タグ名からサーバー側 id への解決を含む点が
// 他のエンティティと違うため個別に固定する。

describe('applyConfigPackage: sink group は接続名・タグ名を id へ解決してから CRUD する', () => {
	const pkgWithSinkGroup: ConfigPackage = {
		...pkg,
		plcConnections: [
			{
				name: 'pg1',
				protocol: 'postgres',
				host: '10.0.0.5',
				port: 5432,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: 'appdb',
				username: 'appuser'
			}
		],
		collectionGroups: [],
		tags: [],
		sinkGroups: [
			{
				name: 'line1-log',
				dbConnectionName: 'pg1',
				mode: 'interval',
				intervalMs: 1000,
				tableName: 'public.tag_history',
				storeBad: false,
				enabled: true,
				tagNames: ['temp01']
			}
		]
	};

	function seedPgConnectionAndTag(): void {
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'pg1',
			protocol: 'postgres',
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: 'appdb',
			username: 'appuser',
			passwordSet: false
		});
		vi.mocked(listTags).mockResolvedValue([
			{
				id: 55,
				name: 'temp01',
				collectionGroupId: 1,
				address: 'D100',
				dataType: 'i16',
				stringLength: null,
				stringEncoding: 'utf8',
				rawLo: null,
				rawHi: null,
				engLo: null,
				engHi: null,
				unit: null,
				decimals: 0,
				thresholdH: null,
				thresholdHh: null,
				thresholdL: null,
				thresholdLl: null,
				enabled: true,
				writable: false,
				tagKind: 'plc',
				expression: null,
				retain: false,
				revision: 1
			}
		]);
	}

	it('createSinkGroup には接続 id・タグ id 配列を解決して渡す（新規作成）', async () => {
		seedPgConnectionAndTag();
		vi.mocked(listSinkGroups).mockResolvedValue([]);
		vi.mocked(createSinkGroup).mockResolvedValue({
			id: 9,
			name: 'line1-log',
			dbConnectionId: 1,
			mode: 'interval',
			intervalMs: 1000,
			tableName: 'public.tag_history',
			storeBad: false,
			enabled: true,
			tagIds: [55]
		});

		await applyConfigPackage(pkgWithSinkGroup);

		expect(createSinkGroup).toHaveBeenCalledWith({
			name: 'line1-log',
			dbConnectionId: 1,
			mode: 'interval',
			intervalMs: 1000,
			tableName: 'public.tag_history',
			storeBad: false,
			enabled: true,
			tagIds: [55]
		});
		expect(updateSinkGroup).not.toHaveBeenCalled();
	});

	it('同名の既存 sink group があれば updateSinkGroup を呼ぶ（作成ではなく更新）', async () => {
		seedPgConnectionAndTag();
		vi.mocked(listSinkGroups).mockResolvedValue([
			{
				id: 9,
				name: 'line1-log',
				dbConnectionId: 1,
				mode: 'interval',
				intervalMs: 5000,
				tableName: 'public.tag_history',
				storeBad: false,
				enabled: true,
				tagIds: [55]
			}
		]);
		vi.mocked(updateSinkGroup).mockResolvedValue({
			id: 9,
			name: 'line1-log',
			dbConnectionId: 1,
			mode: 'interval',
			intervalMs: 1000,
			tableName: 'public.tag_history',
			storeBad: false,
			enabled: true,
			tagIds: [55]
		});

		await applyConfigPackage(pkgWithSinkGroup);

		expect(updateSinkGroup).toHaveBeenCalledWith(9, {
			name: 'line1-log',
			dbConnectionId: 1,
			mode: 'interval',
			intervalMs: 1000,
			tableName: 'public.tag_history',
			storeBad: false,
			enabled: true,
			tagIds: [55]
		});
		expect(createSinkGroup).not.toHaveBeenCalled();
	});

	it('接続を解決できない sink group は warning を積んで CRUD を呼ばない', async () => {
		vi.mocked(listSinkGroups).mockResolvedValue([]);
		// `createPlcConnection`をモックしないため接続は解決されない
		// （pg1接続自体の作成は素通りするが `connectionByName` には乗らない）。
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'different-name',
			protocol: 'postgres',
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			database: 'appdb',
			username: 'appuser',
			passwordSet: false
		});

		const summary = await applyConfigPackage(pkgWithSinkGroup);

		expect(createSinkGroup).not.toHaveBeenCalled();
		expect(updateSinkGroup).not.toHaveBeenCalled();
		expect(summary.warnings.some((w) => w.includes('line1-log'))).toBe(true);
	});

	it('inspectConfigPackage は sinkGroups の create/update 件数を数える', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([]);
		vi.mocked(listCollectionGroups).mockResolvedValue([]);
		vi.mocked(listTags).mockResolvedValue([]);
		vi.mocked(listSinkGroups).mockResolvedValue([]);

		const inspection = await inspectConfigPackage(pkgWithSinkGroup);

		expect(inspection.counts.sinkGroups).toEqual({ create: 1, update: 0 });
	});

	it('inspectConfigPackage は未解決の接続・タグを warning に積む', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([]);
		vi.mocked(listCollectionGroups).mockResolvedValue([]);
		vi.mocked(listTags).mockResolvedValue([]);
		vi.mocked(listSinkGroups).mockResolvedValue([]);

		const pkgWithUnresolvedRefs: ConfigPackage = {
			...pkgWithSinkGroup,
			plcConnections: [],
			sinkGroups: [
				{
					...pkgWithSinkGroup.sinkGroups[0],
					dbConnectionName: 'no-such-connection',
					tagNames: ['no-such-tag']
				}
			]
		};

		const inspection = await inspectConfigPackage(pkgWithUnresolvedRefs);

		expect(inspection.warnings.some((w) => w.includes('no-such-connection'))).toBe(true);
		expect(inspection.warnings.some((w) => w.includes('no-such-tag'))).toBe(true);
	});
});
