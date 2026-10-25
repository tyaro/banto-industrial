/**
 * Client for the #383 段階2a / R1-B タグレジストリ API: `plc_connections`
 * (PLC接続), `collection_groups` (収集グループ) と `tags`（タグ）- banto-tags
 * の3階層レジストリを、このアプリが REST/Tauri 両経路で公開したもの。
 * `usersAdmin.ts` と同じ三環境分岐:
 *
 * - Tauri webview -> `invoke()` の `plc_connections_*` / `collection_groups_*`
 *   / `tags_*` コマンド（`apps/chronogazer/src-tauri/src/lib.rs`）。
 * - LAN ブラウザ（組み込みサーバー） -> `fetch()` の
 *   `/api/plc-connections[...]` / `/api/collection-groups[...]` /
 *   `/api/tags[...]` REST ルート（`apps/chronogazer/core/src/rest.rs`）。
 * - 単体ブラウザのデモモード（`vite dev`/`vite preview`）-> レジストリDBが
 *   無いので全呼び出しが `DEMO_MODE_MESSAGE` で拒否される。
 *   `isTagRegistryAvailable()` で事前に判定できる。
 *
 * 3エンティティとも editor-write / viewer-read（R0 §3.6）で、REST/Tauri
 * 両経路が同じ内容で監査する。型は `chronogazer_core::rest` の camelCase
 * `*Payload`/response 型と1:1（banto-tags 自身の snake_case `*Input` では
 * ない - payload がワイヤ形状を持つ）。
 *
 * relay-wright の同名ファイル（`apps/relay-wright/src/lib/banto/
 * tagRegistryAdmin.ts`）を手本にしつつ、chronogazer には無いカスケード削除
 * （relay-wright の feature/easy-delete、このアプリのスコープ外）は持ち込ま
 * ない。banto-hub の 1029 行版（バッチ・式チェック等）も同様に移植しない。
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';
import type { ExclusionView } from './collectAdmin';

export type { ExclusionView };

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

/** 利用可能な機器プロトコルは `listDrivers()` の応答で決める。 */
export type PlcProtocol = string;

/**
 * #383 段階3 P2（2026-10-10 オーナー決定 H1）: レジストリには Hub 経由の接続
 * （`protocol: 'hub'`、`banto_tags::HUB_PROTOCOL`）も入りうるので、**読み取った**
 * 接続の型はこちら。Hub 接続は画面の作成フォームでは作らず、P4 の
 * 「Hub から追加」（{@link registerTagsFromHub}）がサーバー側で暗黙に作る
 * （REST・Tauri は P4 から `hub` の作成も受け付ける）。一覧では読み取り専用で出す。
 */
export type RegistryProtocol = PlcProtocol;

export interface DriverSettingField {
	key: string;
	label: string;
	help: string | null;
	kind: DriverFieldKind;
	required: boolean;
	default: string | number | boolean | null;
	group: 'basic' | 'advanced';
}

export type DriverFieldKind =
	| { type: 'text'; max_len: number | null; placeholder: string | null }
	| { type: 'host' }
	| { type: 'integer'; min: number | null; max: number | null }
	| { type: 'choice'; options: { value: string; label: string }[] }
	| { type: 'bool' }
	| { type: 'secret' };

export interface DriverInfo {
	id: string;
	displayName: string;
	vendor: string | null;
	version: string;
	capabilities: { read: boolean; write: boolean };
	transports: string[];
	settingsSchema: { fields: DriverSettingField[] };
	supportedKinds: string[];
	addressHelp: string;
	kind: 'frame' | 'session';
}

/** Mirrors `chronogazer_core::rest::PlcConnectionResponse`. */
export interface PlcConnection {
	id: number;
	name: string;
	protocol: RegistryProtocol;
	enabled: boolean;
	settings: Record<string, unknown>;
	/**
	 * #413（2026-09-23 オーナー決定）: 接続単位シミュレーション（**レジストリの
	 * 値**）。`true` の接続は実機の代わりにプロセス内シミュレータへ接続し、
	 * 値は現在値・イベントには出るが**データファイルには記録されない**。
	 * 切替は「収集を再起動」まで走っている収集に反映されない - 走っている
	 * 収集が実際にシミュレータ相手かは `collectAdmin.ts` の
	 * `ConnectionView.simulation`。
	 */
	simulation: boolean;
}

/** Mirrors `chronogazer_core::rest::PlcConnectionPayload`. */
export interface PlcConnectionInput {
	name: string;
	/**
	 * 作成フォームが出すのは `listDrivers()` にあるプロトコルだけ。`'hub'` は Hub 接続の「有効」の
	 * 切り替え（#383 段階3 P4）の更新でだけ送る。
	 */
	protocol: RegistryProtocol;
	enabled: boolean;
	settings: Record<string, unknown>;
	/** #413: サーバー側では省略可（作成なら `false`、更新なら既存の値を保つ - `PlcConnectionPayload::simulation`）。この画面は常に送る。 */
	simulation: boolean;
}

/** Mirrors `banto_tags::CollectionGroup`. */
export interface CollectionGroup {
	id: number;
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/** Mirrors `chronogazer_core::rest::CollectionGroupPayload`. */
export interface CollectionGroupInput {
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
}

/**
 * Selectable collection periods (ms) - mirrors `banto_tags::ALLOWED_PERIOD_MS`
 * (recorder-requirements.md §3.1)。バックエンドはこれ以外を人間可読な検証
 * エラーで拒否するので、画面はこの選択肢しか出さない。
 */
export const ALLOWED_PERIOD_MS: readonly number[] = [100, 200, 500, 1000, 2000, 5000, 10000, 60000];

/**
 * R0 §3.1 が要求するデータ型（ビット/16bit/32bit 符号有無/実数、および
 * #325 の64bit拡張）。`banto_tags::ALLOWED_DATA_TYPES` のうち `"string"` は
 * 対応する `stringLength`/`stringEncoding` の入力導線が無いため、このPRの
 * ペイロード/画面からは除いている（`TagInput` doc comment参照）。
 */
export type TagDataType = 'bit' | 'i16' | 'u16' | 'i32' | 'u32' | 'f32' | 'i64' | 'u64' | 'f64';

/** Mirrors `banto_tags::Tag`（本PRが公開するフィールドのみ）。 */
export interface Tag {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	rawLo: number | null;
	rawHi: number | null;
	engLo: number | null;
	engHi: number | null;
	unit: string | null;
	decimals: number;
	// しきい値は持たない（#532: タグ定義の属性ではなく、記録計の側の設定。
	// `tagThresholdsAdmin.ts`）。banto-tags の `tags` にもしきい値の列は無い（#533）。
	enabled: boolean;
	/** #525: 楽観ロック用の版。更新のたびに +1 される（更新時は `TagInput.expectedRevision` に載せる）。 */
	revision: number;
}

/**
 * Mirrors `chronogazer_core::rest::TagPayload`。
 *
 * `PUT` は**置換**（省略した項目は既定値）。更新では常に今の値を送ること。
 * **しきい値は送らない**（#532。値付きで送るとサーバーが `thresholdH` などの
 * 検証エラーで断る）。しきい値の保存は `tagThresholdsAdmin.ts` の
 * `updateTagThresholds`。
 */
export interface TagInput {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	unit?: string | null;
	decimals: number;
	enabled: boolean;
	/** #525: 更新のとき、編集を始めた時点の `Tag.revision`。他者が先に更新していれば `409`（`expectedRevision` の検証エラー）で拒否される。省略すると版を確かめない。 */
	expectedRevision?: number;
}

// --- environment/error plumbing (usersAdmin.ts と同じ作法) ------------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

/** デモモードの拒否（`displayGroupsAdmin.ts` も使う）。 */
export function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** レジストリDBを持つ環境（Tauri または組み込みサーバー）か？ デモモードなら false。 */
export function isTagRegistryAvailable(): boolean {
	return getBantoMode() !== 'demo';
}

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

function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

/** Tauri コマンドの呼び出し（エラーを `ProviderError` にそろえる。`displayGroupsAdmin.ts` も使う）。 */
export async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

export interface HttpInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
}

/** REST の呼び出し（CSRF ヘッダー・Bearer・`ErrorBody` の復元。`displayGroupsAdmin.ts` も使う）。 */
export async function httpRequest<T>(path: string, init: HttpInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (hasBody) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: hasBody ? JSON.stringify(init.body) : undefined
		});
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

	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

// --- PLC connections --------------------------------------------------------

export async function listDrivers(): Promise<DriverInfo[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<DriverInfo[]>('drivers_list');
	return httpRequest<DriverInfo[]>('/api/drivers', { method: 'GET' });
}

export async function listPlcConnections(): Promise<PlcConnection[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<PlcConnection[]>('plc_connections_list');
	return httpRequest<PlcConnection[]>('/api/plc-connections', { method: 'GET' });
}

export async function createPlcConnection(input: PlcConnectionInput): Promise<PlcConnection> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnection>('plc_connections_create', { input });
	}
	return httpRequest<PlcConnection>('/api/plc-connections', { method: 'POST', body: input });
}

export async function updatePlcConnection(
	id: number,
	input: PlcConnectionInput
): Promise<PlcConnection> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<PlcConnection>('plc_connections_update', { id, input });
	}
	return httpRequest<PlcConnection>(`/api/plc-connections/${id}`, { method: 'PUT', body: input });
}

export async function deletePlcConnection(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('plc_connections_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/plc-connections/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- config exclusions (#414 段階2) -------------------------------------------

/**
 * 今のレジストリで収集を開始したら外される接続・グループ・タグ（`viewer`
 * 以上）。`/tags` の印に使う。**判定は Rust（収集の開始と同じ
 * `banto_collect::build_config_lenient_from`）だけ**で、画面は表示するだけ。
 * 収集が走っていなくても返る。
 */
export async function listConfigExclusions(): Promise<ExclusionView[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<ExclusionView[]>('config_exclusions_list');
	}
	return httpRequest<ExclusionView[]>('/api/config-exclusions', { method: 'GET' });
}

// --- collection groups ------------------------------------------------------

export async function listCollectionGroups(): Promise<CollectionGroup[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup[]>('collection_groups_list');
	}
	return httpRequest<CollectionGroup[]>('/api/collection-groups', { method: 'GET' });
}

export async function createCollectionGroup(input: CollectionGroupInput): Promise<CollectionGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup>('collection_groups_create', { input });
	}
	return httpRequest<CollectionGroup>('/api/collection-groups', { method: 'POST', body: input });
}

export async function updateCollectionGroup(
	id: number,
	input: CollectionGroupInput
): Promise<CollectionGroup> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<CollectionGroup>('collection_groups_update', { id, input });
	}
	return httpRequest<CollectionGroup>(`/api/collection-groups/${id}`, {
		method: 'PUT',
		body: input
	});
}

export async function deleteCollectionGroup(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('collection_groups_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/collection-groups/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- tags -------------------------------------------------------------------

export async function listTags(): Promise<Tag[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<Tag[]>('tags_list');
	return httpRequest<Tag[]>('/api/tags', { method: 'GET' });
}

export async function createTag(input: TagInput): Promise<Tag> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<Tag>('tags_create', { input });
	}
	return httpRequest<Tag>('/api/tags', { method: 'POST', body: input });
}

export async function updateTag(id: number, input: TagInput): Promise<Tag> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<Tag>('tags_update', { id, input });
	}
	return httpRequest<Tag>(`/api/tags/${id}`, { method: 'PUT', body: input });
}

export async function deleteTag(id: number): Promise<void> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('tags_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/tags/${id}`, { method: 'DELETE', expectNoContent: true });
}

// --- #383 段階3 P4: Hub から追加 ---------------------------------------------

/** Mirrors `chronogazer_core::hub_tags::HubTagRow`（ダイアログが読んだ catalog の 1 行）。 */
export interface HubTagRow {
	externalName: string;
	name: string;
	dataType: string;
	unit: string | null;
	decimals: number | null;
}

/**
 * Mirrors `chronogazer_core::hub_tags::HubTagRegistration`。登録先は
 * `collectionGroupId`（Hub 接続の下の既存グループ）か `newGroup` の**どちらか 1 つ**。
 */
export interface HubTagRegistration {
	collectionGroupId: number | null;
	newGroup: { name: string; periodMs: number } | null;
	tags: HubTagRow[];
}

/** 1 行分のエラー（`index` = リクエストの `tags` の位置）。 */
export interface HubRowError {
	index: number;
	externalName: string;
	fieldErrors: { field: string; message: string }[];
}

/**
 * Mirrors `chronogazer_core::hub_tags::HubRegistrationResult`。**検証の不合格は
 * 例外ではなく `ok: false`**（banto-hub の `POST /api/tags/batch` と同じ）で、
 * そのときは何も書かれていない。`fieldErrors` は行に属さないエラー（登録先・
 * 新しいグループの `newGroupName` / `newGroupPeriodMs`）。
 */
export interface HubRegistrationResult {
	ok: boolean;
	fieldErrors: { field: string; message: string }[];
	rowErrors: HubRowError[];
	connectionId: number | null;
	connectionCreated: boolean;
	collectionGroup: CollectionGroup | null;
	groupCreated: boolean;
	tags: Tag[];
	/**
	 * 移行前の選択の消去を**予約した**か（消すべき選択があった）。消去はサーバーが
	 * 応答を待たせずに裏で行い、予約した時点から {@link getHubCatalog} の
	 * `legacySelection` は空になる。
	 */
	legacySelectionClearScheduled: boolean;
}

/**
 * `editor`-floor: Hub のカタログから選んだタグを登録する（#383 段階3 P4）。
 * Hub 接続がまだ無ければサーバーが暗黙に作る（新しいグループを作る登録のとき）。
 */
export async function registerTagsFromHub(
	input: HubTagRegistration
): Promise<HubRegistrationResult> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<HubRegistrationResult>('tags_register_from_hub', { input });
	}
	return httpRequest<HubRegistrationResult>('/api/tags/from-hub', {
		method: 'POST',
		body: input
	});
}
