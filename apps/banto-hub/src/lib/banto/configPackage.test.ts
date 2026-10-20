import { describe, expect, it } from 'vitest';
import {
	CONFIG_PACKAGE_EXCLUDED_SECRETS,
	CONFIG_PACKAGE_PRODUCT,
	CONFIG_PACKAGE_SCHEMA_VERSION,
	buildConfigPackage,
	parseConfigPackage,
	planByKey,
	planByName,
	serializeConfigPackage,
	type ConfigPackage
} from './configPackage';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import type { GrpcSettings } from './grpcSettingsAdmin';
import type { MqttSettings } from './mqttSettingsAdmin';
import type { SinkGroup } from './sinkGroupsAdmin';

const BASE_CONNECTION: PlcConnection = {
	id: 1,
	name: 'line-a',
	protocol: 'modbus-tcp',
	host: '127.0.0.1',
	port: 502,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high',
	cpuType: 'R',
	database: null,
	username: null,
	passwordSet: false
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
	wordOrder: 'low_high',
	cpuType: 'R',
	database: null,
	username: null,
	passwordSet: false
};

// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: postgres（DB
// Source）接続のフィクスチャ - `passwordSet: true`（保存済みパスワードあり）
// でも export には一切パスワードが乗らないことを確認する用。
const DB_CONNECTION: PlcConnection = {
	id: 3,
	name: 'pg-a',
	protocol: 'postgres',
	host: '10.0.0.5',
	port: 5432,
	unitId: 1,
	enabled: true,
	simulation: false,
	wordOrder: 'low_high',
	cpuType: 'R',
	database: 'appdb',
	username: 'appuser',
	passwordSet: true
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
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				cpuType: 'R'
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
		const text = JSON.stringify({ ...makePackage(), schemaVersion: 3 }, null, 2);
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
			/schemaVersion 1 は未対応です（期待値: 2）。古い版の構成パッケージは読み込めません/
		);
	});

	// --- 版 2（2026-10-20 オーナー決定）: 版 1 の間に足した項目（wordOrder・cpuType・
	// defaultWritable・stringEncoding・sinkGroups）は必須。欠けたパッケージは
	// 項目の場所を示して断り、既定値で黙って埋めない。

	it('parseConfigPackage は wordOrder を持たない接続を、項目の場所を示して拒否する', () => {
		const pkg = makePackage();
		const withoutWordOrder = {
			...pkg,
			plcConnections: pkg.plcConnections.map(({ wordOrder: _wordOrder, ...rest }) => rest)
		};
		expect(() => parseConfigPackage(JSON.stringify(withoutWordOrder))).toThrow(
			/plcConnections\[0\]\.wordOrder は low_high \/ high_low/
		);
	});

	it('parseConfigPackage は不正な wordOrder を拒否する', () => {
		const pkg = makePackage();
		const withBadWordOrder = {
			...pkg,
			plcConnections: pkg.plcConnections.map((c) => ({ ...c, wordOrder: 'middle_endian' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadWordOrder))).toThrow(/wordOrder/);
	});

	// #605 + 版 2: cpuType は必須（省略・null は拒否。R を入れて既存の Q 接続を
	// 戻してしまうことがない）。明示した Q は往復する。未知の値も拒否する。
	it('parseConfigPackage は cpuType を持たない接続を、項目の場所を示して拒否する', () => {
		const pkg = makePackage();
		const withoutCpu = {
			...pkg,
			plcConnections: pkg.plcConnections.map(({ cpuType: _cpuType, ...rest }) => rest)
		};
		expect(() => parseConfigPackage(JSON.stringify(withoutCpu))).toThrow(
			/plcConnections\[0\]\.cpuType は Q \/ R/
		);
		const withNullCpu = {
			...pkg,
			plcConnections: pkg.plcConnections.map((c) => ({ ...c, cpuType: null }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withNullCpu))).toThrow(/cpuType/);
	});

	it('buildConfigPackage/parseConfigPackage は SLMP 接続の cpuType Q を往復する', () => {
		const qSeries: PlcConnection = {
			...BASE_CONNECTION,
			protocol: 'slmp',
			port: 5007,
			cpuType: 'Q'
		};
		const pkg = buildConfigPackage({
			plcConnections: [qSeries],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-10-14T00:00:00.000Z'
		});
		expect(pkg.plcConnections[0].cpuType).toBe('Q');
		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed.plcConnections[0].cpuType).toBe('Q');
	});

	it('parseConfigPackage は不正な cpuType を拒否する', () => {
		const pkg = makePackage();
		const withBadCpu = {
			...pkg,
			plcConnections: pkg.plcConnections.map((c) => ({ ...c, cpuType: 'L' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadCpu))).toThrow(/cpuType/);
	});

	// --- 2026-09-08 オーナー決定（issue #325 の修正）: modbus-tcp 接続の
	// wordOrder（既定が high_low に変わった）が export/import で正しく
	// 往復すること。以前は UI が modbus-tcp でこのフィールドを一切編集させず
	// 常に列の初期値 'low_high' が保存されていたため、この経路は事実上
	// 'low_high' しか通っていなかった - 'high_low' を明示的に持つ modbus-tcp
	// 接続が壊れずに往復することを固定する。

	it('buildConfigPackage/parseConfigPackage は modbus-tcp 接続の wordOrder（high_low）を正しく往復する', () => {
		const modbusHighLow: PlcConnection = { ...BASE_CONNECTION, wordOrder: 'high_low' };
		const pkg = buildConfigPackage({
			plcConnections: [modbusHighLow],
			collectionGroups: [],
			tags: [],
			mqtt: MQTT,
			grpc: GRPC,
			exportedAt: '2026-08-11T00:00:00.000Z'
		});
		expect(pkg.plcConnections[0].wordOrder).toBe('high_low');

		const parsed = parseConfigPackage(serializeConfigPackage(pkg));
		expect(parsed.plcConnections[0].wordOrder).toBe('high_low');
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
		expect(CONFIG_PACKAGE_SCHEMA_VERSION).toBe(2);
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

	// --- S1（docs/banto-hub-external-db-design.md §4.1・§2.2、実装指示5）:
	// postgres（DB Source）接続は database/username を export に含むが、
	// パスワードは絶対に含まない - `CONFIG_PACKAGE_EXCLUDED_SECRETS`に
	// `'plc_connections.password'`が追加されていること、実際に`password`
	// キーが export の JSON に一切現れないことの両方を固定する。

	it('S1: buildConfigPackage は postgres 接続の database/username を含み、password は一切含まない', () => {
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
			host: '10.0.0.5',
			port: 5432,
			unitId: 1,
			enabled: true,
			simulation: false,
			wordOrder: 'low_high',
			cpuType: 'R',
			database: 'appdb',
			username: 'appuser'
		});
		expect(exported).not.toHaveProperty('password');
		expect(exported).not.toHaveProperty('passwordSet');
		// シリアライズした JSON テキストの plcConnections 部分自体にも
		// "password" という文字列が一切現れないことを固定する
		// （フィールド名の綴りミスでうっかり別の場所から漏れる、といった
		// 事故の防止線）。`excludedSecrets`（'plc_connections.password' /
		// 'mqtt.password'という文字列そのもの）はこの一致対象から除く -
		// あちらは「除外した」ことの宣言であって漏洩ではない。
		const plcConnectionsJson = JSON.stringify(pkg.plcConnections);
		expect(plcConnectionsJson).not.toMatch(/password/i);
	});

	it('S1: CONFIG_PACKAGE_EXCLUDED_SECRETS は plc_connections.password を含む', () => {
		expect(CONFIG_PACKAGE_EXCLUDED_SECRETS).toContain('plc_connections.password');
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
			database: 'appdb',
			username: 'appuser'
		});
	});

	it('S1: parseConfigPackage は database/username を持たない旧パッケージ（非postgres接続のみ）を受け入れる', () => {
		const pkg = makePackage();
		const withoutDbFields = {
			...pkg,
			plcConnections: pkg.plcConnections.map(
				({ database: _database, username: _username, ...rest }) => rest
			)
		};
		expect(() => parseConfigPackage(JSON.stringify(withoutDbFields))).not.toThrow();
	});

	it('S1: parseConfigPackage は不正な protocol（postgres 追加後の許容値外）を拒否する', () => {
		const pkg = makePackage();
		const withBadProtocol = {
			...pkg,
			plcConnections: pkg.plcConnections.map((c) => ({ ...c, protocol: 'mysql' }))
		};
		expect(() => parseConfigPackage(JSON.stringify(withBadProtocol))).toThrow(/protocol/);
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

	const LINE_B: PlcConnection = { ...BASE_CONNECTION, id: 4, name: 'line-b', port: 503 };
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
