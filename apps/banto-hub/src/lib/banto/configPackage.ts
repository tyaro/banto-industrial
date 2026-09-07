import {
	type CollectionGroup,
	type CollectionGroupInput,
	type PlcConnection,
	type PlcConnectionInput,
	type PlcProtocol,
	type WordOrder,
	type StringEncoding,
	type Tag,
	type TagInput
} from './tagRegistryAdmin';
import { type GrpcSettings } from './grpcSettingsAdmin';
import { type MqttSettings } from './mqttSettingsAdmin';
import type { SinkGroup, SinkGroupMode } from './sinkGroupsAdmin';

export const CONFIG_PACKAGE_SCHEMA_VERSION = 1 as const;
export const CONFIG_PACKAGE_PRODUCT = 'banto-hub' as const;

export const CONFIG_PACKAGE_EXCLUDED_SECRETS = [
	'mqtt.username',
	'mqtt.password',
	'users.password_hash',
	'api_keys.key_hash',
	'sessions',
	'audit_logs',
	'history',
	// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: postgres 接続の
	// パスワードは平文保存される秘密情報 - `mqtt.password`と同じ理由で
	// export に含めない（`sanitizeConnection`が実際に落とす。ここでの
	// 列挙は「除外した」ことを export 自体の中に明記するための一覧で、
	// 実際の除外動作を担うわけではない - `mqtt.password`も同じ関係）。
	'plc_connections.password'
] as const;

/**
 * S1: `PlcConnectionInput`から`password`を除いた形 - **export には絶対に
 * パスワードを乗せない**（`CONFIG_PACKAGE_EXCLUDED_SECRETS`の
 * `'plc_connections.password'`参照）。`sanitizeConnection`がこの型に
 * 詰め替える際、`password`フィールド自体を持たないこの型を使うことで
 * 「うっかり詰め忘れる」ではなく「そもそも代入できない」という形で
 * コンパイル時に保証する。
 */
export type ConfigPackagePlcConnection = Omit<PlcConnectionInput, 'password'>;

export interface ConfigPackageCollectionGroup extends Omit<
	CollectionGroupInput,
	'plcConnectionId'
> {
	plcConnectionName: string;
}

export interface ConfigPackageTag extends Omit<TagInput, 'collectionGroupId' | 'expectedRevision'> {
	collectionGroupName: string;
}

export type ConfigPackageMqttSettings = Omit<MqttSettings, 'username'>;
export type ConfigPackageGrpcSettings = GrpcSettings;

/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6、実装指示6）: DB
 * Sink group 1件分 - `SinkGroup`から`id`/`dbConnectionId`/`tagIds`
 * （数値 id、環境をまたぐと再現できない）を除き、代わりに**名前で**参照する
 * （`ConfigPackageCollectionGroup::plcConnectionName`と同じ考え方 - タグの
 * 参照は他のエンティティ（`ConfigPackageTag`）と同じく`name`をキーにする
 * ことで、タグ id が環境間で異なっても import 側で解決できる）。
 * `storeBad`/`enabled`/`mode`/`intervalMs`/`tableName`は秘密情報を含まない
 * ためそのまま export する（`CONFIG_PACKAGE_EXCLUDED_SECRETS`に追加項目は
 * 無い - sink group 自体に資格情報は無く、参照する DB 接続のパスワードは
 * 既に`plc_connections.password`として除外済み）。
 */
export interface ConfigPackageSinkGroup {
	name: string;
	dbConnectionName: string;
	mode: SinkGroupMode;
	intervalMs: number;
	tableName: string;
	storeBad: boolean;
	enabled: boolean;
	tagNames: string[];
}

export interface ConfigPackage {
	schemaVersion: typeof CONFIG_PACKAGE_SCHEMA_VERSION;
	product: typeof CONFIG_PACKAGE_PRODUCT;
	exportedAt: string;
	excludedSecrets: readonly string[];
	plcConnections: ConfigPackagePlcConnection[];
	collectionGroups: ConfigPackageCollectionGroup[];
	tags: ConfigPackageTag[];
	mqtt: ConfigPackageMqttSettings;
	grpc: ConfigPackageGrpcSettings;
	/**
	 * S6: 旧スキーマ（この項目を持たないパッケージ）との後方互換のため、
	 * パース後は常に配列（空配列を含む）で埋める - `parseConfigPackage`の
	 * `parseSinkGroups`が省略時に`[]`へフォールバックする
	 * （`expectWordOrder`等と同じ「後方互換な追加フィールドはバージョンを
	 * 上げない」方針、実装指示6「old packages without the key still
	 * import」）。
	 */
	sinkGroups: ConfigPackageSinkGroup[];
}

export interface ConfigPackageInspectionCounts {
	plcConnections: { create: number; update: number };
	collectionGroups: { create: number; update: number };
	tags: { create: number; update: number };
	sinkGroups: { create: number; update: number };
}

export interface ConfigPackageInspection {
	counts: ConfigPackageInspectionCounts;
	warnings: string[];
	mqttCredentialsRequired: boolean;
	mqttSettings: ConfigPackageMqttSettings;
	grpcSettings: ConfigPackageGrpcSettings;
	/**
	 * S1（docs/banto-hub-external-db-design.md §4.1・§2.2、実装指示5）:
	 * このパッケージが含む postgres（DB Source）接続の名前一覧。
	 * `mqttCredentialsRequired`と同じ立て付け（MQTTのユーザー名・
	 * パスワードが export に含まれないのと同じ理由で、postgres接続の
	 * パスワードも含まれない - `CONFIG_PACKAGE_EXCLUDED_SECRETS`の
	 * `'plc_connections.password'`参照）だが、こちらは接続ごとに複数
	 * 存在しうるため配列にする。空配列ならこのパッケージに postgres 接続を
	 * 含まない。`settings/+page.svelte`が MQTT の通知と同じ場所に
	 * 「インポート後にパスワードの再設定が必要」の案内を出すために使う。
	 */
	dbConnectionsPasswordRequired: string[];
}

export interface ConfigPackageImportSummary {
	counts: ConfigPackageInspectionCounts;
	mqttApplied: boolean;
	grpcApplied: boolean;
	warnings: string[];
}

export interface ConfigPackageImportOptions {
	mqttUsername?: string;
	mqttPassword?: string;
}

export class ConfigPackageParseError extends Error {
	constructor(message: string) {
		super(message);
		this.name = 'ConfigPackageParseError';
	}
}

function isVirtualConnection(connection: Pick<PlcConnection, 'protocol'>): boolean {
	return connection.protocol === 'virtual';
}

function stripBom(text: string): string {
	return text.length > 0 && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null;
}

function asString(value: unknown): string | undefined {
	return typeof value === 'string' ? value : undefined;
}

function asNumber(value: unknown): number | undefined {
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function asInteger(value: unknown): number | undefined {
	if (!Number.isInteger(value)) return undefined;
	return asNumber(value);
}

function asBoolean(value: unknown): boolean | undefined {
	return typeof value === 'boolean' ? value : undefined;
}

function asNullableString(value: unknown): string | null | undefined {
	if (value === undefined) return undefined;
	if (value === null) return null;
	return typeof value === 'string' ? value : undefined;
}

function asNullableNumber(value: unknown): number | null | undefined {
	if (value === undefined) return undefined;
	if (value === null) return null;
	return asNumber(value);
}

function expectRecord(value: unknown, path: string): Record<string, unknown> {
	if (!isRecord(value))
		throw new ConfigPackageParseError(`${path} はオブジェクトである必要があります`);
	return value;
}

function expectString(value: unknown, path: string): string {
	const result = asString(value);
	if (result === undefined) {
		throw new ConfigPackageParseError(`${path} は文字列である必要があります`);
	}
	return result;
}

function expectInteger(value: unknown, path: string): number {
	const result = asInteger(value);
	if (result === undefined) {
		throw new ConfigPackageParseError(`${path} は整数である必要があります`);
	}
	return result;
}

function expectBoolean(value: unknown, path: string): boolean {
	const result = asBoolean(value);
	if (result === undefined) {
		throw new ConfigPackageParseError(`${path} は真偽値である必要があります`);
	}
	return result;
}

function expectNullableString(value: unknown, path: string): string | null {
	const result = asNullableString(value);
	if (result === undefined) {
		throw new ConfigPackageParseError(`${path} は文字列または null である必要があります`);
	}
	return result;
}

function expectNullableNumber(value: unknown, path: string): number | null {
	const result = asNullableNumber(value);
	if (result === undefined) {
		throw new ConfigPackageParseError(`${path} は数値または null である必要があります`);
	}
	return result;
}

/**
 * P3-b（監査指摘 2026-08-12）: `wordOrder` は既存のエクスポート済み構成
 * パッケージ（この列を持たない旧スキーマ）にはまだ存在しない可能性がある
 * ので、`expectString` 等と違い省略を許容する — 省略時はバックエンドの既定
 * （`banto_tags::plc_connection::default_word_order`、`"low_high"`）と同じ
 * 値にフォールバックし、旧パッケージのインポートを壊さない
 * （`CONFIG_PACKAGE_SCHEMA_VERSION` は据え置き — 後方互換な追加フィールド
 * なのでバージョンを上げる理由がない）。値が存在する場合は
 * `low_high`/`high_low` のいずれかであることを検証する。
 *
 * 2026-09-08 オーナー決定（issue #325 の作業中に発覚した既存バグの修正）で
 * ワード順はプロトコル依存になった: modbus-tcp は `high_low`、それ以外は
 * `low_high`。省略時フォールバックも**同じくプロトコル依存**にしてある。
 *
 * ここを無条件 `low_high` にしてはいけない理由: 省略が起きるのは
 * 「`wordOrder` 自体を一度も知らない極めて古いパッケージ」だが、そのパッケージが
 * 書き出された当時、modbus-tcp 接続の収集経路は列を無視して `HighLow` で
 * 動いていた（`banto-collect` の取りこぼし）。つまり当時の実挙動は
 * `high_low` であり、`low_high` を埋めるとインポート時に静かに挙動が反転して
 * しまう。migration 0017 が既存 modbus 行を `high_low` へ backfill するのと
 * 全く同じ「実態への同期」をここでも行う。
 *
 * この関数が返す型名は元 `SlmpWordOrder` から `WordOrder` に改名した
 * （SLMP専用ではなくなったため）。
 */
function expectWordOrder(value: unknown, path: string, protocol: PlcProtocol): WordOrder {
	if (value === undefined) return protocol === 'modbus-tcp' ? 'high_low' : 'low_high';
	if (value === 'low_high' || value === 'high_low') return value;
	throw new ConfigPackageParseError(
		`${path} は low_high / high_low のいずれかである必要があります`
	);
}

/**
 * `stringEncoding` は既存のエクスポート済み構成パッケージ（この項目を
 * 持たない旧スキーマ）にはまだ存在しない可能性があるので、
 * `expectWordOrder` と同じ理由で省略を許容する — 省略時は既定値
 * `'utf8'` にフォールバックし、旧パッケージのインポートを壊さない
 * （`CONFIG_PACKAGE_SCHEMA_VERSION` は据え置き — 後方互換な追加
 * フィールドなのでバージョンを上げる理由がない）。値が存在する場合は
 * `utf8`/`shift_jis` のいずれかであることを検証する。
 */
function expectStringEncoding(value: unknown, path: string): StringEncoding {
	if (value === undefined || value === null) return 'utf8';
	if (value === 'utf8' || value === 'shift_jis') return value;
	throw new ConfigPackageParseError(`${path} は utf8 または shift_jis のいずれかです`);
}

/**
 * T19 S1-b（UX-34、2026-09-02 オーナー決定）: `defaultWritable` は既存の
 * エクスポート済み構成パッケージ（この項目を持たない旧スキーマ）には
 * まだ存在しない可能性があるので、`expectWordOrder` と同じ理由で省略を
 * 許容する — 省略時はバックエンドの既定（`banto_tags::collection_group::
 * default_writable_true`、`true`）と同じ値にフォールバックし、旧
 * パッケージのインポートを壊さない（`CONFIG_PACKAGE_SCHEMA_VERSION` は
 * 据え置き — 後方互換な追加フィールドなのでバージョンを上げる理由が
 * ない）。値が存在する場合は真偽値であることを検証する。
 */
function expectDefaultWritable(value: unknown, path: string): boolean {
	if (value === undefined) return true;
	return expectBoolean(value, path);
}

function ensureUniqueNames<T extends { name: string }>(items: readonly T[], path: string): void {
	const seen = new Set<string>();
	for (const item of items) {
		if (seen.has(item.name)) {
			throw new ConfigPackageParseError(`${path} に重複した name '${item.name}' が含まれています`);
		}
		seen.add(item.name);
	}
}

function filterVirtualConnections(connections: readonly PlcConnection[]): PlcConnection[] {
	return connections.filter((connection) => !isVirtualConnection(connection));
}

/**
 * S1: `PlcConnection`（GET応答、`password`列を持たない - `passwordSet`の
 * みを持つ）から export 用の形へ詰め替える。`database`/`username`は
 * postgres 以外では常に`null`（`PlcConnectionResponse::from`参照）なので、
 * そのまま`undefined`に変換して非 postgres 接続の export 形を変えない
 * （既存スキーマとの互換 - 後述`parsePlcConnections`の後方互換ルール
 * 参照）。`password`はそもそも入力に存在しない（`PlcConnection`型に
 * フィールド自体が無い）ため、詰め忘れようがない。
 */
function sanitizeConnection(input: PlcConnection): ConfigPackagePlcConnection {
	const { name, protocol, host, port, unitId, enabled, simulation, wordOrder, database, username } =
		input;
	return {
		name,
		protocol,
		host,
		port,
		unitId,
		enabled,
		simulation,
		wordOrder,
		database: database ?? undefined,
		username: username ?? undefined
	};
}

function sanitizeGroup(
	input: CollectionGroup,
	connectionName: string
): ConfigPackageCollectionGroup {
	return {
		name: input.name,
		plcConnectionName: connectionName,
		periodMs: input.periodMs,
		enabled: input.enabled,
		defaultWritable: input.defaultWritable,
		// S3（docs/banto-hub-external-db-design.md §4.1・§4.2）: `database`/
		// `username`（S1）と同じ「postgres 以外では常に null → undefined」
		// の変換（`CollectionGroupInput::querySql`は`string | undefined`）。
		// `querySql`自体はパスワードのような秘密情報ではないため
		// `CONFIG_PACKAGE_EXCLUDED_SECRETS`の対象にしない - そのまま export
		// する。
		querySql: input.querySql ?? undefined
	};
}

function sanitizeTag(input: Tag, groupName: string): ConfigPackageTag {
	return {
		name: input.name,
		collectionGroupName: groupName,
		address: input.address,
		dataType: input.dataType,
		stringLength: input.stringLength,
		stringEncoding: input.stringEncoding,
		rawLo: input.rawLo,
		rawHi: input.rawHi,
		engLo: input.engLo,
		engHi: input.engHi,
		unit: input.unit,
		decimals: input.decimals,
		thresholdH: input.thresholdH,
		thresholdHh: input.thresholdHh,
		thresholdL: input.thresholdL,
		thresholdLl: input.thresholdLl,
		enabled: input.enabled,
		writable: input.writable,
		tagKind: input.tagKind,
		expression: input.expression,
		retain: input.retain
	};
}

/**
 * S6: `SinkGroup`（GET応答、`dbConnectionId`/`tagIds`は数値 id）から export
 * 用の形へ詰め替える。`dbConnectionName`は呼び出し元（`buildConfigPackage`）
 * が解決済みの接続名を渡す - `sanitizeGroup`が`connectionName`を引数で
 * 受け取るのと同じ形。`tagIds`は`tagNameById`で名前へ変換し、解決できない
 * id（`hub_sink_group_tags`の孤児許容 - `crate::sink`のモジュール doc「FK を
 * 張らない理由」参照。タグ削除時は能動的にクリーンアップされるため、
 * export 時点でこの分岐へ入ることは通常無い）は黙って読み飛ばす -
 * `build_sink_config_response`（Hub側）と同じ「読み飛ばす」方針。
 */
function sanitizeSinkGroup(
	input: SinkGroup,
	dbConnectionName: string,
	tagNameById: ReadonlyMap<number, string>
): ConfigPackageSinkGroup {
	return {
		name: input.name,
		dbConnectionName,
		mode: input.mode,
		intervalMs: input.intervalMs,
		tableName: input.tableName,
		storeBad: input.storeBad,
		enabled: input.enabled,
		tagNames: input.tagIds
			.map((tagId) => tagNameById.get(tagId))
			.filter((name): name is string => name !== undefined)
	};
}

export function buildConfigPackage(input: {
	plcConnections: readonly PlcConnection[];
	collectionGroups: readonly CollectionGroup[];
	tags: readonly Tag[];
	mqtt: MqttSettings;
	grpc: GrpcSettings;
	/**
	 * S6（実装指示6）: 省略時は`[]`（sink group を持たない環境からの
	 * export と同じ形になる - `parseConfigPackage`側の後方互換フォール
	 * バックと対称）。
	 */
	sinkGroups?: readonly SinkGroup[];
	exportedAt?: string;
}): ConfigPackage {
	const connectionById = new Map(
		input.plcConnections.map((connection) => [connection.id, connection])
	);
	const groupById = new Map(input.collectionGroups.map((group) => [group.id, group]));
	const tagNameById = new Map(input.tags.map((tag) => [tag.id, tag.name]));

	const collectionGroups = input.collectionGroups.map((group) => {
		const connection = connectionById.get(group.plcConnectionId);
		if (!connection) {
			throw new ConfigPackageParseError(
				`collection group '${group.name}' の接続 id=${group.plcConnectionId} に対応する connection が見つかりません`
			);
		}
		return sanitizeGroup(group, connection.name);
	});

	const tags = input.tags.map((tag) => {
		const group = groupById.get(tag.collectionGroupId);
		if (!group) {
			throw new ConfigPackageParseError(
				`tag '${tag.name}' の collectionGroupId=${tag.collectionGroupId} に対応する group が見つかりません`
			);
		}
		return sanitizeTag(tag, group.name);
	});

	const sinkGroups = (input.sinkGroups ?? []).map((group) => {
		const connection = connectionById.get(group.dbConnectionId);
		if (!connection) {
			throw new ConfigPackageParseError(
				`sink group '${group.name}' の接続 id=${group.dbConnectionId} に対応する connection が見つかりません`
			);
		}
		return sanitizeSinkGroup(group, connection.name, tagNameById);
	});

	return {
		schemaVersion: CONFIG_PACKAGE_SCHEMA_VERSION,
		product: CONFIG_PACKAGE_PRODUCT,
		exportedAt: input.exportedAt ?? new Date().toISOString(),
		excludedSecrets: CONFIG_PACKAGE_EXCLUDED_SECRETS,
		plcConnections: filterVirtualConnections(input.plcConnections).map(sanitizeConnection),
		collectionGroups,
		tags,
		sinkGroups,
		mqtt: {
			enabled: input.mqtt.enabled,
			host: input.mqtt.host,
			port: input.mqtt.port,
			clientId: input.mqtt.clientId,
			prefix: input.mqtt.prefix,
			qos: input.mqtt.qos,
			minIntervalMs: input.mqtt.minIntervalMs
		},
		grpc: {
			enabled: input.grpc.enabled,
			bind: input.grpc.bind,
			port: input.grpc.port
		}
	};
}

export function serializeConfigPackage(pkg: ConfigPackage): string {
	return `${JSON.stringify(pkg, null, 2)}\n`;
}

/**
 * S1（docs/banto-hub-external-db-design.md §4.1）: `database`/`username`は
 * 既存のエクスポート済み構成パッケージ（この列を持たない旧スキーマ、
 * または postgres 以外の接続）にはまだ存在しない可能性があるので、
 * `expectWordOrder`と同じ理由で省略を許容する - 省略/`null`時は
 * `undefined`にフォールバックし、旧パッケージのインポートを壊さない
 * （`CONFIG_PACKAGE_SCHEMA_VERSION`は据え置き - 後方互換な追加
 * フィールドなのでバージョンを上げる理由がない、#264の
 * `expectStringEncoding`と同じ判断）。値が存在する場合は文字列であることを
 * 検証する（postgres での必須チェックは`plcConnectionForm.ts
 * ::validatePostgresFields`と同じ内容をここでは行わない - configPackage の
 * パースは「形として妥当か」だけを見て、実際の作成/更新時のサーバー側
 * バリデーションに委ねる。他の`ConfigPackage*`パーサーも同じ方針
 * （例: `parseTags`が`tagKind`の placement ルールをサーバーに委ねるのと
 * 同じ）。
 */
function expectOptionalString(value: unknown, path: string): string | undefined {
	if (value === undefined || value === null) return undefined;
	return expectString(value, path);
}

function parsePlcConnections(raw: unknown): ConfigPackagePlcConnection[] {
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('plcConnections は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const item = expectRecord(entry, `plcConnections[${index}]`);
		const protocol = expectString(
			item.protocol,
			`plcConnections[${index}].protocol`
		) as PlcProtocol;
		if (
			protocol !== 'modbus-tcp' &&
			protocol !== 'slmp' &&
			protocol !== 'virtual' &&
			protocol !== 'postgres'
		) {
			throw new ConfigPackageParseError(
				`plcConnections[${index}].protocol は modbus-tcp / slmp / virtual / postgres のいずれかである必要があります`
			);
		}
		return {
			name: expectString(item.name, `plcConnections[${index}].name`),
			protocol,
			host: expectString(item.host, `plcConnections[${index}].host`),
			port: expectInteger(item.port, `plcConnections[${index}].port`),
			unitId: expectInteger(item.unitId, `plcConnections[${index}].unitId`),
			enabled: expectBoolean(item.enabled, `plcConnections[${index}].enabled`),
			simulation: expectBoolean(item.simulation, `plcConnections[${index}].simulation`),
			wordOrder: expectWordOrder(item.wordOrder, `plcConnections[${index}].wordOrder`, protocol),
			database: expectOptionalString(item.database, `plcConnections[${index}].database`),
			username: expectOptionalString(item.username, `plcConnections[${index}].username`)
		};
	});
}

function parseCollectionGroups(raw: unknown): ConfigPackageCollectionGroup[] {
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('collectionGroups は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const item = expectRecord(entry, `collectionGroups[${index}]`);
		return {
			name: expectString(item.name, `collectionGroups[${index}].name`),
			plcConnectionName: expectString(
				item.plcConnectionName,
				`collectionGroups[${index}].plcConnectionName`
			),
			periodMs: expectInteger(item.periodMs, `collectionGroups[${index}].periodMs`),
			enabled: expectBoolean(item.enabled, `collectionGroups[${index}].enabled`),
			defaultWritable: expectDefaultWritable(
				item.defaultWritable,
				`collectionGroups[${index}].defaultWritable`
			),
			// S3: `querySql`は既存のエクスポート済み構成パッケージ（この項目を
			// 持たない旧スキーマ、または postgres 以外の接続配下のグループ）
			// にはまだ存在しない可能性があるので、`database`/`username`
			// （S1、`expectOptionalString`）と同じ理由で省略を許容する -
			// 省略/`null`時は`undefined`にフォールバックする（postgres 配下
			// での必須チェックはここでは行わない - `expectOptionalString`の
			// doc comment と同じ方針、実際の作成/更新時のサーバー側
			// バリデーションに委ねる）。
			querySql: expectOptionalString(item.querySql, `collectionGroups[${index}].querySql`)
		};
	});
}

function parseTags(raw: unknown): ConfigPackageTag[] {
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('tags は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const item = expectRecord(entry, `tags[${index}]`);
		return {
			name: expectString(item.name, `tags[${index}].name`),
			collectionGroupName: expectString(
				item.collectionGroupName,
				`tags[${index}].collectionGroupName`
			),
			address: expectString(item.address, `tags[${index}].address`),
			dataType: expectString(item.dataType, `tags[${index}].dataType`) as TagInput['dataType'],
			stringLength: expectNullableNumber(item.stringLength, `tags[${index}].stringLength`),
			stringEncoding: expectStringEncoding(item.stringEncoding, `tags[${index}].stringEncoding`),
			rawLo: expectNullableNumber(item.rawLo, `tags[${index}].rawLo`),
			rawHi: expectNullableNumber(item.rawHi, `tags[${index}].rawHi`),
			expression: expectNullableString(item.expression, `tags[${index}].expression`),
			engLo: expectNullableNumber(item.engLo, `tags[${index}].engLo`),
			engHi: expectNullableNumber(item.engHi, `tags[${index}].engHi`),
			unit: expectNullableString(item.unit, `tags[${index}].unit`),
			decimals: expectInteger(item.decimals, `tags[${index}].decimals`),
			thresholdH: expectNullableNumber(item.thresholdH, `tags[${index}].thresholdH`),
			thresholdHh: expectNullableNumber(item.thresholdHh, `tags[${index}].thresholdHh`),
			thresholdL: expectNullableNumber(item.thresholdL, `tags[${index}].thresholdL`),
			thresholdLl: expectNullableNumber(item.thresholdLl, `tags[${index}].thresholdLl`),
			enabled: expectBoolean(item.enabled, `tags[${index}].enabled`),
			writable: expectBoolean(item.writable, `tags[${index}].writable`),
			tagKind: expectString(item.tagKind, `tags[${index}].tagKind`) as TagInput['tagKind'],
			retain: expectBoolean(item.retain, `tags[${index}].retain`)
		};
	});
}

/**
 * S6（実装指示6「old packages without the key still import」）:
 * `sinkGroups`は既存のエクスポート済み構成パッケージ（この項目を持たない
 * 旧スキーマ）にはまだ存在しない可能性があるので、`querySql`/`database`と
 * 同じ理由で省略を許容する - 省略時は`[]`にフォールバックする
 * （`CONFIG_PACKAGE_SCHEMA_VERSION`は据え置き - 後方互換な追加フィールド
 * なのでバージョンを上げる理由がない）。
 */
function parseSinkGroups(raw: unknown): ConfigPackageSinkGroup[] {
	if (raw === undefined) return [];
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('sinkGroups は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const item = expectRecord(entry, `sinkGroups[${index}]`);
		const mode = expectString(item.mode, `sinkGroups[${index}].mode`);
		if (mode !== 'interval' && mode !== 'on_change') {
			throw new ConfigPackageParseError(
				`sinkGroups[${index}].mode は interval / on_change のいずれかである必要があります`
			);
		}
		const tagNamesRaw = item.tagNames;
		if (!Array.isArray(tagNamesRaw)) {
			throw new ConfigPackageParseError(`sinkGroups[${index}].tagNames は配列である必要があります`);
		}
		return {
			name: expectString(item.name, `sinkGroups[${index}].name`),
			dbConnectionName: expectString(
				item.dbConnectionName,
				`sinkGroups[${index}].dbConnectionName`
			),
			mode: mode as SinkGroupMode,
			intervalMs: expectInteger(item.intervalMs, `sinkGroups[${index}].intervalMs`),
			tableName: expectString(item.tableName, `sinkGroups[${index}].tableName`),
			storeBad: expectBoolean(item.storeBad, `sinkGroups[${index}].storeBad`),
			enabled: expectBoolean(item.enabled, `sinkGroups[${index}].enabled`),
			tagNames: tagNamesRaw.map((value, tagIndex) =>
				expectString(value, `sinkGroups[${index}].tagNames[${tagIndex}]`)
			)
		};
	});
}

function parseMqtt(raw: unknown): ConfigPackageMqttSettings {
	const item = expectRecord(raw, 'mqtt');
	const enabled = expectBoolean(item.enabled, 'mqtt.enabled');
	const host = expectString(item.host, 'mqtt.host');
	const port = expectInteger(item.port, 'mqtt.port');
	const clientId = expectString(item.clientId, 'mqtt.clientId');
	const prefix = expectString(item.prefix, 'mqtt.prefix');
	const qos = expectInteger(item.qos, 'mqtt.qos');
	const minIntervalMs = expectInteger(item.minIntervalMs, 'mqtt.minIntervalMs');
	if (qos !== 0 && qos !== 1) {
		throw new ConfigPackageParseError('mqtt.qos は 0 または 1 である必要があります');
	}
	return { enabled, host, port, clientId, prefix, qos, minIntervalMs };
}

function parseGrpc(raw: unknown): ConfigPackageGrpcSettings {
	const item = expectRecord(raw, 'grpc');
	return {
		enabled: expectBoolean(item.enabled, 'grpc.enabled'),
		bind: expectString(item.bind, 'grpc.bind'),
		port: expectInteger(item.port, 'grpc.port')
	};
}

function validateReferences(pkg: ConfigPackage): void {
	ensureUniqueNames(pkg.plcConnections, 'plcConnections');
	ensureUniqueNames(pkg.collectionGroups, 'collectionGroups');
	ensureUniqueNames(pkg.tags, 'tags');
	ensureUniqueNames(pkg.sinkGroups, 'sinkGroups');

	const connectionNames = new Set(pkg.plcConnections.map((connection) => connection.name));
	connectionNames.add('calc');
	connectionNames.add('mem');
	for (const group of pkg.collectionGroups) {
		if (!connectionNames.has(group.plcConnectionName)) {
			throw new ConfigPackageParseError(
				`collectionGroups '${group.name}' が参照する connection '${group.plcConnectionName}' が見つかりません`
			);
		}
	}
	const groupNames = new Set(pkg.collectionGroups.map((group) => group.name));
	for (const tag of pkg.tags) {
		if (!groupNames.has(tag.collectionGroupName)) {
			throw new ConfigPackageParseError(
				`tags '${tag.name}' が参照する group '${tag.collectionGroupName}' が見つかりません`
			);
		}
	}

	// S6: sink group が参照する接続・タグ名がパッケージ内で解決できること。
	const tagNames = new Set(pkg.tags.map((tag) => tag.name));
	for (const sinkGroup of pkg.sinkGroups) {
		if (!connectionNames.has(sinkGroup.dbConnectionName)) {
			throw new ConfigPackageParseError(
				`sinkGroups '${sinkGroup.name}' が参照する connection '${sinkGroup.dbConnectionName}' が見つかりません`
			);
		}
		for (const tagName of sinkGroup.tagNames) {
			if (!tagNames.has(tagName)) {
				throw new ConfigPackageParseError(
					`sinkGroups '${sinkGroup.name}' が参照する tag '${tagName}' が見つかりません`
				);
			}
		}
	}
}

export function parseConfigPackage(text: string): ConfigPackage {
	const root = expectRecord(JSON.parse(stripBom(text)), 'root');
	if (root.schemaVersion !== CONFIG_PACKAGE_SCHEMA_VERSION) {
		throw new ConfigPackageParseError(
			`schemaVersion ${String(root.schemaVersion)} は未対応です（期待値: ${CONFIG_PACKAGE_SCHEMA_VERSION}）`
		);
	}
	if (root.product !== CONFIG_PACKAGE_PRODUCT) {
		throw new ConfigPackageParseError(
			`product ${String(root.product)} は banto-hub ではありません`
		);
	}
	const pkg: ConfigPackage = {
		schemaVersion: CONFIG_PACKAGE_SCHEMA_VERSION,
		product: CONFIG_PACKAGE_PRODUCT,
		exportedAt: expectString(root.exportedAt, 'exportedAt'),
		excludedSecrets: Array.isArray(root.excludedSecrets)
			? root.excludedSecrets.map((value, index) => expectString(value, `excludedSecrets[${index}]`))
			: (() => {
					throw new ConfigPackageParseError('excludedSecrets は配列である必要があります');
				})(),
		plcConnections: parsePlcConnections(root.plcConnections),
		collectionGroups: parseCollectionGroups(root.collectionGroups),
		tags: parseTags(root.tags),
		sinkGroups: parseSinkGroups(root.sinkGroups),
		mqtt: parseMqtt(root.mqtt),
		grpc: parseGrpc(root.grpc)
	};
	validateReferences(pkg);
	return pkg;
}

export function planByName<TIncoming extends { name: string }, TExisting extends { name: string }>(
	incoming: readonly TIncoming[],
	existing: readonly TExisting[]
): {
	create: TIncoming[];
	update: Array<{ incoming: TIncoming; existing: TExisting }>;
	missing: TIncoming[];
} {
	const existingByName = new Map(existing.map((item) => [item.name, item]));
	const create: TIncoming[] = [];
	const update: Array<{ incoming: TIncoming; existing: TExisting }> = [];
	for (const item of incoming) {
		const hit = existingByName.get(item.name);
		if (hit) update.push({ incoming: item, existing: hit });
		else create.push(item);
	}
	return { create, update, missing: create };
}
