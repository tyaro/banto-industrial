import { describe, expect, it } from 'vitest';
import {
	CONFIG_PACKAGE_EXCLUDED_SECRETS,
	CONFIG_PACKAGE_PRODUCT,
	CONFIG_PACKAGE_SCHEMA_VERSION,
	INGEST_KEYS_NOT_MIGRATED_MESSAGE,
	buildConfigPackage,
	checkConfigPackageConnectionFields,
	parseConfigPackage,
	planByKey,
	planByName,
	resolvePushSettings,
	serializeConfigPackage,
	type ConfigPackage,
	type ConfigPackagePushSettings
} from './configPackage';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import { TEST_DRIVERS } from './configPackageTestDrivers';
import type { GrpcSettings } from './grpcSettingsAdmin';
import type { MqttSettings } from './mqttSettingsAdmin';
import type { SinkGroup } from './sinkGroupsAdmin';

// 版 3（R2b-6b-3）: 接続は REST の新しい形（`PlcConnection`）。`settings` の中は
// ドライバの JSON のまま（snake_case）。
const BASE_CONNECTION: PlcConnection = {
	id: 1,
	name: 'line-a',
	protocol: 'modbus-tcp',
	enabled: true,
	simulation: false,
	settings: { host: '127.0.0.1', port: 502, unit_id: 1, word_order: 'low_high' }
};

const VIRTUAL_CONNECTION: PlcConnection = {
	id: 2,
	name: 'calc',
	protocol: 'virtual',
	enabled: true,
	simulation: false,
	settings: {}
};

// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: postgres（DB
// Source）接続のフィクスチャ - 保存済みパスワードあり（`{"$secret": "set"}`）
// でも export には一切パスワードが乗らないことを確認する用。
const DB_CONNECTION: PlcConnection = {
	id: 3,
	name: 'pg-a',
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
};

// SLMP（Q シリーズ）の接続。
const SLMP_CONNECTION: PlcConnection = {
	id: 5,
	name: 'line-q',
	protocol: 'slmp',
	enabled: true,
	simulation: true,
	settings: { host: '192.168.11.10', port: 3100, cpu_type: 'Q', word_order: 'high_low' }
};

const BASE_GROUP: CollectionGroup = {
	id: 10,
	name: 'group-a',
	plcConnectionId: BASE_CONNECTION.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true,
	querySql: null
};

const VIRTUAL_GROUP: CollectionGroup = {
	id: 11,
	name: 'group-calc',
	plcConnectionId: VIRTUAL_CONNECTION.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: true,
	querySql: null
};

// S3（docs/banto-hub-external-db-design.md §7 row S3）: postgres（DB
// Source）接続配下の収集グループのフィクスチャ - `querySql`が export に
// 含まれ、往復できることを確認する用。
const DB_GROUP: CollectionGroup = {
	id: 12,
	name: 'group-pg',
	plcConnectionId: DB_CONNECTION.id,
	periodMs: 1000,
	enabled: true,
	defaultWritable: false,
	querySql: 'SELECT id, temperature, running FROM sensors'
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

// S3: `tagKind: 'db'`のタグのフィクスチャ - `address`は結果列名。
const DB_TAG: Tag = {
	...BASE_TAG,
	id: 102,
	name: 'tag-db',
	collectionGroupId: DB_GROUP.id,
	tagKind: 'db',
	address: 'temperature',
	dataType: 'f32',
	writable: false,
	expression: null
};

// S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6）: sink group の
// フィクスチャ - `dbConnectionId`は`DB_CONNECTION`（postgres）、`tagIds`は
// `BASE_TAG`/`DB_TAG`の2件を参照する。
const SINK_GROUP: SinkGroup = {
	id: 200,
	name: 'line1-log',
	dbConnectionId: DB_CONNECTION.id,
	mode: 'interval',
	intervalMs: 1000,
	tableName: 'public.tag_history',
	storeBad: false,
	enabled: true,
	tagIds: [BASE_TAG.id, DB_TAG.id]
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
				enabled: true,
				simulation: false,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1, word_order: 'low_high' }
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
				plcConnectionName: 'line-a',
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
				enabled: true,
				writable: false,
				tagKind: 'plc',
				expression: null,
				retain: false
			},
			{
				name: 'tag-calc',
				plcConnectionName: 'calc',
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
						enabled: true,
						defaultWritable: true
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
		const text = JSON.stringify({ ...makePackage(), schemaVersion: 4 }, null, 2);
		expect(() => parseConfigPackage(text)).toThrow(/schemaVersion/);
	});

	// 2026-10-19 オーナー決定: 版 1（グループ・タグを名前だけで参照する形式）は
	// 読まない（互換性は持たない）。版 1 の形のままのパッケージは、参照が曖昧で
	// なくても版で断る。
	it('parseConfigPackage は版 1 のパッケージを未対応の版として拒否する', () => {
		const pkg = makePackage();
		const v1 = {
			...pkg,
			schemaVersion: 1,
			tags: pkg.tags.map(({ plcConnectionName: _c, ...rest }) => rest)
		};
		expect(() => parseConfigPackage(JSON.stringify(v1))).toThrow(
			/schemaVersion 1 は未対応です（期待値: 3）。古い版の構成パッケージは読み込めません/
		);
	});

	// 版 3（R2b-6b-3、2026-10-24）: 版 2（接続が平らな形）も読まない。
	it('parseConfigPackage は版 2 のパッケージ（平らな接続）を未対応の版として拒否する', () => {
		const v2 = {
			...makePackage(),
			schemaVersion: 2,
			plcConnections: [
				{
					name: 'line-a',
					protocol: 'modbus-tcp',
					host: '127.0.0.1',
					port: 502,
					unitId: 1,
					enabled: true,
					simulation: false,
					wordOrder: 'low_high',
					cpuType: 'R'
				}
			]
		};
		expect(() => parseConfigPackage(JSON.stringify(v2))).toThrow(
			/schemaVersion 2 は未対応です（期待値: 3）。古い版の構成パッケージは読み込めません。今の banto-hub で書き出し直してください/
		);
	});

	// --- 版 3: 接続は `{ name, protocol, enabled, simulation, settings }`。

	it('版 3: SLMP 接続の cpu_type（Q）・word_order・simulation を settings のまま往復する', () => {
		const pkg = buildConfigPackage({
			plcConnections: [SLMP_CONNECTION],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-24T00:00:00.000Z'
		});
		expect(pkg.plcConnections).toEqual([
			{
				name: 'line-q',
				protocol: 'slmp',
				enabled: true,
				simulation: true,
				settings: { host: '192.168.11.10', port: 3100, cpu_type: 'Q', word_order: 'high_low' }
			}
		]);
		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed).toEqual(pkg);
		expect(() => checkConfigPackageConnectionFields(parsed, TEST_DRIVERS)).not.toThrow();
	});

	it('版 3: Modbus 接続の unit_id・word_order（high_low）を往復する', () => {
		const modbus: PlcConnection = {
			...BASE_CONNECTION,
			settings: { host: '10.0.0.9', port: 1502, unit_id: 7, word_order: 'high_low' }
		};
		const pkg = buildConfigPackage({
			plcConnections: [modbus],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-24T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed.plcConnections[0].settings).toEqual({
			host: '10.0.0.9',
			port: 1502,
			unit_id: 7,
			word_order: 'high_low'
		});
		expect(() => checkConfigPackageConnectionFields(parsed, TEST_DRIVERS)).not.toThrow();
	});

	it('版 3: settings が無い・オブジェクトでない接続は、項目の場所を示して拒否する', () => {
		const raw = JSON.parse(serializeConfigPackage(makePackage()));
		delete raw.plcConnections[0].settings;
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/plcConnections\[0\]\.settings はオブジェクトである必要があります/
		);
		raw.plcConnections[0].settings = [];
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(/plcConnections\[0\]\.settings/);
	});

	it('版 3: settings に秘密の値（$secret）を持つ接続は拒否する（黙って捨てない）', () => {
		const raw = JSON.parse(serializeConfigPackage(makePackage()));
		raw.plcConnections[0].settings.token = { $secret: 'set' };
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/plcConnections\[0\]\.settings\.token は秘密の値です/
		);
	});

	// --- T19 S1-b（UX-34）+ 版 2: defaultWritable は必須。

	it('parseConfigPackage は defaultWritable を持たないグループを、項目の場所を示して拒否する', () => {
		const pkg = makePackage();
		const withoutDefaultWritable = {
			...pkg,
			collectionGroups: pkg.collectionGroups.map(
				({ defaultWritable: _defaultWritable, ...rest }) => rest
			)
		};
		expect(() => parseConfigPackage(JSON.stringify(withoutDefaultWritable))).toThrow(
			/collectionGroups\[0\]\.defaultWritable/
		);
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

	it('parseConfigPackage は stringEncoding を持たないタグを、項目の場所を示して拒否する', () => {
		const pkg = makePackage();
		const withoutStringEncoding = {
			...pkg,
			tags: pkg.tags.map(({ stringEncoding: _stringEncoding, ...rest }) => rest)
		};
		expect(() => parseConfigPackage(JSON.stringify(withoutStringEncoding))).toThrow(
			/tags\[0\]\.stringEncoding は utf8 または shift_jis/
		);
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

	// --- #533（2026-10-08 オーナー決定「しきい値は使う側（記録計・SCADA）が
	// 持つ設定で、Hub は持たない」）: export はしきい値を書かず、値付きの
	// しきい値を持つ旧パッケージは取り込みを断る（`null`・項目なしは通す）。
	// （#533 の時点では版 1。版 2 は 2026-10-19 にグループ・タグの参照を変えたため。）

	it('#533: buildConfigPackage はタグのしきい値を書かない', () => {
		const pkg = makePackage();
		const text = serializeConfigPackage(pkg);
		expect(text).not.toContain('threshold');
		expect(CONFIG_PACKAGE_SCHEMA_VERSION).toBe(3);
	});

	it('#533: parseConfigPackage は しきい値が null の旧パッケージを受け入れ、しきい値を持ち込まない', () => {
		const pkg = makePackage();
		const old = {
			...pkg,
			tags: pkg.tags.map((t) => ({
				...t,
				thresholdH: null,
				thresholdHh: null,
				thresholdL: null,
				thresholdLl: null
			}))
		};
		const parsed = parseConfigPackage(JSON.stringify(old));
		expect(parsed.tags[0].name).toBe(pkg.tags[0].name);
		expect(Object.keys(parsed.tags[0]).filter((key) => key.startsWith('threshold'))).toEqual([]);
	});

	it('#533: parseConfigPackage は値付きのしきい値を持つ旧パッケージを拒否する（黙って捨てない）', () => {
		const pkg = makePackage();
		const old = {
			...pkg,
			tags: pkg.tags.map((t) => ({ ...t, thresholdH: null, thresholdHh: 90 }))
		};
		expect(() => parseConfigPackage(JSON.stringify(old))).toThrow(
			/tags\[0\]\.thresholdHh にしきい値が入っています/
		);
	});

	// --- S1（docs/banto-hub-external-db-design.md §4.1・§2.2、実装指示5）+ 版 3:
	// postgres（DB Source）接続は settings の host/port/database/username を export に
	// 含むが、パスワード（応答の `{"$secret": …}`）は絶対に含まない。

	it('S1: buildConfigPackage は postgres 接続の settings を含み、password は一切含まない', () => {
		const pkg = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP],
			tags: [BASE_TAG],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-06T00:00:00.000Z'
		});
		const exported = pkg.plcConnections.find((c) => c.name === 'pg-a');
		expect(exported).toEqual({
			name: 'pg-a',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' }
		});
		// シリアライズした JSON テキストの plcConnections 部分自体にも
		// "password"・"$secret" という文字列が一切現れないことを固定する
		// （`excludedSecrets` は「除外した」ことの宣言であって漏洩ではないので除く）。
		const plcConnectionsJson = JSON.stringify(pkg.plcConnections);
		expect(plcConnectionsJson).not.toMatch(/password/i);
		expect(plcConnectionsJson).not.toContain('$secret');
	});

	it('S1: 読めない秘密値（unreadable）も書き出さない', () => {
		const unreadable: PlcConnection = {
			...DB_CONNECTION,
			settings: { ...DB_CONNECTION.settings, password: { $secret: 'unreadable' } }
		};
		const pkg = buildConfigPackage({
			plcConnections: [unreadable],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC
		});
		expect(pkg.plcConnections[0].settings).not.toHaveProperty('password');
	});

	it('S1: CONFIG_PACKAGE_EXCLUDED_SECRETS は接続の秘密のキーを含む', () => {
		expect(CONFIG_PACKAGE_EXCLUDED_SECRETS).toContain('plc_connections.settings.$secret');
	});

	it('S1: postgres 接続を含むパッケージは serializeConfigPackage/parseConfigPackage で往復できる（password 抜きのまま）', () => {
		const original = buildConfigPackage({
			plcConnections: [DB_CONNECTION],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-06T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
		expect(parsed.plcConnections[0]).toMatchObject({
			protocol: 'postgres',
			settings: { database: 'appdb', username: 'appuser' }
		});
		expect(() => checkConfigPackageConnectionFields(parsed, TEST_DRIVERS)).not.toThrow();
	});

	// --- S3（docs/banto-hub-external-db-design.md §7 row S3）: 収集グループの
	// `querySql`（postgres 配下限定）と `tagKind: 'db'` のタグが export/import
	// で往復できること、非 postgres グループでは `querySql` が現れないことを
	// 固定する。

	it('S3: buildConfigPackage は postgres 配下グループの querySql を含み、非 postgres グループには含まない', () => {
		const pkg = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-06T00:00:00.000Z'
		});
		const plcGroup = pkg.collectionGroups.find((g) => g.name === 'group-a');
		const dbGroup = pkg.collectionGroups.find((g) => g.name === 'group-pg');
		expect(plcGroup?.querySql).toBeUndefined();
		// JSON へシリアライズした時点で `querySql: undefined` のキー自体が
		// 落ちることを固定する（`JSON.stringify` は undefined 値のプロパティを
		// 出力しない）- S1 の password 非漏洩テストと同じ考え方。
		expect(JSON.stringify(plcGroup)).not.toMatch(/querySql/);
		expect(dbGroup).toEqual({
			name: 'group-pg',
			plcConnectionName: 'pg-a',
			periodMs: 1000,
			enabled: true,
			defaultWritable: false,
			querySql: 'SELECT id, temperature, running FROM sensors'
		});
		const dbTag = pkg.tags.find((t) => t.name === 'tag-db');
		expect(dbTag).toMatchObject({
			tagKind: 'db',
			address: 'temperature',
			writable: false,
			expression: null
		});
	});

	it('S3: postgres 配下グループ・db タグを含むパッケージは serializeConfigPackage/parseConfigPackage で往復できる', () => {
		const original = buildConfigPackage({
			plcConnections: [DB_CONNECTION],
			collectionGroups: [DB_GROUP],
			tags: [DB_TAG],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-06T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
		expect(parsed.collectionGroups[0].querySql).toBe(
			'SELECT id, temperature, running FROM sensors'
		);
		expect(parsed.tags[0].tagKind).toBe('db');
	});

	it('S3: parseConfigPackage は querySql を持たない旧パッケージ（非 postgres グループのみ）を受け入れる', () => {
		const pkg = makePackage();
		expect(() => parseConfigPackage(JSON.stringify(pkg))).not.toThrow();
		expect(pkg.collectionGroups[0].querySql).toBeUndefined();
	});

	it('S6: buildConfigPackage は sink group を接続名・タグ名で参照する形に変換する', () => {
		const pkg = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		expect(pkg.sinkGroups).toEqual([
			{
				name: 'line1-log',
				dbConnectionName: 'pg-a',
				mode: 'interval',
				intervalMs: 1000,
				tableName: 'public.tag_history',
				storeBad: false,
				enabled: true,
				tags: [
					{ plcConnectionName: 'line-a', collectionGroupName: 'group-a', tagName: 'tag-a' },
					{ plcConnectionName: 'pg-a', collectionGroupName: 'group-pg', tagName: 'tag-db' }
				]
			}
		]);
	});

	it('S6: sinkGroups を省略した buildConfigPackage 呼び出しは空配列になり、export に sinkGroups が書かれる', () => {
		const pkg = makePackage();
		expect(pkg.sinkGroups).toEqual([]);
		const raw = JSON.parse(serializeConfigPackage(pkg)) as Record<string, unknown>;
		expect(raw.sinkGroups).toEqual([]);
		expect(parseConfigPackage(serializeConfigPackage(pkg)).sinkGroups).toEqual([]);
	});

	it('S6: sink group を含むパッケージは serializeConfigPackage/parseConfigPackage で往復できる', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
		expect(parsed.sinkGroups[0].tags.map((ref) => ref.tagName)).toEqual(['tag-a', 'tag-db']);
	});

	it('S6: parseConfigPackage は sinkGroups キーを持たないパッケージを拒否する', () => {
		const pkg = makePackage();
		const raw = JSON.parse(serializeConfigPackage(pkg)) as Record<string, unknown>;
		delete raw.sinkGroups;
		expect(raw.sinkGroups).toBeUndefined();
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/sinkGroups は配列である必要があります/
		);
	});

	it('S6: parseConfigPackage は不正な sinkGroups.mode を拒否する', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		const raw = JSON.parse(serializeConfigPackage(original)) as {
			sinkGroups: Array<Record<string, unknown>>;
		};
		raw.sinkGroups[0].mode = 'hourly';
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(/interval \/ on_change/);
	});

	it('S6: parseConfigPackage は存在しない接続を参照する sinkGroups を拒否する', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		const raw = JSON.parse(serializeConfigPackage(original)) as {
			sinkGroups: Array<Record<string, unknown>>;
		};
		raw.sinkGroups[0].dbConnectionName = 'unknown-connection';
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(/connection/);
	});

	it('S6: parseConfigPackage は存在しないタグを参照する sinkGroups を拒否する', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		const raw = JSON.parse(serializeConfigPackage(original)) as {
			sinkGroups: Array<Record<string, unknown>>;
		};
		raw.sinkGroups[0].tags = [
			{ plcConnectionName: 'line-a', collectionGroupName: 'group-a', tagName: 'no-such-tag' }
		];
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/sinkGroups\[0\]\.tags\[0\].*line-a \/ group-a \/ no-such-tag/
		);
	});

	it('S6: parseConfigPackage は形の崩れた sinkGroups.tags の参照を拒否する', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, DB_GROUP],
			tags: [BASE_TAG, DB_TAG],
			sinkGroups: [SINK_GROUP],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-09-07T00:00:00.000Z'
		});
		const raw = JSON.parse(serializeConfigPackage(original)) as {
			sinkGroups: Array<Record<string, unknown>>;
		};
		// 版 1 の `tagNames`（名前だけ）の形は受け付けない。
		delete raw.sinkGroups[0].tags;
		raw.sinkGroups[0].tagNames = ['tag-a'];
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(/sinkGroups\[0\]\.tags/);
		raw.sinkGroups[0].tags = [{ collectionGroupName: 'group-a', tagName: 'tag-a' }];
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/sinkGroups\[0\]\.tags\[0\]\.plcConnectionName/
		);
	});

	// --- 版 2（2026-10-19 オーナー決定: グループ名は接続内で一意）----------------

	const LINE_B: PlcConnection = {
		...BASE_CONNECTION,
		id: 4,
		name: 'line-b',
		settings: { ...BASE_CONNECTION.settings, port: 503 }
	};
	const GROUP_A_ON_B: CollectionGroup = { ...BASE_GROUP, id: 13, plcConnectionId: LINE_B.id };
	const TAG_A_ON_B: Tag = { ...BASE_TAG, id: 103, collectionGroupId: GROUP_A_ON_B.id };

	it('版 2: 2 つの接続に同じ名前のグループ・同じ名前のタグがあっても往復でき、sink の参照も区別できる', () => {
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION, LINE_B, DB_CONNECTION],
			collectionGroups: [BASE_GROUP, GROUP_A_ON_B],
			tags: [BASE_TAG, TAG_A_ON_B],
			sinkGroups: [{ ...SINK_GROUP, tagIds: [TAG_A_ON_B.id] }],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-19T00:00:00.000Z'
		});
		expect(original.collectionGroups.map((g) => [g.plcConnectionName, g.name])).toEqual([
			['line-a', 'group-a'],
			['line-b', 'group-a']
		]);
		expect(original.tags.map((t) => [t.plcConnectionName, t.collectionGroupName, t.name])).toEqual([
			['line-a', 'group-a', 'tag-a'],
			['line-b', 'group-a', 'tag-a']
		]);
		expect(original.sinkGroups[0].tags).toEqual([
			{ plcConnectionName: 'line-b', collectionGroupName: 'group-a', tagName: 'tag-a' }
		]);
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
	});

	it('版 2: 1 つの接続の 2 つのグループに同じ名前のタグがあっても往復できる', () => {
		const groupB: CollectionGroup = { ...BASE_GROUP, id: 14, name: 'group-b' };
		const tagInB: Tag = { ...BASE_TAG, id: 104, collectionGroupId: groupB.id };
		const original = buildConfigPackage({
			plcConnections: [BASE_CONNECTION],
			collectionGroups: [BASE_GROUP, groupB],
			tags: [BASE_TAG, tagInB],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-19T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(original));
		expect(parsed).toEqual(original);
		expect(parsed.tags.map((t) => t.collectionGroupName)).toEqual(['group-a', 'group-b']);
	});

	it('版 2: 同じ接続に同じ名前のグループが 2 つあるパッケージは拒否する', () => {
		const pkg = makePackage();
		const dup = { ...pkg, collectionGroups: [pkg.collectionGroups[0], pkg.collectionGroups[0]] };
		expect(() => parseConfigPackage(JSON.stringify(dup))).toThrow(
			/collectionGroups\[1\] は 接続 'line-a' のグループ 'group-a' と重複しています/
		);
	});

	it('版 2: 同じグループに同じ名前のタグが 2 つあるパッケージは拒否する', () => {
		const pkg = makePackage();
		const dup = { ...pkg, tags: [pkg.tags[0], pkg.tags[1], pkg.tags[0]] };
		expect(() => parseConfigPackage(JSON.stringify(dup))).toThrow(
			/tags\[2\] は 接続 'line-a' のグループ 'group-a' のタグ 'tag-a' と重複しています/
		);
	});

	it('版 2: タグの所属は接続名とグループ名の組で解決する（別の接続の同名グループには付かない）', () => {
		const pkg = makePackage();
		const wrong = {
			...pkg,
			tags: [{ ...pkg.tags[0], plcConnectionName: 'calc' }]
		};
		expect(() => parseConfigPackage(JSON.stringify(wrong))).toThrow(
			/tags\[0\]\.collectionGroupName: .*接続 'calc' のグループ 'group-a' が見つかりません/
		);
		const missing = {
			...pkg,
			tags: pkg.tags.map(({ plcConnectionName: _c, ...rest }) => rest)
		};
		expect(() => parseConfigPackage(JSON.stringify(missing))).toThrow(
			/tags\[0\]\.plcConnectionName は文字列である必要があります/
		);
	});

	it('planByKey は指定したキーで create/update を分け、キーの無い既存行は照合しない', () => {
		const plan = planByKey(
			[
				{ conn: 'a', name: 'g' },
				{ conn: 'b', name: 'g' }
			],
			[
				{ id: 1, key: JSON.stringify(['b', 'g']) as string | undefined },
				{ id: 2, key: undefined }
			],
			(item) => JSON.stringify([item.conn, item.name]),
			(item) => item.key
		);
		expect(plan.create).toEqual([{ conn: 'a', name: 'g' }]);
		expect(plan.update).toEqual([
			{
				incoming: { conn: 'b', name: 'g' },
				existing: { id: 1, key: JSON.stringify(['b', 'g']) }
			}
		]);
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

// R2a-2e（docs/banto-hub-ingest-design.md §3.1・§5.1）: push 接続の `settings`
// の往復。ブリッジの読む DB は id ではなく接続名（`dbConnectionName`）で持ち運ぶ。
const PUSH_BRIDGE_CONNECTION: PlcConnection = {
	id: 12,
	name: 'Bridge1',
	protocol: 'push',
	enabled: true,
	simulation: false,
	settings: {
		stale_timeout_ms: 5000,
		bridge: {
			kind: 'db',
			db_connection_id: DB_CONNECTION.id,
			queries: [
				{
					name: 'line1',
					period_ms: 1000,
					sql: 'SELECT temp, run FROM line1_now',
					columns: [
						{ column: 'temp', key: 'line1.temp' },
						{ column: 'run', key: 'line1.run' }
					]
				}
			]
		}
	}
};

// 版 3: 既定のままの push 接続の settings は `{}`（REST の応答）。
const PUSH_PLAIN_CONNECTION: PlcConnection = {
	id: 13,
	name: 'Script1',
	protocol: 'push',
	enabled: true,
	simulation: false,
	settings: {}
};

function pushSettingsOf(pkg: ConfigPackage, name: string): ConfigPackagePushSettings {
	return pkg.plcConnections.find((c) => c.name === name)!.settings as ConfigPackagePushSettings;
}

function makePushPackage(): ConfigPackage {
	return buildConfigPackage({
		// push 接続を postgres 接続より前に並べても export は名前で書ける。
		plcConnections: [PUSH_BRIDGE_CONNECTION, PUSH_PLAIN_CONNECTION, DB_CONNECTION, BASE_CONNECTION],
		collectionGroups: [],
		tags: [],
		mqtt: MQTT,
		grpc: GRPC,
		exportedAt: '2026-10-21T00:00:00.000Z'
	});
}

describe('configPackage: R2a-2e push 接続の settings（dbConnectionName）', () => {
	it('export はブリッジの db_connection_id を接続名に写し、既定のままの push は settings: {} を書く', () => {
		const pkg = makePushPackage();
		const bridge = pkg.plcConnections.find((c) => c.name === 'Bridge1');
		expect(bridge?.settings).toEqual({
			stale_timeout_ms: 5000,
			bridge: {
				kind: 'db',
				dbConnectionName: 'pg-a',
				queries: [
					{
						name: 'line1',
						period_ms: 1000,
						sql: 'SELECT temp, run FROM line1_now',
						columns: [
							{ column: 'temp', key: 'line1.temp' },
							{ column: 'run', key: 'line1.run' }
						]
					}
				]
			}
		});
		expect(JSON.stringify(bridge)).not.toContain('db_connection_id');
		expect(pkg.plcConnections.find((c) => c.name === 'Script1')?.settings).toEqual({});
		// 版 3: push 以外の接続も settings を書く（秘密のキーは落とす）。
		expect(pkg.plcConnections.find((c) => c.name === 'pg-a')!.settings).toEqual({
			host: '10.0.0.5',
			port: 5432,
			database: 'appdb',
			username: 'appuser'
		});
		expect(pkg.plcConnections.find((c) => c.name === 'line-a')!.settings).toEqual(
			BASE_CONNECTION.settings
		);
		// API キーは持ち運ばない（項目そのものが無い）。
		expect(serializeConfigPackage(pkg)).not.toContain('ingest:#');
	});

	it('serializeConfigPackage と parseConfigPackage で往復できる', () => {
		const pkg = makePushPackage();
		expect(parseConfigPackage(serializeConfigPackage(pkg))).toEqual(pkg);
	});

	it('取り込み先の id に付け直す（resolvePushSettings）。名前を引けなければ null', () => {
		const pkg = makePushPackage();
		const settings = pushSettingsOf(pkg, 'Bridge1');
		const resolved = resolvePushSettings(settings, (name) => (name === 'pg-a' ? 77 : undefined));
		expect(resolved?.bridge?.db_connection_id).toBe(77);
		expect(resolved?.stale_timeout_ms).toBe(5000);
		expect(resolved?.bridge?.queries).toEqual(settings.bridge!.queries);
		expect(resolvePushSettings(settings, () => undefined)).toBeNull();
		// ブリッジの無い settings は名前を引かない。
		expect(resolvePushSettings({}, () => undefined)).toEqual({});
	});

	it('export の時点でブリッジの読む接続を引けなければ断る（壊れた参照を書き出さない）', () => {
		expect(() =>
			buildConfigPackage({
				plcConnections: [PUSH_BRIDGE_CONNECTION],
				collectionGroups: [],
				tags: [],
				mqtt: MQTT,
				grpc: GRPC
			})
		).toThrow(/Bridge1/);
	});

	it('parseConfigPackage はパッケージに無い接続・postgres でない接続を読むブリッジを断る', () => {
		const pkg = makePushPackage();
		const missing = structuredClone(pkg);
		missing.plcConnections = missing.plcConnections.filter((c) => c.name !== 'pg-a');
		expect(() => parseConfigPackage(serializeConfigPackage(missing))).toThrow(
			/settings\.bridge\.dbConnectionName.*pg-a/
		);
		const notPg = structuredClone(pkg);
		pushSettingsOf(notPg, 'Bridge1').bridge!.dbConnectionName = 'line-a';
		expect(() => parseConfigPackage(serializeConfigPackage(notPg))).toThrow(/postgres/);
	});

	it('parseConfigPackage は形の崩れた push の settings を断る', () => {
		const pkg = makePushPackage();
		const badKind = JSON.parse(serializeConfigPackage(pkg));
		badKind.plcConnections.find(
			(c: { name: string }) => c.name === 'Bridge1'
		).settings.bridge.kind = 'mqtt';
		expect(() => parseConfigPackage(JSON.stringify(badKind))).toThrow(/bridge\.kind/);
		const noColumns = JSON.parse(serializeConfigPackage(pkg));
		delete noColumns.plcConnections.find((c: { name: string }) => c.name === 'Bridge1').settings
			.bridge.queries[0].columns;
		expect(() => parseConfigPackage(JSON.stringify(noColumns))).toThrow(/columns/);
	});

	it('版 3: settings を持たない push 接続（手で書いたパッケージ）は既定で埋めずに断る', () => {
		const raw = JSON.parse(serializeConfigPackage(makePushPackage()));
		delete raw.plcConnections.find((c: { name: string }) => c.name === 'Script1').settings;
		expect(() => parseConfigPackage(JSON.stringify(raw))).toThrow(
			/plcConnections\[1\]\.settings はオブジェクトである必要があります/
		);
	});

	it('版 3: 往復した push のパッケージは欄の確かめも通る（push の欄はどれも任意）', () => {
		const parsed = parseConfigPackage(serializeConfigPackage(makePushPackage()));
		expect(() => checkConfigPackageConnectionFields(parsed, TEST_DRIVERS)).not.toThrow();
	});

	it('案内の見出しは「取り込みのキーは移りません」', () => {
		expect(INGEST_KEYS_NOT_MIGRATED_MESSAGE).toBe('取り込みのキーは移りません');
	});
});

// 版 3（R2b-6b-3、設計 §7.1 の表の最後の行）: 欠けた欄は既定で埋めずに断る。欄はドライバの
// スキーマ（`GET /api/drivers`、ここでは `TEST_DRIVERS`）とホストの接続の表で決める。
describe('configPackage: checkConfigPackageConnectionFields（欠けた欄の断り）', () => {
	function pkgWith(...connections: ConfigPackage['plcConnections']): ConfigPackage {
		return { ...makePackage(), plcConnections: connections };
	}
	const modbus = (settings: Record<string, unknown>) => ({
		name: 'line-a',
		protocol: 'modbus-tcp',
		enabled: true,
		simulation: false,
		settings
	});
	const FULL_MODBUS = { host: '127.0.0.1', port: 502, unit_id: 1, word_order: 'high_low' };

	it('必須で既定の無い欄（host）の欠けは断る', () => {
		const { host: _host, ...rest } = FULL_MODBUS;
		expect(() => checkConfigPackageConnectionFields(pkgWith(modbus(rest)), TEST_DRIVERS)).toThrow(
			/plcConnections\[0\]\.settings\.host がありません（接続 'line-a'。既定値では埋めません/
		);
	});

	it('必須で既定つきの欄（port）の欠け・null は断る', () => {
		const { port: _port, ...rest } = FULL_MODBUS;
		expect(() => checkConfigPackageConnectionFields(pkgWith(modbus(rest)), TEST_DRIVERS)).toThrow(
			/settings\.port がありません/
		);
		expect(() =>
			checkConfigPackageConnectionFields(
				pkgWith(modbus({ ...FULL_MODBUS, port: null })),
				TEST_DRIVERS
			)
		).toThrow(/settings\.port がありません/);
	});

	it('任意で既定つきの欄（unit_id・word_order・SLMP の cpu_type）の欠けも断る', () => {
		const { unit_id: _unit, ...noUnit } = FULL_MODBUS;
		expect(() => checkConfigPackageConnectionFields(pkgWith(modbus(noUnit)), TEST_DRIVERS)).toThrow(
			/settings\.unit_id がありません/
		);
		const { word_order: _order, ...noOrder } = FULL_MODBUS;
		expect(() =>
			checkConfigPackageConnectionFields(pkgWith(modbus(noOrder)), TEST_DRIVERS)
		).toThrow(/settings\.word_order がありません/);
		const slmp = {
			name: 'line-q',
			protocol: 'slmp',
			enabled: true,
			simulation: false,
			settings: { host: '192.168.11.10', port: 3100, word_order: 'low_high' }
		};
		expect(() => checkConfigPackageConnectionFields(pkgWith(slmp), TEST_DRIVERS)).toThrow(
			/settings\.cpu_type がありません/
		);
	});

	it('Secret の欄（必須でも）と既定の無い任意の欄の欠けは通る', () => {
		const fake = {
			name: 'fake',
			protocol: 'fake-secret',
			enabled: true,
			simulation: false,
			settings: { host: 'h' }
		};
		expect(() => checkConfigPackageConnectionFields(pkgWith(fake), TEST_DRIVERS)).not.toThrow();
	});

	it('Secret の欄に値があれば断る（パッケージは秘密値を持たない）', () => {
		const fake = {
			name: 'fake',
			protocol: 'fake-secret',
			enabled: true,
			simulation: false,
			settings: { host: 'h', token: 'plain' }
		};
		expect(() => checkConfigPackageConnectionFields(pkgWith(fake), TEST_DRIVERS)).toThrow(
			/settings\.token は秘密の欄です/
		);
	});

	it('Secret の欄は null・空文字でも断る（REST は消す指示と読むので値でなくキーの有無で見る）', () => {
		for (const value of [null, '', 'x']) {
			const fake = {
				name: 'fake',
				protocol: 'fake-secret',
				enabled: true,
				simulation: false,
				settings: { host: 'h', token: value }
			};
			expect(() => checkConfigPackageConnectionFields(pkgWith(fake), TEST_DRIVERS)).toThrow(
				/settings\.token は秘密の欄です/
			);
		}
		const pg = {
			name: 'pg-a',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings: { host: 'h', port: 5432, database: 'd', username: 'u', password: null }
		};
		expect(() => checkConfigPackageConnectionFields(pkgWith(pg), TEST_DRIVERS)).toThrow(
			/settings\.password は秘密の欄です/
		);
	});

	it('postgres: host・port・database・username の欠けは断り、password の欠けは通り、値があれば断る', () => {
		const pg = (settings: Record<string, unknown>) => ({
			name: 'pg-a',
			protocol: 'postgres',
			enabled: true,
			simulation: false,
			settings
		});
		const full = { host: '10.0.0.5', port: 5432, database: 'appdb', username: 'appuser' };
		expect(() => checkConfigPackageConnectionFields(pkgWith(pg(full)), TEST_DRIVERS)).not.toThrow();
		for (const key of ['host', 'port', 'database', 'username']) {
			const rest: Record<string, unknown> = { ...full };
			delete rest[key];
			expect(() => checkConfigPackageConnectionFields(pkgWith(pg(rest)), TEST_DRIVERS)).toThrow(
				new RegExp(`settings\\.${key} がありません`)
			);
		}
		expect(() =>
			checkConfigPackageConnectionFields(pkgWith(pg({ ...full, password: 'x' })), TEST_DRIVERS)
		).toThrow(/settings\.password は秘密の欄です/);
	});

	it('この Hub に無いプロトコルは断り、virtual は見ない', () => {
		const unknown = { ...modbus(FULL_MODBUS), protocol: 'mysql' };
		expect(() => checkConfigPackageConnectionFields(pkgWith(unknown), TEST_DRIVERS)).toThrow(
			/plcConnections\[0\]\.protocol 'mysql' はこの banto-hub にありません/
		);
		const virtual = { ...modbus({}), name: 'calc', protocol: 'virtual' };
		expect(() => checkConfigPackageConnectionFields(pkgWith(virtual), TEST_DRIVERS)).not.toThrow();
	});

	it('書き出したパッケージは往復して欄の確かめを通る（Modbus・SLMP・postgres・push）', () => {
		const pkg = buildConfigPackage({
			plcConnections: [
				BASE_CONNECTION,
				SLMP_CONNECTION,
				DB_CONNECTION,
				PUSH_BRIDGE_CONNECTION,
				PUSH_PLAIN_CONNECTION,
				VIRTUAL_CONNECTION
			],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-24T00:00:00.000Z'
		});
		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed).toEqual(pkg);
		expect(parsed.plcConnections.map((c) => c.name)).toEqual([
			'line-a',
			'line-q',
			'pg-a',
			'Bridge1',
			'Script1'
		]);
		expect(() => checkConfigPackageConnectionFields(parsed, TEST_DRIVERS)).not.toThrow();
	});
});
