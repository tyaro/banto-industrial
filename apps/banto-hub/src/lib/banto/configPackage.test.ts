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
	simulation: false
};

const VIRTUAL_CONNECTION: PlcConnection = {
	id: 2,
	name: 'calc',
	protocol: 'virtual',
	host: '',
	port: 0,
	unitId: 0,
	enabled: true,
	simulation: false
};

const BASE_GROUP: CollectionGroup = {
	id: 10,
	name: 'group-a',
	plcConnectionId: BASE_CONNECTION.id,
	periodMs: 1000,
	enabled: true
};

const VIRTUAL_GROUP: CollectionGroup = {
	id: 11,
	name: 'group-calc',
	plcConnectionId: VIRTUAL_CONNECTION.id,
	periodMs: 1000,
	enabled: true
};

const BASE_TAG: Tag = {
	id: 100,
	name: 'tag-a',
	collectionGroupId: BASE_GROUP.id,
	address: 'D100',
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
				simulation: false
			}
		]);
		expect(pkg.collectionGroups).toEqual([
			{
				name: 'group-a',
				plcConnectionName: 'line-a',
				periodMs: 1000,
				enabled: true
			},
			{
				name: 'group-calc',
				plcConnectionName: 'calc',
				periodMs: 1000,
				enabled: true
			}
		]);
		expect(pkg.tags).toEqual([
			{
				name: 'tag-a',
				collectionGroupName: 'group-a',
				address: 'D100',
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
			},
			{
				name: 'tag-calc',
				collectionGroupName: 'group-calc',
				address: '',
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
