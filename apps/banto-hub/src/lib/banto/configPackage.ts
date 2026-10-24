// 型だけの import は `import type` で書く: SvelteKit 3 への移行（tsconfig が
// `$app/tsconfig` を継ぐ形）の後、vitest の変換でも `verbatimModuleSyntax` が
// 効くようになり、`import { type X } from './tagRegistryAdmin'` は空の import と
// して残る - `configPackage.test.ts` で `./setup` が評価され、EventProvider の
// 接続（`localStorage`）が未処理の reject になった。このモジュールは型以外を
// import しない純関数のまま保つ。
import type {
	CollectionGroup,
	CollectionGroupInput,
	DriverInfo,
	PlcConnection,
	PushBridgeQueryWire,
	PushSettingsWire,
	StringEncoding,
	Tag,
	TagInput
} from './tagRegistryAdmin';
import type { GrpcSettings } from './grpcSettingsAdmin';
import type { MqttSettings } from './mqttSettingsAdmin';
import type { SinkGroup, SinkGroupMode } from './sinkGroupsAdmin';

/**
 * 版 3（R2b-6b-3、docs/banto-hub-driver-ui-design.md §5・§7.1、Q11 (a)）: 接続を REST と
 * 同じ `{ name, protocol, enabled, simulation, settings }` で持つ（版 2 の平らな `host`・
 * `port`・`unitId`・`wordOrder`・`cpuType`・`database`・`username` は無くなった）。
 * `settings` の中はドライバ・ホストの接続の JSON のまま（snake_case のキー）。
 *
 * - **秘密のキーは載せない**: 書き出しは応答の `{"$secret": …}` の値を落とす。読み込みは
 *   `$secret` の形の値と、スキーマで `Secret` の欄（`postgres` の `password`）を断る。
 * - **push のブリッジの読む DB は接続名で持ち運ぶ**（版 2 と同じ。`settings.bridge.
 *   dbConnectionName`）。`settings` をそのまま写すと、接続 id の違う別の Hub で関係のない
 *   PostgreSQL の接続を読むため（設計 §5）。
 * - **欠けた欄は既定で埋めずに断る**（版 2 の `wordOrder`・`cpuType` と同じ考え方、§7.1 の
 *   表の最後の行）: 書き出しが必ず書く欄（`Secret` 以外で、既定値を持つ欄と必須の欄）が
 *   欠けた接続は断る。欄はドライバのスキーマ（`GET /api/drivers`）とホストの接続の表
 *   （{@link HOST_CONNECTION_FIELDS}）から決めるので、読み込み（`parseConfigPackage`）の後に
 *   {@link checkConfigPackageConnectionFields} で確かめる（`configPackageAdmin.ts` が呼ぶ）。
 * - 版 1・2 のパッケージは読まない（使用者無しの前提、2026-10-22 オーナー）。
 *
 * 以下は版 2 の記録（グループ・タグの参照の形は版 3 でも同じ）。
 *
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
export const CONFIG_PACKAGE_SCHEMA_VERSION = 3 as const;
export const CONFIG_PACKAGE_PRODUCT = 'banto-hub' as const;

export const CONFIG_PACKAGE_EXCLUDED_SECRETS = [
	'mqtt.username',
	'mqtt.password',
	'users.password_hash',
	'api_keys.key_hash',
	'sessions',
	'audit_logs',
	'history',
	// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: 接続の秘密のキー（`postgres` の
	// パスワードなど、版 3 では `settings` の `{"$secret": …}` の値）- `mqtt.password`と同じ
	// 理由で export に含めない（`sanitizeConnection`が実際に落とす。ここでの列挙は
	// 「除外した」ことを export 自体の中に明記するための一覧で、実際の除外動作を担うわけでは
	// ない - `mqtt.password`も同じ関係）。
	'plc_connections.settings.$secret'
] as const;

/**
 * 版 3: パッケージの接続 1 つ（REST の `PlcConnectionInput` と同じ形。id は持たない）。
 * `settings` は秘密のキーを持たない（{@link CONFIG_PACKAGE_SCHEMA_VERSION} の doc）。
 * `push` の `settings` は {@link ConfigPackagePushSettings}（ブリッジの読む DB は接続名）。
 */
export interface ConfigPackagePlcConnection {
	name: string;
	/** ドライバの id（`modbus-tcp`・`slmp` …）またはホストの接続（`postgres`・`push`）。 */
	protocol: string;
	enabled: boolean;
	simulation: boolean;
	settings: Record<string, unknown>;
}

/** R2a-2e: パッケージの中の `settings.bridge`（`db_connection_id` の代わりに接続名）。 */
export interface ConfigPackagePushBridge {
	kind: 'db';
	dbConnectionName: string;
	queries: PushBridgeQueryWire[];
}

/**
 * R2a-2e: パッケージの中の push 接続の `settings`（§3.2 の形。ブリッジは名前で参照）。
 * `ConfigPackagePlcConnection.settings` に入れるので `type` で書く（`interface` は
 * `Record<string, unknown>` に代入できない）。
 */
export type ConfigPackagePushSettings = {
	stale_timeout_ms?: number;
	bridge?: ConfigPackagePushBridge;
};

/** 版 3: push 接続ならパッケージの `settings` のブリッジ（無ければ `undefined`）。 */
export function configPackagePushBridge(
	connection: Pick<ConfigPackagePlcConnection, 'protocol' | 'settings'>
): ConfigPackagePushBridge | undefined {
	if (connection.protocol !== 'push') return undefined;
	return (connection.settings as ConfigPackagePushSettings).bridge;
}

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
	/**
	 * R2a-2e（docs/banto-hub-ingest-design.md §5.1）: このパッケージが含む `push`
	 * 接続の名前。設定パッケージは API キーを持ち運ばず、取り込み先では接続の id
	 * が変わる（送り手の URL `/api/v1/ingest/{id}/…` とスコープ `ingest:#{id}` も
	 * 変わる）ので、画面は「取り込みのキーは移りません」と案内する。
	 */
	ingestConnections: string[];
}

/** R2a-2e: 取り込んだ `push` 接続 1 つ（取り込み先での id、§5.1）。 */
export interface ConfigPackageIngestConnection {
	name: string;
	id: number;
}

export interface ConfigPackageImportSummary {
	counts: ConfigPackageInspectionCounts;
	mqttApplied: boolean;
	grpcApplied: boolean;
	warnings: string[];
	/**
	 * R2a-2e（§5.1）: 取り込んだ `push` 接続と取り込み先での id。キーは移らない
	 * ので、結果の画面はこの id で送り手の URL とスコープを直すよう案内する。
	 */
	ingestConnections: ConfigPackageIngestConnection[];
}

/** R2a-2e（§5.1）: 取り込みの結果の画面・確認の画面に出す案内の見出し。 */
export const INGEST_KEYS_NOT_MIGRATED_MESSAGE = '取り込みのキーは移りません';

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

function isVirtualConnection(connection: { protocol: string }): boolean {
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

/** 応答・パッケージの秘密のキーの値の形（`{"$secret": …}`。中身は問わない）。 */
function isSecretShaped(value: unknown): boolean {
	return isRecord(value) && !Array.isArray(value) && Object.hasOwn(value, '$secret');
}

/**
 * 版 3: 応答（`PlcConnection`）から export の形へ。秘密のキーの値
 * （`{"$secret": "set" | "unreadable"}`）は落とす - 値そのものはサーバーが返さないので、
 * 載るのは状態だけだが、それも持ち運ばない（取り込み先では意味が無い）。push の
 * `settings` はブリッジの `db_connection_id` を接続名に写す（{@link sanitizePushSettings}）。
 */
function sanitizeConnection(
	input: PlcConnection,
	connectionNameById: ReadonlyMap<number, string>
): ConfigPackagePlcConnection {
	const settings =
		input.protocol === 'push'
			? sanitizePushSettings(
					input.name,
					(input.settings ?? {}) as PushSettingsWire,
					connectionNameById
				)
			: Object.fromEntries(
					Object.entries(input.settings ?? {}).filter(([, value]) => !isSecretShaped(value))
				);
	return {
		name: input.name,
		protocol: input.protocol,
		enabled: input.enabled,
		simulation: input.simulation,
		settings
	};
}

/**
 * R2a-2e: push 接続の `settings` を export の形へ。ブリッジの `db_connection_id` を
 * 接続名に写す（§3.1「`dbConnectionName` に写して持ち運ぶ」）。名前を引けない
 * （参照先が一覧に無い）なら、壊れた参照を書き出さずに断る（sink group の
 * 接続が引けないときと同じ）。秘密値は無い（DB のパスワードは参照先の
 * `postgres` 接続の列で、`plc_connections.password` として除外済み）。
 */
function sanitizePushSettings(
	connectionName: string,
	settings: PushSettingsWire | null,
	connectionNameById: ReadonlyMap<number, string>
): ConfigPackagePushSettings {
	const out: ConfigPackagePushSettings = {};
	if (settings?.stale_timeout_ms !== undefined) out.stale_timeout_ms = settings.stale_timeout_ms;
	const bridge = settings?.bridge;
	if (bridge) {
		const dbConnectionName = connectionNameById.get(bridge.db_connection_id);
		if (dbConnectionName === undefined) {
			throw new ConfigPackageParseError(
				`push 接続 '${connectionName}' のブリッジが読む接続 id=${bridge.db_connection_id} に対応する connection が見つかりません`
			);
		}
		out.bridge = {
			kind: 'db',
			dbConnectionName,
			queries: bridge.queries.map((query) => ({
				name: query.name,
				period_ms: query.period_ms,
				sql: query.sql,
				columns: query.columns.map((column) => ({ column: column.column, key: column.key }))
			}))
		};
	}
	return out;
}

/**
 * R2a-2e: パッケージの中の push 接続の `settings` を、取り込み先の接続 id で
 * REST の `settings` に戻す（`sanitizePushSettings` の逆）。`resolveId` が
 * 名前を引けなければ `null`（呼び出し側が警告を積んでその接続を飛ばす）。
 */
export function resolvePushSettings(
	settings: ConfigPackagePushSettings,
	resolveId: (connectionName: string) => number | undefined
): PushSettingsWire | null {
	const out: PushSettingsWire = {};
	if (settings.stale_timeout_ms !== undefined) out.stale_timeout_ms = settings.stale_timeout_ms;
	if (settings.bridge) {
		const id = resolveId(settings.bridge.dbConnectionName);
		if (id === undefined) return null;
		out.bridge = {
			kind: 'db',
			db_connection_id: id,
			queries: settings.bridge.queries
		};
	}
	return out;
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
	const connectionNameById = new Map(
		input.plcConnections.map((connection) => [connection.id, connection.name])
	);

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
		plcConnections: filterVirtualConnections(input.plcConnections).map((connection) =>
			sanitizeConnection(connection, connectionNameById)
		),
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
 * S3（docs/banto-hub-external-db-design.md §4.2）: グループの `querySql` は postgres 接続の
 * 配下でだけ意味を持ち、サーバーは他のプロトコルでの指定を拒否する。このため省略／`null` を
 * 通し、`undefined`にする（項目の性質）。値が存在する場合は文字列であることを検証する
 * （必須の判定はサーバー側のバリデーションに委ねる - configPackage のパースは「形として
 * 妥当か」だけを見る。他の`ConfigPackage*`パーサーも同じ方針）。版 2 まで接続の
 * `database`/`username` にも使っていたが、版 3 で接続の `settings` に移った。
 */
function expectOptionalString(value: unknown, path: string): string | undefined {
	if (value === undefined || value === null) return undefined;
	return expectString(value, path);
}

/**
 * 版 3: 接続の形だけを確かめる（`name`・`protocol`・`enabled`・`simulation` と、オブジェクトの
 * `settings` は必須）。欄の有無はスキーマで決まるので、ここでは見ない
 * （{@link checkConfigPackageConnectionFields}）。値の規則（範囲・選択肢）は取り込みの作成・
 * 更新でサーバーが確かめる（他のパーサーと同じ「形として妥当か」だけの方針）。
 *
 * 秘密の値は断る: `$secret` の形の値は、書き出しが落とすので手で書いたパッケージだけに
 * 現れる（黙って捨てると、取り込んだ側は秘密値が移ったと思い込む）。
 */
function parsePlcConnections(raw: unknown): ConfigPackagePlcConnection[] {
	if (!Array.isArray(raw)) {
		throw new ConfigPackageParseError('plcConnections は配列である必要があります');
	}
	return raw.map((entry, index) => {
		const path = `plcConnections[${index}]`;
		const item = expectRecord(entry, path);
		const protocol = expectString(item.protocol, `${path}.protocol`);
		if (protocol === '') {
			throw new ConfigPackageParseError(`${path}.protocol は空にできません`);
		}
		const rawSettings = item.settings;
		if (!isRecord(rawSettings) || Array.isArray(rawSettings)) {
			throw new ConfigPackageParseError(`${path}.settings はオブジェクトである必要があります`);
		}
		let settings: Record<string, unknown>;
		if (protocol === 'push') {
			settings = parsePushSettings(rawSettings, `${path}.settings`);
		} else {
			for (const [key, value] of Object.entries(rawSettings)) {
				if (isSecretShaped(value)) {
					throw new ConfigPackageParseError(
						`${path}.settings.${key} は秘密の値です。構成パッケージは秘密の値を持ちません（取り込んだ後に画面で設定してください）`
					);
				}
			}
			settings = { ...rawSettings };
		}
		return {
			name: expectString(item.name, `${path}.name`),
			protocol,
			enabled: expectBoolean(item.enabled, `${path}.enabled`),
			simulation: expectBoolean(item.simulation, `${path}.simulation`),
			settings
		};
	});
}

/**
 * 版 3: パッケージの欄の規則 1 つ（{@link checkConfigPackageConnectionFields}）。
 * `mustWrite` = 書き出しが必ず書く欄（`Secret` 以外で、既定値を持つ欄と必須の欄）。
 */
export interface ConfigPackageFieldRule {
	key: string;
	mustWrite: boolean;
	secret: boolean;
}

/**
 * 版 3: ホストの接続（`GET /api/drivers` に載らない）の `settings` の欄 - mirrors
 * `banto_tags::plc_connection::settings_input`（`postgres` は `host`・`port`・`database`・
 * `username` を受け、`legacy::settings_from_flat` が `host`・`port` を必ず書き、検証が
 * `database`・`username` を必須にする。`password` は秘密のキー）と
 * `banto_tags::push::validate_push_settings`（`stale_timeout_ms`・`bridge` はどちらも任意で
 * 既定の値を書かない。push の既定は `{}`）。`virtual` は書き出さないので表に無い。
 */
export const HOST_CONNECTION_FIELDS: Readonly<Record<string, readonly ConfigPackageFieldRule[]>> = {
	postgres: [
		{ key: 'host', mustWrite: true, secret: false },
		{ key: 'port', mustWrite: true, secret: false },
		{ key: 'database', mustWrite: true, secret: false },
		{ key: 'username', mustWrite: true, secret: false },
		{ key: 'password', mustWrite: false, secret: true }
	],
	push: [
		{ key: 'stale_timeout_ms', mustWrite: false, secret: false },
		{ key: 'bridge', mustWrite: false, secret: false }
	]
};

function driverFieldRules(driver: DriverInfo): ConfigPackageFieldRule[] {
	return driver.settingsSchema.fields.map((field) => {
		const secret = field.kind.type === 'secret';
		return {
			key: field.key,
			secret,
			mustWrite: !secret && (field.required || (field.default ?? null) !== null)
		};
	});
}

/**
 * 版 3（設計 §7.1 の表の最後の行）: パッケージの接続の `settings` を、取り込み先の
 * ドライバのスキーマ（`drivers` = `GET /api/drivers`）とホストの接続の表
 * （{@link HOST_CONNECTION_FIELDS}）で確かめる。**既定で埋めない**（サーバーの
 * `settings_from_schema` の既定の補完を当てにすると、書き出した側の値と食い違っても
 * 気づけない）。
 *
 * - 書き出しが必ず書く欄（`mustWrite`）が無い・`null` なら断る（欠けは手で書き換えた
 *   パッケージだけ）。
 * - `Secret` の欄があれば断る（パッケージは秘密値を持たない）。
 * - `Secret` の欄と既定値の無い任意の欄は欠けてよい。表に無いキーはサーバーに任せる
 *   （未知のキーはサーバーが断る）。
 * - `virtual` は見ない（書き出さない。取り込みは警告を積んで飛ばす）。ドライバにも表にも
 *   無いプロトコルは断る。
 */
export function checkConfigPackageConnectionFields(
	pkg: Pick<ConfigPackage, 'plcConnections'>,
	drivers: readonly DriverInfo[]
): void {
	const driverById = new Map(drivers.map((driver) => [driver.id, driver]));
	pkg.plcConnections.forEach((connection, index) => {
		const path = `plcConnections[${index}]`;
		if (isVirtualConnection(connection)) return;
		const driver = driverById.get(connection.protocol);
		const rules = driver
			? driverFieldRules(driver)
			: Object.hasOwn(HOST_CONNECTION_FIELDS, connection.protocol)
				? HOST_CONNECTION_FIELDS[connection.protocol]
				: undefined;
		if (!rules) {
			const known = [...driverById.keys(), ...Object.keys(HOST_CONNECTION_FIELDS)].join(' / ');
			throw new ConfigPackageParseError(
				`${path}.protocol '${connection.protocol}' はこの banto-hub にありません（${known} のいずれかである必要があります）`
			);
		}
		for (const rule of rules) {
			const value = connection.settings[rule.key];
			const present = value !== undefined && value !== null;
			if (rule.secret) {
				// 値ではなくキーの有無で断る。REST は null・空文字を「消す」と読むので、
				// `"password": null` を通すと取り込み先の保存済みの秘密が消える。
				if (Object.hasOwn(connection.settings, rule.key)) {
					throw new ConfigPackageParseError(
						`${path}.settings.${rule.key} は秘密の欄です。構成パッケージは秘密の値を持ちません（取り込んだ後に画面で設定してください）`
					);
				}
				continue;
			}
			if (rule.mustWrite && !present) {
				throw new ConfigPackageParseError(
					`${path}.settings.${rule.key} がありません（接続 '${connection.name}'。既定値では埋めません。今の banto-hub で書き出し直してください）`
				);
			}
		}
	});
}

/**
 * R2a-2e: パッケージの push 接続の `settings` の形を確かめる（中身の規則 -
 * 範囲・SQL・見積もり - は取り込みの作成・更新でサーバーが確かめる。他の
 * パーサーと同じ「形として妥当か」だけの方針）。
 */
function parsePushSettings(raw: unknown, path: string): ConfigPackagePushSettings {
	const item = expectRecord(raw, path);
	const out: ConfigPackagePushSettings = {};
	if (item.stale_timeout_ms !== undefined) {
		out.stale_timeout_ms = expectInteger(item.stale_timeout_ms, `${path}.stale_timeout_ms`);
	}
	if (item.bridge !== undefined) {
		const bridge = expectRecord(item.bridge, `${path}.bridge`);
		if (bridge.kind !== 'db') {
			throw new ConfigPackageParseError(`${path}.bridge.kind は db である必要があります`);
		}
		const queries = bridge.queries;
		if (!Array.isArray(queries)) {
			throw new ConfigPackageParseError(`${path}.bridge.queries は配列である必要があります`);
		}
		out.bridge = {
			kind: 'db',
			dbConnectionName: expectString(bridge.dbConnectionName, `${path}.bridge.dbConnectionName`),
			queries: queries.map((entry, qIndex) => {
				const qPath = `${path}.bridge.queries[${qIndex}]`;
				const query = expectRecord(entry, qPath);
				const columns = query.columns;
				if (!Array.isArray(columns)) {
					throw new ConfigPackageParseError(`${qPath}.columns は配列である必要があります`);
				}
				return {
					name: expectString(query.name, `${qPath}.name`),
					period_ms: expectInteger(query.period_ms, `${qPath}.period_ms`),
					sql: expectString(query.sql, `${qPath}.sql`),
					columns: columns.map((value, cIndex) => {
						const cPath = `${qPath}.columns[${cIndex}]`;
						const column = expectRecord(value, cPath);
						return {
							column: expectString(column.column, `${cPath}.column`),
							key: expectString(column.key, `${cPath}.key`)
						};
					})
				};
			})
		};
	}
	return out;
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

	// R2a-2e: push 接続のブリッジが読む接続がパッケージ内の postgres 接続で
	// あること（名前で持ち運ぶので、ここで引けないと取り込み先で付け直せない）。
	const protocolByName = new Map(pkg.plcConnections.map((c) => [c.name, c.protocol]));
	pkg.plcConnections.forEach((connection, index) => {
		const bridge = configPackagePushBridge(connection);
		if (!bridge) return;
		const protocol = protocolByName.get(bridge.dbConnectionName);
		if (protocol === undefined) {
			throw new ConfigPackageParseError(
				`plcConnections[${index}].settings.bridge.dbConnectionName: push 接続 '${connection.name}' のブリッジが読む connection '${bridge.dbConnectionName}' が見つかりません`
			);
		}
		if (protocol !== 'postgres') {
			throw new ConfigPackageParseError(
				`plcConnections[${index}].settings.bridge.dbConnectionName: push 接続 '${connection.name}' のブリッジが読む connection '${bridge.dbConnectionName}' は postgres 接続ではありません（${protocol}）`
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
		// 版 1・2（2026-10-24 より前の export）は読まない - 互換性は持たない
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
