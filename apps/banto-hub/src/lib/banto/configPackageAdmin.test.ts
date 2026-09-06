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
				defaultWritable: true
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
			defaultWritable: true
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
			defaultWritable: true
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
