// 型だけの import は `import type` で書く: SvelteKit 3 への移行（tsconfig が
// `$app/tsconfig` を継ぐ形）の後、vitest の変換でも `verbatimModuleSyntax` が
// 効くようになり、`import { type X } from './tagRegistryAdmin'` は空の import と
// して残る - `configPackage.test.ts` で `./setup` が評価され、EventProvider の
// 接続（`localStorage`）が未処理の reject になった。このモジュールは型以外を
// import しない純関数のまま保つ。
import type {
	CollectionGroup,
	CollectionGroupInput,
	CpuType,
	PlcConnection,
	PlcConnectionInput,
	PlcProtocol,
	WordOrder,
	StringEncoding,
	Tag,
	TagInput
} from './tagRegistryAdmin';
import type { GrpcSettings } from './grpcSettingsAdmin';
import type { MqttSettings } from './mqttSettingsAdmin';
import type { SinkGroup, SinkGroupMode } from './sinkGroupsAdmin';

/**
 * 版 2（2026-10-19 オーナー決定）: 収集グループ名が「接続内で一意」になった
 * （banto-tags の migration 0022）ので、名前だけの参照では同じ名前のグループ・
 * タグを区別できない。グループは `(plcConnectionName, name)`、タグは
 * `(plcConnectionName, collectionGroupName, name)` で識別し、sink group の
 * タグ参照も同じ 3 つ組（`ConfigPackageTagRef`）にした。
 *
 * **版 1 のパッケージは読まない**（互換性は持たない - 同日のオーナー決定）。
 * `parseConfigPackage` は版 1 を「未対応の版」として断り、今の banto-hub で
 * 書き出し直すよう案内する。export は常に版 2 を書く。
 *
 * **版 1 の「省略を許す」措置も外した**（2026-10-20 オーナー決定）: 版 1 の間に
 * 後から足した項目（`wordOrder`・`cpuType`・`defaultWritable`・`stringEncoding`・
 * `sinkGroups`）は、版 2 では**必須**で、欠けたパッケージは項目の場所
 * （`plcConnections[0].wordOrder` など）を示して断る。省略時の既定値で
 * 黙って埋めない（取り込み側の挙動が export した側の意図と食い違うのを防ぐ）。
 * 接続の `database`/`username` とグループの `querySql` は postgres 接続でだけ
 * 意味を持つので、省略／`null` を通す（版 1 とは無関係の、項目の性質）。
 *
 * （版 1 の時代の経緯: #533（2026-10-08）でタグのしきい値（`thresholdH`/
 * `thresholdHh`/`thresholdL`/`thresholdLl`）を外した。export は書かず、取り込みは
 * 項目なし・`null` を通し、値付きは拒否する（`rejectTagThresholds`）- 版 2 でも
 * 同じ。）
 */
export const CONFIG_PACKAGE_SCHEMA_VERSION = 2 as const;
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
export type ConfigPackagePlcConnection = Omit<PlcConnectionInput, 'password' | 'cpuType'> & {
	/** 版 2 では必須（REST の作成・更新と違い「省略は既存を保つ」を持たない）。 */
	cpuType: CpuType;
};

export interface ConfigPackageCollectionGroup extends Omit<
	CollectionGroupInput,
	'plcConnectionId'
> {
	plcConnectionName: string;
}

/**
 * 版 2: タグは所属グループの接続名も持つ - グループ名は接続内でしか一意でない
 * （2026-10-19 オーナー決定）ので、`collectionGroupName` だけではどのグループか
 * 決まらない。パッケージ内のタグは `(plcConnectionName, collectionGroupName,
 * name)` で一意（タグ名はグループ内で一意、migration 0011）。
 */
export interface ConfigPackageTag extends Omit<TagInput, 'collectionGroupId' | 'expectedRevision'> {
	plcConnectionName: string;
	collectionGroupName: string;
}

/**
 * 版 2: sink group などがタグを指す参照。外部名（`接続.グループ.タグ`）の文字列に
 * しないのは、名前に `.` を含められて文字列としては曖昧になりうるため（#635）。
 */
export interface ConfigPackageTagRef {
	plcConnectionName: string;
	collectionGroupName: string;
	tagName: string;
}

/**
 * パッケージ内・現在の環境の両方で、グループ・タグを照合するキー。名前に
 * どんな文字が入っても区切りが曖昧にならないよう JSON の配列にする。
 */
export function configPackageGroupKey(plcConnectionName: string, groupName: string): string {
	return JSON.stringify([plcConnectionName, groupName]);
}

export function configPackageTagKey(
	plcConnectionName: string,
	groupName: string,
	tagName: string
): string {
	return JSON.stringify([plcConnectionName, groupName, tagName]);
}

/** エラー・警告の文言用（`接続 / グループ / タグ`）。 */
export function describeConfigPackageTagRef(ref: ConfigPackageTagRef): string {
	return `${ref.plcConnectionName} / ${ref.collectionGroupName} / ${ref.tagName}`;
}

export type ConfigPackageMqttSettings = Omit<MqttSettings, 'username'>;
export type ConfigPackageGrpcSettings = GrpcSettings;

/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6、実装指示6）: DB
 * Sink group 1件分 - `SinkGroup`から`id`/`dbConnectionId`/`tagIds`
 * （数値 id、環境をまたぐと再現できない）を除き、代わりに**名前で**参照する
 * （`ConfigPackageCollectionGroup::plcConnectionName`と同じ考え方 - タグの
 * 参照は`ConfigPackageTag`と同じ `接続名・グループ名・タグ名` の 3 つ組
 * （版 2、`ConfigPackageTagRef`）をキーにすることで、タグ id が環境間で
 * 異なっても import 側で解決できる）。
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
	/** 版 2: 名前だけの `tagNames` から 3 つ組の参照へ（`ConfigPackageTagRef`）。 */
	tags: ConfigPackageTagRef[];
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
	 * S6: DB Sink group。版 2 では必須（sink group が無い環境の export も
	 * 空配列を書く）。項目が無いパッケージは `parseSinkGroups` が断る。
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
 * `cpuType`（#605）は版 2 では必須で、`Q` / `R` のいずれか。省略・`null` は
 * 断る（REST の作成・更新は省略を許し「更新は既存の値を保つ」が、パッケージは
 * 常に export が全項目を書くので、ここで既定の `R` を入れると既存の Q 接続を
 * R に戻してしまう）。
 */
function expectCpuType(value: unknown, path: string): CpuType {
	if (value === 'Q' || value === 'R') return value;
	throw new ConfigPackageParseError(`${path} は Q / R のいずれかである必要があります`);
}

/**
 * P3-b（監査指摘 2026-08-12）: `wordOrder` は版 2 では必須で、`low_high` /
 * `high_low` のいずれか。省略は断る（版 1 の間は省略時にプロトコル依存の既定
 * 値を入れていたが、2026-10-20 のオーナー決定で外した。export は常に書く）。
 *
 * この関数が返す型名は元 `SlmpWordOrder` から `WordOrder` に改名した
 * （SLMP専用ではなくなったため）。
 */
function expectWordOrder(value: unknown, path: string): WordOrder {
	if (value === 'low_high' || value === 'high_low') return value;
	throw new ConfigPackageParseError(
		`${path} は low_high / high_low のいずれかである必要があります`
	);
}

/**
 * `stringEncoding` は版 2 では必須で、`utf8` / `shift_jis` のいずれか
 * （省略・`null` は断る。export は常に書く）。
 */
function expectStringEncoding(value: unknown, path: string): StringEncoding {
	if (value === 'utf8' || value === 'shift_jis') return value;
	throw new ConfigPackageParseError(`${path} は utf8 または shift_jis のいずれかです`);
}

/**
 * T19 S1-b（UX-34、2026-09-02 オーナー決定）: `defaultWritable` は版 2 では
 * 必須の真偽値（省略は断る）。
 */
function expectDefaultWritable(value: unknown, path: string): boolean {
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
 * （`database`/`username`は postgres 接続でだけ意味を持つ項目で、
 * 省略を許す - `expectOptionalString`参照）。`password`はそもそも入力に存在しない（`PlcConnection`型に
 * フィールド自体が無い）ため、詰め忘れようがない。
 */
function sanitizeConnection(input: PlcConnection): ConfigPackagePlcConnection {
	const {
		name,
		protocol,
		host,
		port,
		unitId,
		enabled,
		simulation,
		wordOrder,
		cpuType,
		database,
		username
	} = input;
	return {
		name,
		protocol,
		host,
		port,
		unitId,
		enabled,
		simulation,
		wordOrder,
		cpuType,
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

function sanitizeTag(input: Tag, connectionName: string, groupName: string): ConfigPackageTag {
	return {
		name: input.name,
		plcConnectionName: connectionName,
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
 * 受け取るのと同じ形。`tagIds`は`tagRefById`で 3 つ組の参照へ変換し、解決できない
 * id（`hub_sink_group_tags`の孤児許容 - `crate::sink`のモジュール doc「FK を
 * 張らない理由」参照。タグ削除時は能動的にクリーンアップされるため、
 * export 時点でこの分岐へ入ることは通常無い）は黙って読み飛ばす -
 * `build_sink_config_response`（Hub側）と同じ「読み飛ばす」方針。
 */
function sanitizeSinkGroup(
	input: SinkGroup,
	dbConnectionName: string,
	tagRefById: ReadonlyMap<number, ConfigPackageTagRef>
): ConfigPackageSinkGroup {
	return {
		name: input.name,
		dbConnectionName,
		mode: input.mode,
		intervalMs: input.intervalMs,
		tableName: input.tableName,
		storeBad: input.storeBad,
		enabled: input.enabled,
		tags: input.tagIds
			.map((tagId) => tagRefById.get(tagId))
			.filter((ref): ref is ConfigPackageTagRef => ref !== undefined)
	};
}

export function buildConfigPackage(input: {
	plcConnections: readonly PlcConnection[];
	collectionGroups: readonly CollectionGroup[];
	tags: readonly Tag[];
	mqtt: MqttSettings;
	grpc: GrpcSettings;
	/**
	 * S6（実装指示6）: 省略時は`[]`（sink group を持たない環境でも、
	 * export は`sinkGroups: []`を必ず書く - 版 2 では項目が必須）。
	 */
	sinkGroups?: readonly SinkGroup[];
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

	const tagRefById = new Map<number, ConfigPackageTagRef>();
	const tags = input.tags.map((tag) => {
		const group = groupById.get(tag.collectionGroupId);
		if (!group) {
			throw new ConfigPackageParseError(
				`tag '${tag.name}' の collectionGroupId=${tag.collectionGroupId} に対応する group が見つかりません`
			);
		}
		const connection = connectionById.get(group.plcConnectionId);
		if (!connection) {
			throw new ConfigPackageParseError(
				`tag '${tag.name}' のグループ '${group.name}' の接続 id=${group.plcConnectionId} に対応する connection が見つかりません`
			);
		}
		tagRefById.set(tag.id, {
			plcConnectionName: connection.name,
			collectionGroupName: group.name,
			tagName: tag.name
		});
		return sanitizeTag(tag, connection.name, group.name);
	});

	const sinkGroups = (input.sinkGroups ?? []).map((group) => {
		const connection = connectionById.get(group.dbConnectionId);
		if (!connection) {
			throw new ConfigPackageParseError(
				`sink group '${group.name}' の接続 id=${group.dbConnectionId} に対応する connection が見つかりません`
			);
		}
		return sanitizeSinkGroup(group, connection.name, tagRefById);
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
 * S1（docs/banto-hub-external-db-design.md §4.1）: `database`/`username`と
 * （S3）`querySql`は postgres 接続（配下のグループ）でだけ意味を持ち、サーバーは
 * 他のプロトコルでの指定を拒否する。このため版 2 でも省略／`null` を通し、
 * `undefined`にする（版 1 の互換とは無関係の、項目の性質）。値が存在する場合は
 * 文字列であることを検証する（postgres での必須チェックは`plcConnectionForm.ts
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
			wordOrder: expectWordOrder(item.wordOrder, `plcConnections[${index}].wordOrder`),
			cpuType: expectCpuType(item.cpuType, `plcConnections[${index}].cpuType`),
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
			// S3: `querySql`は postgres 接続配下のグループでだけ意味を持つので、
			// `database`/`username`（S1、`expectOptionalString`）と同じく省略／
			// `null`を通す（必須チェックはサーバー側に委ねる）。
			querySql: expectOptionalString(item.querySql, `collectionGroups[${index}].querySql`)
		};
	});
}

/**
 * #533（2026-10-08 オーナー決定「しきい値は使う側（記録計・SCADA）が持つ設定で、
 * Hub は持たない」）: タグのしきい値の 4 項目。export はもう書かない。
 */
const REMOVED_TAG_THRESHOLD_KEYS = [
	'thresholdH',
	'thresholdHh',
	'thresholdL',
	'thresholdLl'
] as const;

/**
 * #533: しきい値を**値付きで**持つ旧パッケージ（#533 より前の export）は、
 * 黙って捨てずに取り込みを断る（REST の `TagPayload::reject_thresholds` と
 * 同じ考え方 - 捨てると、取り込んだ側は警報が設定できたと思い込む）。
 * 項目が無い・`null`（#533 より前の export で、しきい値を設定していなかった
 * タグ）は通す - 版 2 でも同じ（項目を外しただけで、しきい値を設定していなかった
 * タグはそのまま取り込める）。
 */
function rejectTagThresholds(item: Record<string, unknown>, index: number): void {
	for (const key of REMOVED_TAG_THRESHOLD_KEYS) {
		const value = item[key];
		if (value !== undefined && value !== null) {
			throw new ConfigPackageParseError(
				`tags[${index}].${key} にしきい値が入っています。banto-hub はしきい値を持ちません（記録計・SCADA の側で設定します）。パッケージからしきい値を消してから取り込んでください`
			);
		}
	}
}

function parseTags(raw: unknown): ConfigPackageTag[] {
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('tags は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const item = expectRecord(entry, `tags[${index}]`);
		rejectTagThresholds(item, index);
		return {
			name: expectString(item.name, `tags[${index}].name`),
			plcConnectionName: expectString(item.plcConnectionName, `tags[${index}].plcConnectionName`),
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
			enabled: expectBoolean(item.enabled, `tags[${index}].enabled`),
			writable: expectBoolean(item.writable, `tags[${index}].writable`),
			tagKind: expectString(item.tagKind, `tags[${index}].tagKind`) as TagInput['tagKind'],
			retain: expectBoolean(item.retain, `tags[${index}].retain`)
		};
	});
}

/**
 * S6（実装指示6）: `sinkGroups`は版 2 では必須の配列（sink group が無くても
 * 空配列）。項目が無いパッケージは断る。
 */
function parseSinkGroups(raw: unknown): ConfigPackageSinkGroup[] {
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
		const tagsRaw = item.tags;
		if (!Array.isArray(tagsRaw)) {
			throw new ConfigPackageParseError(`sinkGroups[${index}].tags は配列である必要があります`);
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
			tags: tagsRaw.map((value, tagIndex) => {
				const path = `sinkGroups[${index}].tags[${tagIndex}]`;
				const ref = expectRecord(value, path);
				return {
					plcConnectionName: expectString(ref.plcConnectionName, `${path}.plcConnectionName`),
					collectionGroupName: expectString(ref.collectionGroupName, `${path}.collectionGroupName`),
					tagName: expectString(ref.tagName, `${path}.tagName`)
				};
			})
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

/**
 * 版 2: キーで重複を断る。`describe` は文言で重複した組を示す。
 */
function ensureUniqueKeys<T>(
	items: readonly T[],
	path: string,
	key: (item: T) => string,
	describe: (item: T) => string
): void {
	const seen = new Set<string>();
	items.forEach((item, index) => {
		const k = key(item);
		if (seen.has(k)) {
			throw new ConfigPackageParseError(
				`${path}[${index}] は ${describe(item)} と重複しています（${path} の中で同じものは 1 つだけです）`
			);
		}
		seen.add(k);
	});
}

function validateReferences(pkg: ConfigPackage): void {
	ensureUniqueNames(pkg.plcConnections, 'plcConnections');
	// 2026-10-19 オーナー決定: グループ名は接続内で一意、タグ名はグループ内で一意。
	ensureUniqueKeys(
		pkg.collectionGroups,
		'collectionGroups',
		(group) => configPackageGroupKey(group.plcConnectionName, group.name),
		(group) => `接続 '${group.plcConnectionName}' のグループ '${group.name}'`
	);
	ensureUniqueKeys(
		pkg.tags,
		'tags',
		(tag) => configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name),
		(tag) =>
			`接続 '${tag.plcConnectionName}' のグループ '${tag.collectionGroupName}' のタグ '${tag.name}'`
	);
	ensureUniqueNames(pkg.sinkGroups, 'sinkGroups');

	const connectionNames = new Set(pkg.plcConnections.map((connection) => connection.name));
	connectionNames.add('calc');
	connectionNames.add('mem');
	pkg.collectionGroups.forEach((group, index) => {
		if (!connectionNames.has(group.plcConnectionName)) {
			throw new ConfigPackageParseError(
				`collectionGroups[${index}].plcConnectionName: グループ '${group.name}' が参照する connection '${group.plcConnectionName}' が見つかりません`
			);
		}
	});
	const groupKeys = new Set(
		pkg.collectionGroups.map((group) => configPackageGroupKey(group.plcConnectionName, group.name))
	);
	pkg.tags.forEach((tag, index) => {
		if (!groupKeys.has(configPackageGroupKey(tag.plcConnectionName, tag.collectionGroupName))) {
			throw new ConfigPackageParseError(
				`tags[${index}].collectionGroupName: タグ '${tag.name}' が参照する接続 '${tag.plcConnectionName}' のグループ '${tag.collectionGroupName}' が見つかりません`
			);
		}
	});

	// S6: sink group が参照する接続・タグがパッケージ内で解決できること。
	const tagKeys = new Set(
		pkg.tags.map((tag) =>
			configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name)
		)
	);
	pkg.sinkGroups.forEach((sinkGroup, index) => {
		if (!connectionNames.has(sinkGroup.dbConnectionName)) {
			throw new ConfigPackageParseError(
				`sinkGroups[${index}].dbConnectionName: sink group '${sinkGroup.name}' が参照する connection '${sinkGroup.dbConnectionName}' が見つかりません`
			);
		}
		sinkGroup.tags.forEach((ref, tagIndex) => {
			if (
				!tagKeys.has(
					configPackageTagKey(ref.plcConnectionName, ref.collectionGroupName, ref.tagName)
				)
			) {
				throw new ConfigPackageParseError(
					`sinkGroups[${index}].tags[${tagIndex}]: sink group '${sinkGroup.name}' が参照する tag '${describeConfigPackageTagRef(ref)}' が見つかりません`
				);
			}
		});
	});
}

export function parseConfigPackage(text: string): ConfigPackage {
	const root = expectRecord(JSON.parse(stripBom(text)), 'root');
	if (root.schemaVersion !== CONFIG_PACKAGE_SCHEMA_VERSION) {
		// 版 1（2026-10-19 より前の export）は読まない - 互換性は持たない
		// （オーナー決定、`CONFIG_PACKAGE_SCHEMA_VERSION` の doc）。
		throw new ConfigPackageParseError(
			`schemaVersion ${String(root.schemaVersion)} は未対応です（期待値: ${CONFIG_PACKAGE_SCHEMA_VERSION}）。古い版の構成パッケージは読み込めません。今の banto-hub で書き出し直してください`
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

export interface ConfigPackagePlan<TIncoming, TExisting> {
	create: TIncoming[];
	update: Array<{ incoming: TIncoming; existing: TExisting }>;
	missing: TIncoming[];
}

/**
 * 版 2: キーで突き合わせる（グループは `configPackageGroupKey`、タグは
 * `configPackageTagKey`）。`planByName` はキーが `name` の場合。
 */
export function planByKey<TIncoming, TExisting>(
	incoming: readonly TIncoming[],
	existing: readonly TExisting[],
	incomingKey: (item: TIncoming) => string,
	existingKey: (item: TExisting) => string | undefined
): ConfigPackagePlan<TIncoming, TExisting> {
	const existingByKey = new Map<string, TExisting>();
	for (const item of existing) {
		const key = existingKey(item);
		if (key !== undefined) existingByKey.set(key, item);
	}
	const create: TIncoming[] = [];
	const update: Array<{ incoming: TIncoming; existing: TExisting }> = [];
	for (const item of incoming) {
		const hit = existingByKey.get(incomingKey(item));
		if (hit) update.push({ incoming: item, existing: hit });
		else create.push(item);
	}
	return { create, update, missing: create };
}

export function planByName<TIncoming extends { name: string }, TExisting extends { name: string }>(
	incoming: readonly TIncoming[],
	existing: readonly TExisting[]
): ConfigPackagePlan<TIncoming, TExisting> {
	return planByKey(
		incoming,
		existing,
		(item) => item.name,
		(item) => item.name
	);
}
