/**
 * 管理系状態 API（`GET /api/status`・`GET /api/values`）のクライアント。
 *
 * **2026-08-31 オーナー決定（案A）: `/api/v1/*` から管理系エンドポイントへ
 * 切り替えた。** 元々は `GET /api/v1/status`・`GET /api/v1/values`（タグ
 * 空間 API）を叩いていたが、`/api/v1/*` は `require_tag_space_auth`（API
 * キー or セッション bearer）固定で、設計 §5.6 の判断により試運転モードの
 * バイパス対象**外**（PLC 書き込み経路と同じ境界を守るため）。試運転モード
 * 中（未ロックダウン・未ログイン・API キー未発行）に管理 UI からこれを
 * 直接叩くと 401 になり、状態ページの「サーバー状態」「タグ現在値」が
 * 空になっていた（`hostSwitchGate.isPreflightOk` が `status.revision` を
 * 要求するため、切替ウィザードまで連鎖的に塞がれる）。
 *
 * `/api/status`・`/api/values`（`apps/banto-hub/core/src/rest.rs` の
 * `admin_status`/`admin_values`、新規）は管理系ルーター（試運転モードの
 * バイパスが効き、ロックダウン済みならセッション認証が要る側）に置かれて
 * おり、ロジックは `/api/v1/*` ハンドラと共有（`compute_status`/
 * `build_values_response`）した上で camelCase に包み直したもの。
 * `/api/v1/*` 自体はルート・認証・レスポンス形状とも一切変更していない
 * （機械クライアントの互換性を壊さないため）。
 *
 * **注意: この管理系エンドポイントの応答は camelCase**
 * （`usersAdmin.ts`/`tagRegistryAdmin.ts` 等、他の管理系 DTO と同じ命名
 * 規則）。`/api/v1/*` 側は意図して snake_case のまま
 * （`apps/banto-hub/core/src/rest.rs` の `StatusResponse`/`ValuesResponse`
 * 参照）- 混同しないこと。
 *
 * 認証はセッション bearer（`Authorization: Bearer <token>`）。管理系
 * ルーターの慣行どおり CSRF ヘッダ（`X-Banto-Client`）を要求するため付ける
 * （`/api/v1/*` は要求しないが、こちらは要求する - 管理系ルーター全体に
 * `require_banto_client_header` が掛かっているため）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

/** `GET /api/status` の `connections` 配列1件分。サーバーは
 * `simulation`/`configuredSimulation`/`effectiveSimulation` も返すが、この
 * 画面群はまだ使っていないので（T18-6時点）フィールドを増やさない -
 * 必要になったらここに追加する。 */
export interface ConnectionStatusEntry {
	name: string;
	id: number;
	/**
	 * T19 S2-a（UX-48、2026-09-03）: `unused` は「有効な収集グループが
	 * 1つも無い接続」（=収集側もセッションを張らない）を表す。`stopped`
	 * （無効化・未同期・実際に切れている等）とは意図的に区別する - 見た目
	 * が同じだと「壊れている」と誤解される（`docs/banto-hub-t19-design.md`
	 * §3.8）。
	 */
	status: 'connected' | 'reconnecting' | 'stopped' | 'unused';
	attempt: number | null;
}

/** `GET /api/status` の `mqtt`（T3、設計 §5.3）。 */
export interface MqttStatusEntry {
	enabled: boolean;
	connected: boolean;
}

/**
 * `GET /api/status` の `grpc`（T4、設計 §5.4）。MQTT と違い「実際に
 * 接続できているか」のライブ状態は持たない - gRPC サーバーは listen する
 * だけなので、設定値がそのまま意図した状態を表す。
 */
export interface GrpcStatusEntry {
	enabled: boolean;
	port: number;
}

/**
 * `GET /api/status` の `system`（T19 S3-b、docs/banto-hub-t19-design.md
 * §3.9、UX-46「サーバー状態の拡充」）。単位はいずれもサーバー側の生値
 * （バイト・パーセント） - MB/GB 表記やゲージ表示への整形は
 * `systemInfoFormat.ts` の純関数が担う（このファイルでは行わない）。
 * `cpu_percent` は論理コア1個 = 100%換算（`sysinfo::Process::cpu_usage`の
 * 単位そのまま）で、プロセス起動直後最初のポーリングでは `0` になりうる
 * （`apps/banto-hub/core/src/system_info.rs`のモジュール doc comment参照）。
 */
export interface SystemInfoEntry {
	cpu_percent: number;
	process_memory_bytes: number;
	host_memory_used_bytes: number;
	host_memory_total_bytes: number;
}

/**
 * S3（docs/banto-hub-external-db-design.md §7 row S3、実装指示8）: `GET
 * /api/status` の `dbSource` 配列1件の中の `groups` 配列1件分 - mirrors
 * `banto_hub_core::rest::AdminDbSourceGroupStatusEntry`。`row_count_last`
 * は生成 SQL が `LIMIT 2` を付けるため `2` は「2行以上」を意味する
 * （`banto_hub_core::db_source::convert::build_wrapper_sql`）。
 */
export interface DbSourceGroupStatusEntry {
	group_id: number;
	group_name: string;
	last_ok_at: number | null;
	last_error: string | null;
	row_count_last: number | null;
}

/**
 * S3: `GET /api/status` の `dbSource` 配列1件分（接続ごとの運転状態） -
 * mirrors `banto_hub_core::rest::AdminDbSourceStatusEntry`。
 * `state`（`banto_hub_core::db_source::DbConnectionState::as_str`）は
 * `connected`/`backoff`/`error`/`stopped`/`disabled` のいずれか -
 * `stopped` は S2b（§4.8・§6-16「収集が Running でない」）、`disabled` は
 * 構成上無効（接続自体の `enabled = false`）で、両者は意図的に区別する。
 */
export interface DbSourceStatusEntry {
	connection_id: number;
	connection_name: string;
	state: 'connected' | 'backoff' | 'error' | 'stopped' | 'disabled' | string;
	last_poll_at: number | null;
	last_error: string | null;
	consecutive_failures: number;
	/** S2b（§4.8・§6-17）: この接続のタスクが異常終了して supervisor に作り直された回数。 */
	restarts: number;
	last_restart_reason: string | null;
	groups: DbSourceGroupStatusEntry[];
}

/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§5.5・§7 row S6）: `GET
 * /api/status` の `sink.groups` 配列1件分 - mirrors
 * `banto_hub_core::rest::AdminSinkGroupStatusEntry`（サイドカーが最後に
 * `PUT /api/sink/status` で push した内容をそのまま映す）。
 */
export interface SinkGroupStatusEntry {
	id: number;
	state: string;
	queued: number;
	dropped: number;
	last_flush_at: number | null;
	last_error: string | null;
}

/**
 * S6: `GET /api/status` の `sink.sidecar` - mirrors
 * `banto_hub_core::rest::AdminSinkSidecarStatusEntry`。`state` は
 * `"online"`（`SINK_SIDECAR_STALE_AFTER_MS`（15秒）以内に push があった）
 * または `"unknown"`（無い、または一度も push が無い）。
 */
export interface SinkSidecarStatusEntry {
	state: 'online' | 'unknown' | string;
	last_seen_at: number | null;
}

/** S6: `GET /api/status` の `sink` 節。 */
export interface SinkStatusEntry {
	sidecar: SinkSidecarStatusEntry;
	groups: SinkGroupStatusEntry[];
}

/** `GET /api/status` の応答。 */
export interface StatusResponse {
	version: string;
	revision: number;
	last_config_error: string | null;
	connections: ConnectionStatusEntry[];
	/** T2-4（設計 §6-6）: 書き込み受付のライブフラグ（起動時は必ず false）。 */
	write_enabled: boolean;
	/** T2-4（設計 §6-6）: プロセス再起動前は有効だったか（表示専用の履歴）。 */
	write_was_enabled_before_restart: boolean;
	/** T3（設計 §5.3）: MQTT publish の設定/接続状態。 */
	mqtt: MqttStatusEntry;
	/** T4（設計 §5.4）: gRPC サーバーの設定。 */
	grpc: GrpcStatusEntry;
	/** T14-4 の収集ライフサイクル状態 - mirrors `CollectionState::as_str()`
	 * (`apps/banto-hub/core/src/controller.rs`). `GET /api/status` の
	 * `collectionState` フィールド。 */
	collection_state: 'stopped' | 'starting' | 'running' | 'stopping' | 'faulted' | string;
	/**
	 * 2026-08-31 オーナー指摘（収集開始/停止 UI 追加）で新規に露出。
	 * mirrors `RunMode::as_str()`（`apps/banto-hub/core/src/controller.rs`）。
	 * `collection_state` だけでは「設定どおり運転」と「全 PLC
	 * シミュレーション運転」を区別できない
	 * （desktop-plan §9.7 の状態表 `running/configured` /
	 * `running/all_simulation` に対応するにはこちらも要る）。
	 */
	collection_mode: 'configured' | 'all_simulation' | string;
	/**
	 * 同上（2026-08-31）。`CollectionStatus.last_error` を運ぶ - 収集の
	 * 起動/実行時エラー（`faulted` 状態の原因）。`last_config_error` とは
	 * 別物（あちらは構成の静的検証エラー）なので混同しないこと。
	 */
	last_runtime_error: string | null;
	/** T19 S3-b（UX-46）: サーバー自身の CPU 使用率・メモリ使用量。 */
	system: SystemInfoEntry;
	/** S3（docs/banto-hub-external-db-design.md §7 row S3）: DB Source の接続ごとの運転状態。 */
	db_source: DbSourceStatusEntry[];
	/** S6（docs/banto-hub-external-db-design.md §5.2・§5.5・§7 row S6）: DB Sink サイドカーの運転状態。 */
	sink: SinkStatusEntry;
}

/** サーバーの camelCase 応答（`AdminDbSourceGroupStatusEntry`）の生形。 */
interface RawDbSourceGroupStatusEntry {
	groupId: number;
	groupName: string;
	lastOkAt: number | null;
	lastError: string | null;
	rowCountLast: number | null;
}

/** サーバーの camelCase 応答（`AdminSinkGroupStatusEntry`）の生形。 */
interface RawSinkGroupStatusEntry {
	id: number;
	state: string;
	queued: number;
	dropped: number;
	lastFlushAt: number | null;
	lastError: string | null;
}

/** サーバーの camelCase 応答（`AdminSinkSidecarStatusEntry`）の生形。 */
interface RawSinkSidecarStatusEntry {
	state: string;
	lastSeenAt: number | null;
}

/** サーバーの camelCase 応答（`AdminSinkStatusEntry`）の生形。 */
interface RawSinkStatusEntry {
	sidecar: RawSinkSidecarStatusEntry;
	groups: RawSinkGroupStatusEntry[];
}

/** サーバーの camelCase 応答（`AdminDbSourceStatusEntry`）の生形。 */
interface RawDbSourceStatusEntry {
	connectionId: number;
	connectionName: string;
	state: string;
	lastPollAt: number | null;
	lastError: string | null;
	consecutiveFailures: number;
	restarts: number;
	lastRestartReason: string | null;
	groups: RawDbSourceGroupStatusEntry[];
}

/** サーバーから受け取る camelCase の生レスポンス形（`AdminStatusResponse`）。 */
interface RawStatusResponse {
	version: string;
	revision: number;
	lastConfigError: string | null;
	connections: ConnectionStatusEntry[];
	writeEnabled: boolean;
	writeWasEnabledBeforeRestart: boolean;
	mqtt: MqttStatusEntry;
	grpc: GrpcStatusEntry;
	collectionState: string;
	collectionMode: string;
	lastRuntimeError: string | null;
	system: {
		cpuPercent: number;
		processMemoryBytes: number;
		hostMemoryUsedBytes: number;
		hostMemoryTotalBytes: number;
	};
	dbSource: RawDbSourceStatusEntry[];
	sink: RawSinkStatusEntry;
}

/** サーバーの camelCase 応答を、このファイルが公開する既存の型（snake_case
 * のキーを持つ `StatusResponse`）へ変換する。既存の呼び出し元
 * （`status`/`settings`/`tags` の各画面）はこのファイル冒頭のdoc comment
 * のとおり `/api/v1/*` 時代の snake_case キーのまま参照し続けられる -
 * サーバー側の命名規則の変更をこのモジュール内に閉じ込める。 */
function fromRawStatus(raw: RawStatusResponse): StatusResponse {
	return {
		version: raw.version,
		revision: raw.revision,
		last_config_error: raw.lastConfigError,
		connections: raw.connections,
		write_enabled: raw.writeEnabled,
		write_was_enabled_before_restart: raw.writeWasEnabledBeforeRestart,
		mqtt: raw.mqtt,
		grpc: raw.grpc,
		collection_state: raw.collectionState,
		collection_mode: raw.collectionMode,
		last_runtime_error: raw.lastRuntimeError,
		system: {
			cpu_percent: raw.system.cpuPercent,
			process_memory_bytes: raw.system.processMemoryBytes,
			host_memory_used_bytes: raw.system.hostMemoryUsedBytes,
			host_memory_total_bytes: raw.system.hostMemoryTotalBytes
		},
		db_source: raw.dbSource.map((entry) => ({
			connection_id: entry.connectionId,
			connection_name: entry.connectionName,
			state: entry.state,
			last_poll_at: entry.lastPollAt,
			last_error: entry.lastError,
			consecutive_failures: entry.consecutiveFailures,
			restarts: entry.restarts,
			last_restart_reason: entry.lastRestartReason,
			groups: entry.groups.map((group) => ({
				group_id: group.groupId,
				group_name: group.groupName,
				last_ok_at: group.lastOkAt,
				last_error: group.lastError,
				row_count_last: group.rowCountLast
			}))
		})),
		sink: {
			sidecar: {
				state: raw.sink.sidecar.state,
				last_seen_at: raw.sink.sidecar.lastSeenAt
			},
			groups: raw.sink.groups.map((group) => ({
				id: group.id,
				state: group.state,
				queued: group.queued,
				dropped: group.dropped,
				last_flush_at: group.lastFlushAt,
				last_error: group.lastError
			}))
		}
	};
}

/** `GET /api/values` の1タグ分（`{ tag, v, q, t }`）。 */
export interface ValueEntry {
	tag: string;
	v: number | null;
	q: 'good' | 'bad' | 'stale' | string;
	t: number;
}

/** `GET /api/values` の応答。 */
export interface ValuesResponse {
	revision: number;
	t: number;
	values: ValueEntry[];
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function httpGet<T>(path: string): Promise<T> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, { method: 'GET', headers });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	return (await response.json()) as T;
}

export async function getHubStatus(): Promise<StatusResponse> {
	const raw = await httpGet<RawStatusResponse>('/api/status');
	return fromRawStatus(raw);
}

/** 全タグの現在値スナップショット（`?tags=` 省略 = 全件）。 */
export async function getHubValues(): Promise<ValuesResponse> {
	return httpGet<ValuesResponse>('/api/values');
}
