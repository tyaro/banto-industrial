import { describe, expect, it } from 'vitest';
import {
	CONFIG_PACKAGE_EXCLUDED_SECRETS,
	CONFIG_PACKAGE_PRODUCT,
	CONFIG_PACKAGE_SCHEMA_VERSION,
	buildConfigPackage,
	parseConfigPackage,
	planByName,
	serializeConfigPackage,
	type ConfigPackage
} from './configPackage';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import type { GrpcSettings } from './grpcSettingsAdmin';
import type { MqttSettings } from './mqttSettingsAdmin';

const BASE_CONNECTION: PlcConnection = {
	id: 1,
	name: 'line-a',
	protocol: 'modbus-tcp',
	host: '127.0.0.1',
	port: 502,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high'
};

const VIRTUAL_CONNECTION: PlcConnection = {
	id: 2,
	name: 'calc',
	protocol: 'virtual',
	host: '',
	port: 0,
	unitId: 0,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high'
};

const BASE_GROUP: CollectionGroup = {
	id: 10,
	name: 'group-a',
	plcConnectionId: BASE_CONNECTION.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true
};

const VIRTUAL_GROUP: CollectionGroup = {
	id: 11,
	name: 'group-calc',
	plcConnectionId: VIRTUAL_CONNECTION.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true
};

const BASE_TAG: Tag = {
	id: 100,
	name: 'tag-a',
	collectionGroupId: BASE_GROUP.id,
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
	revision: 7
};

const VIRTUAL_TAG: Tag = {
	...BASE_TAG,
	id: 101,
	name: 'tag-calc',
	collectionGroupId: VIRTUAL_GROUP.id,
	tagKind: 'computed',
	address: '',
	expression: 'tag-a * 2'
};

const MQTT: MqttSettings = {
	enabled: true,
	host: 'mqtt.example.local',
	port: 1883,
	clientId: 'banto-hub',
	username: 'should-not-export',
	prefix: 'banto',
	qos: 1,
	minIntervalMs: 1000
};

const GRPC: GrpcSettings = {
	enabled: true,
	bind: '127.0.0.1',
	port: 50051
};

function makePackage(): ConfigPackage {
	return buildConfigPackage({
		plcConnections: [BASE_CONNECTION, VIRTUAL_CONNECTION],
		collectionGroups: [BASE_GROUP, VIRTUAL_GROUP],
		tags: [BASE_TAG, VIRTUAL_TAG],
		mqtt: MQTT,
		grpc: GRPC,
		exportedAt: '2026-08-11T00:00:00.000Z'
	});
}

describe('configPackage', () => {
	it('buildConfigPackage は秘密情報を除外し、virtual 接続は出力しない', () => {
		const pkg = makePackage();
		expect(pkg.schemaVersion).toBe(CONFIG_PACKAGE_SCHEMA_VERSION);
		expect(pkg.product).toBe(CONFIG_PACKAGE_PRODUCT);
		expect(pkg.excludedSecrets).toEqual(CONFIG_PACKAGE_EXCLUDED_SECRETS);
		expect(pkg.plcConnections).toEqual([
			{
				name: 'line-a',
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high'
			}
		]);
		expect(pkg.collectionGroups).toEqual([
			{
				name: 'group-a',
				plcConnectionName: 'line-a',
				periodMs: 1000,
				enabled: true,
				defaultWritable: true
			},
			{
				name: 'group-calc',
				plcConnectionName: 'calc',
				periodMs: 1000,
				enabled: true,
				defaultWritable: true
			}
		]);
		expect(pkg.tags).toEqual([
			{
				name: 'tag-a',
				collectionGroupName: 'group-a',
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
				retain: false
			},
			{
				name: 'tag-calc',
				collectionGroupName: 'group-calc',
				address: '',
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
				tagKind: 'computed',
				expression: 'tag-a * 2',
				retain: false
			}
		]);
		expect(pkg.mqtt).toEqual({
			enabled: true,
			host: 'mqtt.example.local',
			port: 1883,
			clientId: 'banto-hub',
			prefix: 'banto',
			qos: 1,
			minIntervalMs: 1000
		});
		expect(Object.prototype.hasOwnProperty.call(pkg.mqtt, 'username')).toBe(false);
		expect(pkg.grpc).toEqual(GRPC);
	});

	it('serializeConfigPackage と parseConfigPackage は往復できる', () => {
		const original = makePackage();
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
	});

	it('parseConfigPackage は reserved virtual 名を参照する package も受け入れる', () => {
		const text = JSON.stringify(
			{
				...makePackage(),
				plcConnections: [],
				collectionGroups: [
					{
						name: 'group-calc',
						plcConnectionName: 'calc',
						periodMs: 1000,
						enabled: true
					}
				],
				tags: []
			},
			null,
			2
		);
		expect(() => parseConfigPackage(text)).not.toThrow();
	});

	it('parseConfigPackage は不正な schemaVersion を拒否する', () => {
		const text = JSON.stringify({ ...makePackage(), schemaVersion: 2 }, null, 2);
		expect(() => parseConfigPackage(text)).toThrow(/schemaVersion/);
	});

	// --- P3-b（監査指摘 2026-08-12）: wordOrder は旧スキーマの構成パッケージ
	// （この列を持たない）を読めなければならない - CONFIG_PACKAGE_SCHEMA_VERSION
	// は据え置きのまま追加した後方互換フィールドなので、既存のエクスポート済み
	// ファイルは wordOrder を一切含まない。

	it('parseConfigPackage は wordOrder を持たない旧パッケージを low_high 既定で受け入れる', () => {
		const pkg = makePackage();
		const withoutWordOrder = {
			...pkg,
			plcConnections: pkg.plcConnections.map(({ wordOrder: _wordOrder, ...rest }) => rest)
		};
		const parsed = parseConfigPackage(JSON.stringify(withoutWordOrder));
		expect(parsed.plcConnections[0].wordOrder).toBe('low_high');
	});

	it('parseConfigPackage は不正な wordOrder を拒否する', () => {
		const pkg = makePackage();
		const withBadWordOrder = {
			...pkg,
			plcConnections: pkg.plcConnections.map((c) => ({ ...c, wordOrder: 'middle_endian' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadWordOrder))).toThrow(/wordOrder/);
	});

	// --- T19 S1-b（UX-34、2026-09-02 オーナー決定）: defaultWritable は
	// wordOrder と同じ理由で旧スキーマの構成パッケージ（この項目を持たない）
	// を読めなければならない - CONFIG_PACKAGE_SCHEMA_VERSION は据え置きの
	// まま追加した後方互換フィールド。

	it('parseConfigPackage は defaultWritable を持たない旧パッケージを true 既定で受け入れる', () => {
		const pkg = makePackage();
		const withoutDefaultWritable = {
			...pkg,
			collectionGroups: pkg.collectionGroups.map(
				({ defaultWritable: _defaultWritable, ...rest }) => rest
			)
		};
		const parsed = parseConfigPackage(JSON.stringify(withoutDefaultWritable));
		expect(parsed.collectionGroups[0].defaultWritable).toBe(true);
	});

	it('parseConfigPackage は不正な defaultWritable を拒否する', () => {
		const pkg = makePackage();
		const withBadDefaultWritable = {
			...pkg,
			collectionGroups: pkg.collectionGroups.map((g) => ({ ...g, defaultWritable: 'yes' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadDefaultWritable))).toThrow(
			/defaultWritable/
		);
	});

	// --- stringEncoding は既存のタグ登録経路には存在するが config package の
	// バックアップ/復元だけ未対応だった潜在バグの修正（2026-09-05）:
	// shift_jis のタグをバックアップ→復元すると暗黙的に utf8 へ戻ってしまう
	// データ損失を防ぐ。stringLength と同じ後方互換方針（省略時は utf8 既定）
	// を適用する。

	it('shift_jis の stringEncoding を持つタグが往復できる', () => {
		const pkg = buildConfigPackage({
			plcConnections: [BASE_CONNECTION],
			collectionGroups: [BASE_GROUP],
			tags: [{ ...BASE_TAG, stringEncoding: 'shift_jis' }],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-08-11T00:00:00.000Z'
		});
		expect(pkg.tags[0].stringEncoding).toBe('shift_jis');
		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed.tags[0].stringEncoding).toBe('shift_jis');
	});

	it('parseConfigPackage は stringEncoding を持たない旧パッケージを utf8 既定で受け入れる', () => {
		const pkg = makePackage();
		const withoutStringEncoding = {
			...pkg,
			tags: pkg.tags.map(({ stringEncoding: _stringEncoding, ...rest }) => rest)
		};
		const parsed = parseConfigPackage(JSON.stringify(withoutStringEncoding));
		expect(parsed.tags[0].stringEncoding).toBe('utf8');
	});

	it('parseConfigPackage は不正な stringEncoding を拒否する', () => {
		const pkg = makePackage();
		const withBadStringEncoding = {
			...pkg,
			tags: pkg.tags.map((t) => ({ ...t, stringEncoding: 'euc' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadStringEncoding))).toThrow(
			/stringEncoding/
		);
	});

	it('planByName は name ベースで create/update を分ける', () => {
		const plan = planByName(
			[{ name: 'alpha' }, { name: 'beta' }, { name: 'gamma' }],
			[{ name: 'beta' }, { name: 'delta' }]
		);
		expect(plan.create).toEqual([{ name: 'alpha' }, { name: 'gamma' }]);
		expect(plan.update).toEqual([{ incoming: { name: 'beta' }, existing: { name: 'beta' } }]);
		expect(plan.missing).toEqual(plan.create);
	});
});
