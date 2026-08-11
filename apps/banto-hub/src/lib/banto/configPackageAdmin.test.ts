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
import { describe, expect, it, vi, beforeEach } from 'vitest';
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
	isConfigPackageImportAbortedError,
	ConfigPackageImportAbortedError
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
	collectionGroups: [{ name: 'group1', plcConnectionName: 'plc1', periodMs: 1000, enabled: true }],
	tags: [
		{
			name: 'tag1',
			collectionGroupName: 'group1',
			address: 'D3000',
			dataType: 'i16',
			stringLength: null,
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
				wordOrder: 'low_high'
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
			wordOrder: 'low_high'
		});
		vi.mocked(listCollectionGroups).mockResolvedValue([
			{ id: 1, name: 'group1', plcConnectionId: 1, periodMs: 500, enabled: true }
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
			wordOrder: 'low_high'
		});
		vi.mocked(createCollectionGroup).mockResolvedValue({
			id: 1,
			name: 'group1',
			plcConnectionId: 1,
			periodMs: 1000,
			enabled: true
		});
		vi.mocked(createTag).mockResolvedValue({
			id: 1,
			name: 'tag1',
			collectionGroupId: 1,
			address: 'D3000',
			dataType: 'i16',
			stringLength: null,
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
});
