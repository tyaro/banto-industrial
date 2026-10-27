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
import { buildConfigPackage, parseConfigPackage, serializeConfigPackage } from './configPackage';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import { FAKE_SECRET_DRIVER, TEST_DRIVERS } from './configPackageTestDrivers';

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
	// 版 3: 本物と同じ判定（`configPackageAdmin.ts` がパスワードの状態を見るのに使う）。
	isSecretStateWire: (value: unknown) =>
		typeof value === 'object' &&
		value !== null &&
		Object.keys(value).length === 1 &&
		Object.hasOwn(value, '$secret'),
	listDrivers: vi.fn(),
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
	listDrivers,
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
	configPackageExportFilename,
	connectionImportOrder
} from './configPackageAdmin';

const pkg: ConfigPackage = {
	schemaVersion: 3,
	product: 'banto-hub',
	exportedAt: '2026-08-12T00:00:00.000Z',
	excludedSecrets: [],
	plcConnections: [
		{
			name: 'plc1',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
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
			comment: '',
			plcConnectionName: 'plc1',
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

function plcConnection(id: number, name: string): PlcConnection {
	return {
		id,
		name,
		protocol: 'modbus-tcp',
		enabled: true,
		simulation: false,
		settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
	};
}

function collectionGroup(id: number, name: string, plcConnectionId: number): CollectionGroup {
	return {
		id,
		name,
		plcConnectionId,
		periodMs: 1000,
		enabled: true,
		defaultWritable: true,
		querySql: null
	};
}

function registryTag(id: number, name: string, collectionGroupId: number): Tag {
	return {
		id,
		name,
		comment: '',
		collectionGroupId,
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
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 3
	};
}

beforeEach(() => {
	vi.mocked(listDrivers).mockReset().mockResolvedValue(TEST_DRIVERS);
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
				enabled: true,
				simulation: false,
				settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
			}
		]);
		vi.mocked(updatePlcConnection).mockResolvedValue({
			id: 1,
			name: 'plc1',
			protocol: 'modbus-tcp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
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
			enabled: true,
			simulation: false,
			settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
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
			comment: '',
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
		expect(createTag).toHaveBeenCalledWith(expect.objectContaining({ comment: '' }));
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
			enabled: true,
			simulation: false,
			settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
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
			comment: '',
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
	it('createPlcConnection にはパッケージの settings（host・port・database・username）だけを渡し、password キー自体を渡さない', async () => {
		const pkgWithDbConnection: ConfigPackage = {
			...pkg,
			plcConnections: [
				{
					name: 'pg1',
					protocol: 'postgres',
					enabled: true,
					simulation: false,
					settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
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
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
		});

		await applyConfigPackage(pkgWithDbConnection);

		expect(createPlcConnection).toHaveBeenCalledTimes(1);
		const calledWith = vi.mocked(createPlcConnection).mock.calls[0][0];
		expect(calledWith).toEqual({
			name: 'pg1',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
		});
		expect(calledWith).not.toHaveProperty('password');
		expect(calledWith.settings).not.toHaveProperty('password');
	});

	it('既存の postgres 接続の更新も password を送らない（REST の「秘密のキーの省略は保つ」で保存済みを保つ）', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 4,
				name: 'pg1',
				protocol: 'postgres',
				enabled: true,
				simulation: false,
				settings: {
					host: 'old',
					port: 5432,
					database: 'd',
					username: 'u',
					password: { $secret: 'set' }
				}
			}
		]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) => ({ id, ...input }));
		await applyConfigPackage({
			...pkg,
			plcConnections: [
				{
					name: 'pg1',
					protocol: 'postgres',
					enabled: true,
					simulation: false,
					settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
				}
			],
			collectionGroups: [],
			tags: []
		});
		const [id, sent] = vi.mocked(updatePlcConnection).mock.calls[0];
		expect(id).toBe(4);
		expect(sent.settings).toEqual({
			host: '10.0.0.5',
			port: 5432,
			database: 'appdb',
			username: 'appuser'
		});
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
					enabled: true,
					simulation: false,
					settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
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
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
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
		protocol: 'postgres',
		enabled: true,
		simulation: false,
		settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
	};

	function existingPg(password?: unknown): PlcConnection {
		return {
			id: 1,
			name: 'pg1',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: {
				host: '10.0.0.5',
				port: 5432,
				database: 'appdb',
				username: 'appuser',
				...(password === undefined ? {} : { password })
			}
		};
	}

	it('新規作成される postgres 接続（既存に同名が無い）は列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual(['pg1']);
	});

	it('既存 postgres 接続を更新するだけで、今の password が $secret: set なら列挙されない', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'pg1',
				protocol: 'postgres',
				enabled: true,
				simulation: false,
				settings: {
					host: '10.0.0.5',
					port: 5432,
					database: 'appdb',
					username: 'appuser',
					password: { $secret: 'set' }
				}
			}
		]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual([]);
	});

	it('既存 postgres 接続を更新するだけでも、今の password が無ければ列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{
				id: 1,
				name: 'pg1',
				protocol: 'postgres',
				enabled: true,
				simulation: false,
				settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
			}
		]);

		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));

		expect(inspection.dbConnectionsPasswordRequired).toEqual(['pg1']);
	});

	it('今の password が unreadable（この PC で読めない）なら列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([existingPg({ $secret: 'unreadable' })]);
		const inspection = await inspectConfigPackage(pkgWithConnections([pgConnection]));
		expect(inspection.dbConnectionsPasswordRequired).toEqual(['pg1']);
	});

	it('同名の既存接続が postgres 以外なら（切り替え）列挙される', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			{ ...existingPg(), protocol: 'modbus-tcp', settings: { host: 'h', port: 502 } }
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
				enabled: true,
				simulation: false,
				settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
			}
		]);

		const inspection = await inspectConfigPackage(
			pkgWithConnections([
				{
					name: 'plc1',
					protocol: 'modbus-tcp',
					enabled: true,
					simulation: false,
					settings: { host: '192.168.11.200', port: 502, unit_id: 1, word_order: 'low_high' }
				},
				{
					name: 'plc2',
					protocol: 'modbus-tcp',
					enabled: true,
					simulation: false,
					settings: { host: '192.168.11.201', port: 502, unit_id: 1, word_order: 'low_high' }
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
				enabled: true,
				simulation: false,
				settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
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
				tags: [{ plcConnectionName: 'plc1', collectionGroupName: 'group1', tagName: 'temp01' }]
			}
		]
	};

	// 版 2: sink のタグ参照は (接続名, グループ名, タグ名) で引くので、既存の
	// タグの所属グループ・接続も現在の環境に置く。
	function seedPgConnectionAndTag(): void {
		vi.mocked(listPlcConnections).mockResolvedValue([plcConnection(2, 'plc1')]);
		vi.mocked(listCollectionGroups).mockResolvedValue([collectionGroup(1, 'group1', 2)]);
		vi.mocked(createPlcConnection).mockResolvedValue({
			id: 1,
			name: 'pg1',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
		});
		vi.mocked(listTags).mockResolvedValue([
			{
				id: 55,
				name: 'temp01',
				comment: '',
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
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
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
					tags: [
						{ plcConnectionName: 'plc1', collectionGroupName: 'group1', tagName: 'no-such-tag' }
					]
				}
			]
		};

		const inspection = await inspectConfigPackage(pkgWithUnresolvedRefs);

		expect(inspection.warnings.some((w) => w.includes('no-such-connection'))).toBe(true);
		expect(inspection.warnings.some((w) => w.includes('no-such-tag'))).toBe(true);
	});
});

// 版 3（R2b-6b-3、設計 §7.1）: 欄はドライバのスキーマ（`GET /api/drivers`）で確かめ、
// 欠けた欄を既定で埋めずに断る。確認の画面（inspect）と書き込み（apply）の両方で断る。
describe('applyConfigPackage / inspectConfigPackage: 版 3 の settings（欠けた欄・秘密の欄の断り）', () => {
	const slmpConn: PlcConnection = {
		id: 7,
		name: 'plcq',
		protocol: 'slmp',
		enabled: true,
		simulation: false,
		settings: { host: '10.0.0.8', port: 5007, cpu_type: 'Q', word_order: 'low_high' }
	};

	function slmpPackage(name: string, settings: Record<string, unknown>): ConfigPackage {
		return parseConfigPackage(
			JSON.stringify({
				...pkg,
				plcConnections: [{ name, protocol: 'slmp', enabled: true, simulation: false, settings }],
				collectionGroups: [],
				tags: []
			})
		);
	}

	it('cpu_type を持たない SLMP 接続は、確認の画面でも書き込みでも断り、何も送らない', async () => {
		const missing = slmpPackage('plcq', { host: '10.0.0.8', port: 5007, word_order: 'low_high' });
		await expect(inspectConfigPackage(missing)).rejects.toThrow(
			/plcConnections\[0\]\.settings\.cpu_type がありません/
		);
		await expect(applyConfigPackage(missing)).rejects.toThrow(/settings\.cpu_type がありません/);
		expect(createPlcConnection).not.toHaveBeenCalled();
		expect(updatePlcConnection).not.toHaveBeenCalled();
		expect(saveMqttSettings).not.toHaveBeenCalled();
	});

	it('パッケージの cpu_type（Q）を settings ごと更新に載せる（既存の Q 接続を R に戻さない）', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([slmpConn]);
		vi.mocked(updatePlcConnection).mockResolvedValue(slmpConn);
		await applyConfigPackage(slmpPackage('plcq', { ...slmpConn.settings }));
		const [id, sent] = vi.mocked(updatePlcConnection).mock.calls[0];
		expect(id).toBe(7);
		expect(sent).toEqual({
			name: 'plcq',
			protocol: 'slmp',
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.8', port: 5007, cpu_type: 'Q', word_order: 'low_high' }
		});
	});

	it('新しい接続の作成にもパッケージの settings をそのまま載せる', async () => {
		vi.mocked(createPlcConnection).mockImplementation(async (input) => ({ id: 8, ...input }));
		await applyConfigPackage(
			slmpPackage('plcnew', { host: '10.0.0.9', port: 5007, cpu_type: 'R', word_order: 'high_low' })
		);
		const sent = vi.mocked(createPlcConnection).mock.calls[0][0];
		expect(sent.settings).toEqual({
			host: '10.0.0.9',
			port: 5007,
			cpu_type: 'R',
			word_order: 'high_low'
		});
	});

	it('Secret の欄と既定の無い任意の欄が欠けた接続は通り、Secret の欄を送らない（保存済みを保つ）', async () => {
		vi.mocked(createPlcConnection).mockImplementation(async (input) => ({ id: 9, ...input }));
		const fake = parseConfigPackage(
			JSON.stringify({
				...pkg,
				plcConnections: [
					{
						name: 'fake',
						protocol: FAKE_SECRET_DRIVER.id,
						enabled: true,
						simulation: false,
						settings: { host: 'h' }
					}
				],
				collectionGroups: [],
				tags: []
			})
		);
		await expect(inspectConfigPackage(fake)).resolves.toBeDefined();
		await applyConfigPackage(fake);
		expect(vi.mocked(createPlcConnection).mock.calls[0][0].settings).toEqual({ host: 'h' });
	});

	it('取り込み先の Hub に無いドライバの接続は断る', async () => {
		vi.mocked(listDrivers).mockResolvedValue([]);
		const modbusOnly = { ...pkg, collectionGroups: [], tags: [] };
		await expect(inspectConfigPackage(modbusOnly)).rejects.toThrow(
			/protocol 'modbus-tcp' はこの banto-hub にありません/
		);
	});
});

// --- 版 2（2026-10-19 オーナー決定: グループ名は接続内で一意）: グループは
// (接続名, グループ名)、タグは (接続名, グループ名, タグ名) で既存と突き合わせる。
// 名前だけで突き合わせると、別の接続の同名グループを更新して接続を付け替えたり、
// 別のグループの同名タグを動かしたりしてしまう。

describe('applyConfigPackage / inspectConfigPackage: 版 2 のキーで突き合わせる', () => {
	const tagTemplate = pkg.tags[0];

	it('legacy JSON import preserves an existing comment and an empty comment clears it', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([plcConnection(1, 'plc1')]);
		vi.mocked(listCollectionGroups).mockResolvedValue([collectionGroup(1, 'group1', 1)]);
		vi.mocked(listTags).mockResolvedValue([{ ...registryTag(7, 'tag1', 1), comment: '既存メモ' }]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) =>
			plcConnection(id, input.name)
		);
		vi.mocked(updateCollectionGroup).mockImplementation(async (id, input) =>
			collectionGroup(id, input.name, input.plcConnectionId)
		);
		vi.mocked(updateTag).mockImplementation(async (id, input) =>
			registryTag(id, input.name, input.collectionGroupId)
		);

		const legacy = parseConfigPackage(
			JSON.stringify({ ...pkg, tags: [{ ...tagTemplate, comment: undefined }] })
		);
		await applyConfigPackage(legacy);
		expect(vi.mocked(updateTag).mock.calls[0][1]).not.toHaveProperty('comment');

		vi.mocked(updateTag).mockClear();
		await applyConfigPackage({ ...pkg, tags: [{ ...tagTemplate, comment: '' }] });
		expect(vi.mocked(updateTag).mock.calls[0][1].comment).toBe('');

		vi.mocked(listTags).mockResolvedValue([]);
		vi.mocked(createTag).mockImplementation(async (input) =>
			registryTag(8, input.name, input.collectionGroupId)
		);
		await applyConfigPackage(legacy);
		expect(vi.mocked(createTag).mock.calls[0][0]).not.toHaveProperty('comment');
	});

	it('2 つの接続の同名グループ・同名タグを別々に扱う（既存は更新、もう一方は作成）', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			plcConnection(1, 'plc1'),
			plcConnection(2, 'plc2')
		]);
		vi.mocked(listCollectionGroups).mockResolvedValue([collectionGroup(10, 'group1', 1)]);
		vi.mocked(listTags).mockResolvedValue([registryTag(100, 'tag1', 10)]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) =>
			plcConnection(id, input.name)
		);
		vi.mocked(updateCollectionGroup).mockImplementation(async (id, input) =>
			collectionGroup(id, input.name, input.plcConnectionId)
		);
		vi.mocked(createCollectionGroup).mockImplementation(async (input) =>
			collectionGroup(20, input.name, input.plcConnectionId)
		);
		vi.mocked(updateTag).mockImplementation(async (id, input) =>
			registryTag(id, input.name, input.collectionGroupId)
		);
		vi.mocked(createTag).mockImplementation(async (input) =>
			registryTag(200, input.name, input.collectionGroupId)
		);

		const twoLines: ConfigPackage = {
			...pkg,
			plcConnections: [
				{ ...pkg.plcConnections[0], name: 'plc1' },
				{ ...pkg.plcConnections[0], name: 'plc2' }
			],
			collectionGroups: [
				{ ...pkg.collectionGroups[0], plcConnectionName: 'plc1' },
				{ ...pkg.collectionGroups[0], plcConnectionName: 'plc2' }
			],
			tags: [
				{ ...tagTemplate, plcConnectionName: 'plc1' },
				{ ...tagTemplate, plcConnectionName: 'plc2' }
			]
		};
		// パッケージとして妥当（同じ名前でも接続が違えば重複ではない）。
		expect(() => parseConfigPackage(JSON.stringify(twoLines))).not.toThrow();

		const inspection = await inspectConfigPackage(twoLines);
		expect(inspection.counts.collectionGroups).toEqual({ create: 1, update: 1 });
		expect(inspection.counts.tags).toEqual({ create: 1, update: 1 });
		expect(inspection.warnings).toEqual([]);

		const summary = await applyConfigPackage(twoLines);

		// plc1 の group1 は既存（id 10）を同じ接続のまま更新、plc2 の group1 は新規作成。
		expect(updateCollectionGroup).toHaveBeenCalledTimes(1);
		expect(updateCollectionGroup).toHaveBeenCalledWith(
			10,
			expect.objectContaining({ name: 'group1', plcConnectionId: 1 })
		);
		expect(createCollectionGroup).toHaveBeenCalledTimes(1);
		expect(createCollectionGroup).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'group1', plcConnectionId: 2 })
		);
		// tag1 も同じ: plc1 側は既存（id 100）を更新、plc2 側は新しいグループ（id 20）に作成。
		expect(updateTag).toHaveBeenCalledTimes(1);
		expect(updateTag).toHaveBeenCalledWith(
			100,
			expect.objectContaining({ name: 'tag1', collectionGroupId: 10, expectedRevision: 3 })
		);
		expect(createTag).toHaveBeenCalledTimes(1);
		expect(createTag).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'tag1', collectionGroupId: 20 })
		);
		expect(summary.counts.collectionGroups).toEqual({ create: 1, update: 1 });
		expect(summary.counts.tags).toEqual({ create: 1, update: 1 });
		expect(summary.warnings).toEqual([]);
	});

	it('1 つの接続の 2 つのグループに同名タグがあれば、グループごとに突き合わせる', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([plcConnection(1, 'plc1')]);
		vi.mocked(listCollectionGroups).mockResolvedValue([
			collectionGroup(10, 'group1', 1),
			collectionGroup(11, 'group2', 1)
		]);
		vi.mocked(listTags).mockResolvedValue([registryTag(100, 'tag1', 10)]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) =>
			plcConnection(id, input.name)
		);
		vi.mocked(updateCollectionGroup).mockImplementation(async (id, input) =>
			collectionGroup(id, input.name, input.plcConnectionId)
		);
		vi.mocked(updateTag).mockImplementation(async (id, input) =>
			registryTag(id, input.name, input.collectionGroupId)
		);
		vi.mocked(createTag).mockImplementation(async (input) =>
			registryTag(101, input.name, input.collectionGroupId)
		);

		const twoGroups: ConfigPackage = {
			...pkg,
			collectionGroups: [pkg.collectionGroups[0], { ...pkg.collectionGroups[0], name: 'group2' }],
			tags: [tagTemplate, { ...tagTemplate, collectionGroupName: 'group2' }]
		};

		const summary = await applyConfigPackage(twoGroups);

		expect(updateTag).toHaveBeenCalledTimes(1);
		expect(updateTag).toHaveBeenCalledWith(
			100,
			expect.objectContaining({ name: 'tag1', collectionGroupId: 10 })
		);
		expect(createTag).toHaveBeenCalledTimes(1);
		expect(createTag).toHaveBeenCalledWith(
			expect.objectContaining({ name: 'tag1', collectionGroupId: 11 })
		);
		expect(summary.counts.tags).toEqual({ create: 1, update: 1 });
	});

	it('sink group のタグ参照は、同名タグのうち接続・グループが一致するものへ解決する', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			plcConnection(1, 'plc1'),
			plcConnection(2, 'plc2'),
			{
				...plcConnection(3, 'pg1'),
				protocol: 'postgres',
				settings: { host: 'h', port: 5432, password: { $secret: 'set' } }
			}
		]);
		vi.mocked(listCollectionGroups).mockResolvedValue([
			collectionGroup(10, 'group1', 1),
			collectionGroup(20, 'group1', 2)
		]);
		vi.mocked(listTags).mockResolvedValue([
			registryTag(100, 'temp01', 10),
			registryTag(200, 'temp01', 20)
		]);
		vi.mocked(createSinkGroup).mockImplementation(async (input) => ({ id: 9, ...input }));

		const sinkOnly: ConfigPackage = {
			...pkg,
			plcConnections: [],
			collectionGroups: [],
			tags: [],
			sinkGroups: [
				{
					name: 'line2-log',
					dbConnectionName: 'pg1',
					mode: 'interval',
					intervalMs: 1000,
					tableName: 'public.tag_history',
					storeBad: false,
					enabled: true,
					tags: [{ plcConnectionName: 'plc2', collectionGroupName: 'group1', tagName: 'temp01' }]
				}
			]
		};

		const inspection = await inspectConfigPackage(sinkOnly);
		expect(inspection.warnings).toEqual([]);

		await applyConfigPackage(sinkOnly);
		expect(createSinkGroup).toHaveBeenCalledWith(
			expect.objectContaining({ dbConnectionId: 3, tagIds: [200] })
		);
	});

	it('参照先のタグが現在の環境に無ければ、接続・グループ・タグの組で warning を出す', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([plcConnection(1, 'plc1')]);
		vi.mocked(listCollectionGroups).mockResolvedValue([collectionGroup(10, 'group1', 1)]);
		vi.mocked(listTags).mockResolvedValue([registryTag(100, 'temp01', 10)]);

		const inspection = await inspectConfigPackage({
			...pkg,
			plcConnections: [],
			collectionGroups: [],
			tags: [],
			sinkGroups: [
				{
					name: 'log',
					dbConnectionName: 'plc1',
					mode: 'interval',
					intervalMs: 1000,
					tableName: 'public.tag_history',
					storeBad: false,
					enabled: true,
					// 同名のタグはあるが、別の接続（plc2）を指している。
					tags: [{ plcConnectionName: 'plc2', collectionGroupName: 'group1', tagName: 'temp01' }]
				}
			]
		});
		expect(inspection.warnings).toEqual([
			"sink group 'log' のタグ 'plc2 / group1 / temp01' は現在の環境で見つかりません"
		]);
	});
});

// R2a-2e（docs/banto-hub-ingest-design.md §3.1・§5.1・§10.2）: push 接続の
// ブリッジは読む postgres 接続を id で持つので、取り込みは postgres 接続を先に
// 作成・更新し、パッケージの名前（`dbConnectionName`）を取り込み先の id に付け
// 直す。API キーは移らないので、結果に push 接続の新しい id を返す。
describe('applyConfigPackage: R2a-2e push 接続のブリッジ（dbConnectionName の付け直し）', () => {
	const PG_SETTINGS = { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' };

	function pushPackage(): ConfigPackage {
		return {
			...pkg,
			plcConnections: [
				// わざと push を postgres より前に並べる。
				{
					name: 'Bridge1',
					protocol: 'push',
					enabled: true,
					simulation: false,
					settings: {
						stale_timeout_ms: 5000,
						bridge: {
							kind: 'db',
							dbConnectionName: 'pg-a',
							queries: [
								{
									name: 'line1',
									period_ms: 1000,
									sql: 'SELECT temp FROM line1_now',
									columns: [{ column: 'temp', key: 'line1.temp' }]
								}
							]
						}
					}
				},
				{
					name: 'pg-a',
					protocol: 'postgres',
					enabled: true,
					simulation: false,
					settings: { ...PG_SETTINGS }
				}
			],
			collectionGroups: [],
			tags: []
		};
	}

	function savedConnection(
		input: { name: string; protocol: string; settings?: Record<string, unknown> },
		id: number
	): PlcConnection {
		return {
			id,
			name: input.name,
			protocol: input.protocol,
			enabled: true,
			simulation: false,
			settings: input.settings ?? {}
		};
	}

	it('postgres 接続を先に作り、push のブリッジをその新しい id で作る。結果に push 接続の id を返す', async () => {
		let nextId = 40;
		vi.mocked(createPlcConnection).mockImplementation(async (input) =>
			savedConnection(input, nextId++)
		);
		const summary = await applyConfigPackage(pushPackage());

		const calls = vi.mocked(createPlcConnection).mock.calls.map(([input]) => input);
		expect(calls.map((c) => c.name)).toEqual(['pg-a', 'Bridge1']);
		expect(calls[0].settings).toEqual(PG_SETTINGS);
		expect(calls[1].settings).toEqual({
			stale_timeout_ms: 5000,
			bridge: {
				kind: 'db',
				db_connection_id: 40,
				queries: [
					{
						name: 'line1',
						period_ms: 1000,
						sql: 'SELECT temp FROM line1_now',
						columns: [{ column: 'temp', key: 'line1.temp' }]
					}
				]
			}
		});
		expect(JSON.stringify(calls[1])).not.toContain('dbConnectionName');
		expect(summary.ingestConnections).toEqual([{ name: 'Bridge1', id: 41 }]);
		expect(summary.warnings).toEqual([]);
	});

	it('既存の postgres・push 接続を更新するときは、取り込み先の既存の id に付け直す', async () => {
		vi.mocked(listPlcConnections).mockResolvedValue([
			savedConnection({ name: 'pg-a', protocol: 'postgres' }, 7),
			savedConnection({ name: 'Bridge1', protocol: 'push' }, 9)
		]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) =>
			savedConnection(input, id)
		);
		const summary = await applyConfigPackage(pushPackage());
		const calls = vi.mocked(updatePlcConnection).mock.calls;
		expect(calls.map(([id]) => id)).toEqual([7, 9]);
		expect(
			(calls[1][1].settings.bridge as { db_connection_id: number } | undefined)?.db_connection_id
		).toBe(7);
		expect(summary.ingestConnections).toEqual([{ name: 'Bridge1', id: 9 }]);
	});

	it('ブリッジの読む接続を引けなければ、壊れた参照で作らず警告を積んで飛ばす', async () => {
		vi.mocked(createPlcConnection).mockImplementation(async (input) => savedConnection(input, 50));
		const broken = pushPackage();
		broken.plcConnections = broken.plcConnections.filter((c) => c.name !== 'pg-a');
		const summary = await applyConfigPackage(broken);
		expect(vi.mocked(createPlcConnection)).not.toHaveBeenCalled();
		expect(summary.warnings).toEqual([
			"push 接続 'Bridge1' のブリッジが読む接続 'pg-a' を解決できませんでした"
		]);
		expect(summary.ingestConnections).toEqual([]);
	});

	/** 書き出し元の Hub: pg-a は id 3、Bridge1（id 12）のブリッジは id 3 を読む。 */
	function exportedFromSourceHub(): string {
		return serializeConfigPackage(
			buildConfigPackage({
				plcConnections: [
					savedConnection(
						{
							name: 'Bridge1',
							protocol: 'push',
							settings: {
								bridge: {
									kind: 'db',
									db_connection_id: 3,
									queries: [
										{
											name: 'q',
											period_ms: 500,
											sql: 'SELECT 1 AS v',
											columns: [{ column: 'v', key: 'k' }]
										}
									]
								}
							}
						},
						12
					),
					savedConnection(
						{
							name: 'pg-a',
							protocol: 'postgres',
							settings: {
								host: 'db',
								port: 5432,
								database: 'd',
								username: 'u',
								password: { $secret: 'set' }
							}
						},
						3
					)
				],
				collectionGroups: [],
				tags: [],
				mqtt: { ...currentMqtt },
				grpc: { enabled: false, bind: '127.0.0.1', port: 50051 }
			})
		);
	}

	it('往復: 書き出した Hub の id（12 → 3）が、取り込み先の新しい id（81 → 80）に付け直される', async () => {
		const exported = exportedFromSourceHub();
		expect(exported).not.toContain('db_connection_id');
		expect(JSON.stringify(JSON.parse(exported).plcConnections)).not.toContain('$secret');
		let nextId = 80;
		vi.mocked(createPlcConnection).mockImplementation(async (input) =>
			savedConnection(input, nextId++)
		);
		const summary = await applyConfigPackage(parseConfigPackage(exported));
		const calls = vi.mocked(createPlcConnection).mock.calls.map(([input]) => input);
		expect(calls.find((input) => input.name === 'pg-a')?.settings).toEqual({
			host: 'db',
			port: 5432,
			database: 'd',
			username: 'u'
		});
		expect(calls.find((input) => input.name === 'Bridge1')?.settings).toEqual({
			bridge: {
				kind: 'db',
				db_connection_id: 80,
				queries: [
					{ name: 'q', period_ms: 500, sql: 'SELECT 1 AS v', columns: [{ column: 'v', key: 'k' }] }
				]
			}
		});
		expect(summary.ingestConnections).toEqual([{ name: 'Bridge1', id: 81 }]);
	});

	it('往復: 取り込み先で書き出し元の id（3）が関係のない postgres 接続のものでも、ブリッジは名前で引いた pg-a を読む', async () => {
		// 取り込み先の Hub: id 3 は別の PostgreSQL（pg-other）。pg-a は id 21 で既にある。
		vi.mocked(listPlcConnections).mockResolvedValue([
			savedConnection(
				{
					name: 'pg-other',
					protocol: 'postgres',
					settings: { host: 'other', port: 5432, database: 'x', username: 'y' }
				},
				3
			),
			savedConnection({ name: 'pg-a', protocol: 'postgres' }, 21)
		]);
		vi.mocked(updatePlcConnection).mockImplementation(async (id, input) =>
			savedConnection(input, id)
		);
		vi.mocked(createPlcConnection).mockImplementation(async (input) => savedConnection(input, 30));
		const summary = await applyConfigPackage(parseConfigPackage(exportedFromSourceHub()));
		expect(vi.mocked(updatePlcConnection).mock.calls.map(([id]) => id)).toEqual([21]);
		const bridgeCall = vi.mocked(createPlcConnection).mock.calls[0][0];
		expect(bridgeCall.name).toBe('Bridge1');
		expect((bridgeCall.settings.bridge as { db_connection_id: number }).db_connection_id).toBe(21);
		expect(summary.ingestConnections).toEqual([{ name: 'Bridge1', id: 30 }]);
	});

	it('connectionImportOrder はブリッジを持つ push 接続だけを後ろへ回す（他は元の順）', () => {
		const order = connectionImportOrder([
			...pushPackage().plcConnections,
			{ ...pkg.plcConnections[0], name: 'plc-z' }
		]);
		expect(order.map((c) => c.name)).toEqual(['pg-a', 'plc-z', 'Bridge1']);
	});

	it('inspectConfigPackage は push 接続の名前を ingestConnections に出す（キーは移らない案内用）', async () => {
		const inspection = await inspectConfigPackage(pushPackage());
		expect(inspection.ingestConnections).toEqual(['Bridge1']);
		expect((await inspectConfigPackage(pkg)).ingestConnections).toEqual([]);
	});
});

describe('秘密の欄が null のパッケージは書き込みの前に断る（保存済みの秘密を消さない）', () => {
	const nullSecretConnections: ConfigPackage['plcConnections'][] = [
		[
			{
				name: 'pg1',
				protocol: 'postgres',
				enabled: true,
				simulation: false,
				settings: {
					host: '10.0.0.5',
					port: 5432,
					database: 'appdb',
					username: 'appuser',
					password: null
				}
			}
		],
		[
			{
				name: 'fake1',
				protocol: FAKE_SECRET_DRIVER.id,
				enabled: true,
				simulation: false,
				settings: { host: 'h', token: null }
			}
		]
	];

	for (const connections of nullSecretConnections) {
		it(`${connections[0].protocol}: inspect と apply が断り、作成・更新を 1 回も呼ばない`, async () => {
			vi.mocked(listPlcConnections).mockResolvedValue([]);
			const target: ConfigPackage = {
				...pkg,
				plcConnections: connections,
				collectionGroups: [],
				tags: []
			};

			await expect(inspectConfigPackage(target)).rejects.toThrow(/は秘密の欄です/);
			await expect(applyConfigPackage(target)).rejects.toThrow(/は秘密の欄です/);

			expect(createPlcConnection).not.toHaveBeenCalled();
			expect(updatePlcConnection).not.toHaveBeenCalled();
		});
	}
});
