import {
	type CollectionGroup,
	type CollectionGroupInput,
	type PlcConnection,
	type PlcConnectionInput,
	type PlcProtocol,
	type SlmpWordOrder,
	type Tag,
	type TagInput
} from './tagRegistryAdmin';
import { type GrpcSettings } from './grpcSettingsAdmin';
import { type MqttSettings } from './mqttSettingsAdmin';

export const CONFIG_PACKAGE_SCHEMA_VERSION = 1 as const;
export const CONFIG_PACKAGE_PRODUCT = 'banto-hub' as const;

export const CONFIG_PACKAGE_EXCLUDED_SECRETS = [
	'mqtt.username',
	'mqtt.password',
	'users.password_hash',
	'api_keys.key_hash',
	'sessions',
	'audit_logs',
	'history'
] as const;

export type ConfigPackagePlcConnection = PlcConnectionInput;

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
}

export interface ConfigPackageInspectionCounts {
	plcConnections: { create: number; update: number };
	collectionGroups: { create: number; update: number };
	tags: { create: number; update: number };
}

export interface ConfigPackageInspection {
	counts: ConfigPackageInspectionCounts;
	warnings: string[];
	mqttCredentialsRequired: boolean;
	mqttSettings: ConfigPackageMqttSettings;
	grpcSettings: ConfigPackageGrpcSettings;
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
 */
function expectWordOrder(value: unknown, path: string): SlmpWordOrder {
	if (value === undefined) return 'low_high';
	if (value === 'low_high' || value === 'high_low') return value;
	throw new ConfigPackageParseError(
		`${path} は low_high / high_low のいずれかである必要があります`
	);
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

function sanitizeConnection(input: PlcConnection): ConfigPackagePlcConnection {
	const { name, protocol, host, port, unitId, enabled, simulation, wordOrder } = input;
	return { name, protocol, host, port, unitId, enabled, simulation, wordOrder };
}

function sanitizeGroup(
	input: CollectionGroup,
	connectionName: string
): ConfigPackageCollectionGroup {
	return {
		name: input.name,
		plcConnectionName: connectionName,
		periodMs: input.periodMs,
		enabled: input.enabled
	};
}

function sanitizeTag(input: Tag, groupName: string): ConfigPackageTag {
	return {
		name: input.name,
		collectionGroupName: groupName,
		address: input.address,
		dataType: input.dataType,
		stringLength: input.stringLength,
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

export function buildConfigPackage(input: {
	plcConnections: readonly PlcConnection[];
	collectionGroups: readonly CollectionGroup[];
	tags: readonly Tag[];
	mqtt: MqttSettings;
	grpc: GrpcSettings;
	exportedAt?: string;
}): ConfigPackage {
	const connectionById = new Map(
		input.plcConnections.map((connection) => [connection.id, connection])
	);
	const groupById = new Map(input.collectionGroups.map((group) => [group.id, group]));

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

	return {
		schemaVersion: CONFIG_PACKAGE_SCHEMA_VERSION,
		product: CONFIG_PACKAGE_PRODUCT,
		exportedAt: input.exportedAt ?? new Date().toISOString(),
		excludedSecrets: CONFIG_PACKAGE_EXCLUDED_SECRETS,
		plcConnections: filterVirtualConnections(input.plcConnections).map(sanitizeConnection),
		collectionGroups,
		tags,
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
		if (protocol !== 'modbus-tcp' && protocol !== 'slmp' && protocol !== 'virtual') {
			throw new ConfigPackageParseError(
				`plcConnections[${index}].protocol は modbus-tcp / slmp / virtual のいずれかである必要があります`
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
			wordOrder: expectWordOrder(item.wordOrder, `plcConnections[${index}].wordOrder`)
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
			enabled: expectBoolean(item.enabled, `collectionGroups[${index}].enabled`)
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
