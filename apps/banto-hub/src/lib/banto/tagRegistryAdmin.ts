/**
 * I1 タグレジストリ API（`plc_connections`/`collection_groups`/`tags` -
 * banto-tags の三層レジストリ）のクライアント。
 *
 * relay-wright の同名ファイル（336行）から複製し、Tauri 分岐を削除して
 * HTTP 一択にした。**型定義・ワイヤ形状は relay-wright から1バイトも
 * 変えていない** — banto-hub の `/api/plc-connections|collection-groups|tags`
 * REST は relay-wright/chronogazer と完全に同型（camelCase の
 * `*Payload`/`*Input` DTO、`apps/banto-hub/core/src/rest.rs` の
 * `PlcConnectionPayload`/`CollectionGroupPayload`/`TagPayload` 参照）。
 *
 * relay-wright にあった「プレーン `vite dev`/デモモードでは
 * DEMO_MODE_MESSAGE で reject する」という三環境分岐は完全に削除した —
 * banto-hub にはそもそもデモモードが存在しない（`setup.ts` が常に HTTP
 * プロバイダを配線する）ため、`isTagRegistryAvailable()`/
 * `DEMO_MODE_MESSAGE` のような「利用不可」表現自体が不要と判断し、
 * ページ側の呼び出し元ごと削っている。
 */
import {
	getAuthProvider,
	ProviderError,
	type ErrorBody,
	type ListParams,
	type ListResult
} from '@banto/admin-core';
import { CSRF_HEADER } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

export type PlcProtocol = 'modbus-tcp' | 'slmp' | 'virtual';

/**
 * P3-b（監査指摘 2026-08-12）: SLMP 接続のワード順（32bit値の上位/下位ワードの
 * 並び）— mirrors `banto_plc::decode::WordOrder`（`banto_tags::PlcConnection`
 * では検証済みの文字列として保存される。`ALLOWED_WORD_ORDERS` 参照）。
 * `"slmp"` 接続でのみ意味を持つ（modbus-tcp/virtual では無意味 — `unitId` と
 * 同じ扱い）。
 */
export type SlmpWordOrder = 'low_high' | 'high_low';

/**
 * ワード順のセレクト肢 — mirrors `banto_tags::plc_connection::ALLOWED_WORD_ORDERS`。
 * 既定は `low_high`（MELSEC 標準、D0=下位/D1=上位）。
 */
export const WORD_ORDER_OPTIONS: { value: SlmpWordOrder; label: string }[] = [
	{ value: 'low_high', label: 'low_high（MELSEC標準・既定）' },
	{ value: 'high_low', label: 'high_low（Modbus/IEEE慣習）' }
];

/** Mirrors `banto_tags::PlcConnection`. */
export interface PlcConnection {
	id: number;
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	/**
	 * T9-2 (docs/ux-plan.md §1): 接続単位のシミュレーションモード。true の間、
	 * 実PLCの代わりに内蔵シミュレータへ接続する（開発・検証用、本番非推奨）。
	 */
	simulation: boolean;
	/** P3-b（監査指摘 2026-08-12）。{@link SlmpWordOrder}参照。 */
	wordOrder: SlmpWordOrder;
}

/**
 * T6-2 (docs/tag-server-design.md §4.2/§4.3(a)): the two reserved
 * `protocol: "virtual"` connections `banto-hub` auto-provisions at startup
 * (`bin/banto-hub.rs::ensure_virtual_connection`) — `calc` hosts every
 * `tagKind: "computed"` tag's group, `mem` hosts every `"internal"` tag's
 * group. Mirrors `banto_tags::{CALC_CONNECTION_NAME, MEM_CONNECTION_NAME}`.
 * The backend also refuses to edit/delete a `"virtual"` connection
 * (`PlcConnectionService::update`/`delete`) — the plc-connections page uses
 * these names to show that row as read-only rather than letting the user
 * discover the 403 by trying.
 */
export const CALC_CONNECTION_NAME = 'calc';
export const MEM_CONNECTION_NAME = 'mem';

/** True for the two reserved auto-provisioned connections (see above). */
export function isVirtualConnection(conn: Pick<PlcConnection, 'protocol'>): boolean {
	return conn.protocol === 'virtual';
}

/** Mirrors `banto_hub_core::rest::PlcConnectionPayload`. */
export interface PlcConnectionInput {
	name: string;
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	/** T9-2 (docs/ux-plan.md §1). See {@link PlcConnection.simulation}. */
	simulation: boolean;
	/** P3-b（監査指摘 2026-08-12）. See {@link PlcConnection.wordOrder}. */
	wordOrder: SlmpWordOrder;
}

/** Mirrors `banto_tags::CollectionGroup`. */
export interface CollectionGroup {
	id: number;
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
	/**
	 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2、2026-09-02 オーナー
	 * 決定「グループ単位の既定値は DB 列に持つ」）: このグループへ新規タグを
	 * 登録するときの `writable` チェックボックスの既定値。`tags.writable`
	 * 自体の検証（computed タグ拒否・Modbus 読み取り専用領域拒否）とは
	 * 無関係 - あくまで新規タグフォームを開いた瞬間の UI 上の初期値だけを
	 * 決める（`$lib/banto/writableDefault.ts` 参照）。
	 */
	defaultWritable: boolean;
}

/** Mirrors `banto_hub_core::rest::CollectionGroupPayload`. */
export interface CollectionGroupInput {
	name: string;
	plcConnectionId: number;
	periodMs: number;
	enabled: boolean;
	/** {@link CollectionGroup.defaultWritable} 参照。 */
	defaultWritable: boolean;
}

/**
 * Selectable collection periods (ms) — mirrors
 * `banto_tags::ALLOWED_PERIOD_MS` (the backend rejects anything else with a
 * field-level validation error, so the UI only ever offers these).
 */
export const ALLOWED_PERIOD_MS: readonly number[] = [100, 200, 500, 1000, 2000, 5000, 10000, 60000];

export type TagDataType = 'bit' | 'i16' | 'u16' | 'i32' | 'u32' | 'f32' | 'string';

/**
 * Tag species (T6-2, docs/tag-server-design.md §4.2's table) — mirrors
 * `banto_tags::ALLOWED_TAG_KINDS`.
 *
 * - `plc` (default/existing): value from the collection task, `address`
 *   required, `expression` forbidden.
 * - `computed`: value from evaluating `expression` (banto-expr); `address`
 *   forbidden, `expression` required, `writable` forced false server-side
 *   (write attempts always 403 — "値は式が決める").
 * - `internal`: value from client writes, held entirely in the tag space
 *   (never sent to a PLC); `address`/`expression` both forbidden, `retain`
 *   selects restart persistence.
 *
 * Placement is enforced server-side (`banto_tags::tag::validate_tag_kind_placement`):
 * `computed` only under the `calc` connection, `internal` only under `mem`,
 * `plc` under neither.
 */
export type TagKind = 'plc' | 'computed' | 'internal';

export const TAG_KIND_OPTIONS: { value: TagKind; label: string }[] = [
	{ value: 'plc', label: 'plc（PLC 収集）' },
	{ value: 'computed', label: 'computed（演算タグ）' },
	{ value: 'internal', label: 'internal（内部タグ）' }
];

/** Mirrors `banto_tags::Tag`. */
export interface Tag {
	id: number;
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	/**
	 * Consecutive 16-bit word devices a `string` tag occupies; `Some(1..=128)`
	 * iff `dataType === 'string'`, `null` otherwise.
	 */
	stringLength: number | null;
	rawLo: number | null;
	rawHi: number | null;
	engLo: number | null;
	engHi: number | null;
	unit: string | null;
	decimals: number;
	thresholdH: number | null;
	thresholdHh: number | null;
	thresholdL: number | null;
	thresholdLl: number | null;
	enabled: boolean;
	/** Per-tag write opt-in (T2-3, docs/tag-server-design.md §6 item 1). */
	writable: boolean;
	/** T6-2: tag species — see {@link TagKind}. */
	tagKind: TagKind;
	/** T6-2: computed-tag formula source, only set when `tagKind === 'computed'`. */
	expression: string | null;
	/** T6-2: internal-tag restart-persistence flag. */
	retain: boolean;
	/**
	 * T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目):
	 * 楽観的ロック用の行バージョン。`update` が成功する度に +1 される。
	 * 編集フォームを開いた時点の値を保持しておき、保存時に
	 * {@link TagInput.expectedRevision} として送り返す。
	 */
	revision: number;
}

/** Mirrors `banto_hub_core::rest::TagPayload`. */
export interface TagInput {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	stringLength?: number | null;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	unit?: string | null;
	decimals: number;
	thresholdH?: number | null;
	thresholdHh?: number | null;
	thresholdL?: number | null;
	thresholdLl?: number | null;
	enabled: boolean;
	/** T2-3: `#[serde(default)]` on the backend - omitting this still creates
	 * a non-writable tag, so existing callers of `createTag`/`updateTag` are
	 * unaffected. The admin UI (this app's tags page) always sends it
	 * explicitly from its new checkbox. */
	writable?: boolean;
	/** T6-2: `#[serde(default)]` (= `"plc"`) on the backend. */
	tagKind?: TagKind;
	/** T6-2: required when `tagKind === 'computed'`, otherwise omitted. */
	expression?: string | null;
	/** T6-2: `#[serde(default)]` (= `false`) on the backend. */
	retain?: boolean;
	/**
	 * T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目):
	 * 更新時の楽観ロック。編集フォームを開いた時点の
	 * {@link Tag.revision} を渡す — サーバー側の行が既に進んでいたら
	 * `updateTag` は {@link TagRevisionConflictError} を投げる。
	 * 省略時（`createTag`/連続登録の `createTagsBatch` は常に省略）は
	 * `#[serde(default)]` によりチェック無しで更新される
	 * （バックエンド互換のための挙動で、このページの編集フローは
	 * 常に明示的に渡す）。
	 */
	expectedRevision?: number;
}

/** `string` tag `stringLength` bounds — mirrors the backend CHECK/validation. */
export const MIN_STRING_LENGTH = 1;
export const MAX_STRING_LENGTH = 128;

/**
 * `decimals` bounds — mirrors `banto_tags::tag::{MIN_DECIMALS, MAX_DECIMALS}`
 * (`crates/banto-tags/src/tag.rs`). T18-3e（BantoGrid セル編集）の
 * `decimals` 列 `validate` が使う。
 */
export const MIN_DECIMALS = 0;
export const MAX_DECIMALS = 6;

// --- error plumbing (mirrors writeRegistryAdmin.ts) --------------------------

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

interface HttpInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
	/**
	 * T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目):
	 * エラーレスポンスの body が共通の `ErrorBody`（`kind` タグ）形状を
	 * していない呼び出し元固有のエラー（例: `tags_update` の 409
	 * `tag_revision_conflict` — `error`/`message`/`tag` という別形状）を
	 * 独自の `Error` にマップするためのフック。`undefined` を返した場合は
	 * 通常の `ErrorBody`/`other` フォールバックに委ねる。
	 */
	mapErrorBody?: (body: unknown, status: number) => Error | undefined;
}

async function httpRequest<T>(path: string, init: HttpInit): Promise<T> {
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
		const mapped = init.mapErrorBody?.(body, response.status);
		if (mapped) throw mapped;
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	if (response.status === 202) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isQueuedWhileRunningBody(body)) throw new QueuedWhileRunningError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

/**
 * 監査③（2026-08-12）是正: 収集稼働中は plc-connections/collection-groups/tags
 * の作成・更新・削除が即時適用されず `queue_pending_registry_change`
 * （`apps/banto-hub/core/src/rest.rs`）が 202 Accepted +
 * `QueuedPendingChangeResponse { queued: true, pending, status, message }`
 * を返す。`response.ok` は 202 も真になるため、これを検出せず素通しすると
 * 呼び出し元が「作成済みリソース」型（`PlcConnection`/`CollectionGroup`/
 * `Tag`）として扱ってしまい `.id`/`.name` が `undefined` になる
 * （configPackageAdmin.ts の import がこれで壊れていた）。この 202 を
 * 検出して判別可能な例外に変換し、呼び出し元に「作成済みではなくキュー
 * 投入された」ことを必ず伝える。
 */
export class QueuedWhileRunningError extends Error {
	readonly pending: unknown;
	readonly status: unknown;

	constructor(body: { message: string; pending?: unknown; status?: unknown }) {
		super(body.message);
		this.name = 'QueuedWhileRunningError';
		this.pending = body.pending;
		this.status = body.status;
	}
}

export function isQueuedWhileRunningError(error: unknown): error is QueuedWhileRunningError {
	return error instanceof QueuedWhileRunningError;
}

function isQueuedWhileRunningBody(
	value: unknown
): value is { queued: true; message: string; pending?: unknown; status?: unknown } {
	if (typeof value !== 'object' || value === null) return false;
	const v = value as { queued?: unknown; message?: unknown };
	return v.queued === true && typeof v.message === 'string';
}

// --- PLC connections --------------------------------------------------------

export async function listPlcConnections(): Promise<PlcConnection[]> {
	return httpRequest<PlcConnection[]>('/api/plc-connections', { method: 'GET' });
}

export async function createPlcConnection(input: PlcConnectionInput): Promise<PlcConnection> {
	return httpRequest<PlcConnection>('/api/plc-connections', { method: 'POST', body: input });
}

export async function updatePlcConnection(
	id: number,
	input: PlcConnectionInput
): Promise<PlcConnection> {
	return httpRequest<PlcConnection>(`/api/plc-connections/${id}`, { method: 'PUT', body: input });
}

export async function deletePlcConnection(id: number): Promise<void> {
	await httpRequest<void>(`/api/plc-connections/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- collection groups ------------------------------------------------------

export async function listCollectionGroups(): Promise<CollectionGroup[]> {
	return httpRequest<CollectionGroup[]>('/api/collection-groups', { method: 'GET' });
}

export async function createCollectionGroup(input: CollectionGroupInput): Promise<CollectionGroup> {
	return httpRequest<CollectionGroup>('/api/collection-groups', { method: 'POST', body: input });
}

export async function updateCollectionGroup(
	id: number,
	input: CollectionGroupInput
): Promise<CollectionGroup> {
	return httpRequest<CollectionGroup>(`/api/collection-groups/${id}`, {
		method: 'PUT',
		body: input
	});
}

export async function deleteCollectionGroup(id: number): Promise<void> {
	await httpRequest<void>(`/api/collection-groups/${id}`, {
		method: 'DELETE',
		expectNoContent: true
	});
}

// --- tags -------------------------------------------------------------------

export async function listTags(): Promise<Tag[]> {
	return httpRequest<Tag[]>('/api/tags', { method: 'GET' });
}

/**
 * T18-5a 第2段（docs/banto-hub-t18-design.md §4 決定6「薄い部品の先行配線」）:
 * `POST /api/tags/list` — フィルタ/ソート/ページングつきのタグ一覧。
 * `listWriteAudit`（`writeAuditAdmin.ts`）と同型の素通し呼び出し。まだどの
 * UI からも使われていない（配線のみ）。
 */
export async function listTagsPaged(params: ListParams): Promise<ListResult<Tag>> {
	return httpRequest<ListResult<Tag>>('/api/tags/list', { method: 'POST', body: params });
}

/**
 * T18-5a 第2段（同 §4 決定6）: `GET /api/tags/group-counts` の応答行 -
 * mirrors `banto_tags::GroupTagCount`。
 */
export interface GroupTagCount {
	collectionGroupId: number;
	tagCount: number;
}

/**
 * グループ別のタグ件数集計（`GET /api/tags/group-counts`）。まだどの UI
 * からも使われていない（配線のみ）。
 */
export async function listTagGroupCounts(): Promise<GroupTagCount[]> {
	return httpRequest<GroupTagCount[]>('/api/tags/group-counts', { method: 'GET' });
}

export async function createTag(input: TagInput): Promise<Tag> {
	return httpRequest<Tag>('/api/tags', { method: 'POST', body: input });
}

/**
 * T18-1 (docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目): `updateTag` が
 * `expectedRevision` を渡した際、サーバー側の行が既に他クライアントの
 * 更新で先に進んでいた場合に投げる — mirrors
 * `banto_hub_core::rest::RegistryMutationError::TagRevisionConflict` の
 * 409 応答 `{ error: "tag_revision_conflict", message, tag }`。
 * `current` はその時点のサーバー最新の {@link Tag} で、呼び出し元は
 * これでフォームを上書きする（差分の並列表示は本 PR のスコープ外 -
 * ローカル編集は破棄してサーバー最新を表示するだけ）。
 */
export class TagRevisionConflictError extends Error {
	readonly current: Tag;

	constructor(message: string, current: Tag) {
		super(message);
		this.name = 'TagRevisionConflictError';
		this.current = current;
	}
}

export function isTagRevisionConflictError(error: unknown): error is TagRevisionConflictError {
	return error instanceof TagRevisionConflictError;
}

function mapTagUpdateErrorBody(body: unknown, status: number): Error | undefined {
	if (status !== 409 || typeof body !== 'object' || body === null) return undefined;
	const { error, message, tag } = body as { error?: unknown; message?: unknown; tag?: unknown };
	if (error !== 'tag_revision_conflict' || typeof message !== 'string' || tag === null) {
		return undefined;
	}
	return new TagRevisionConflictError(message, tag as Tag);
}

export async function updateTag(id: number, input: TagInput): Promise<Tag> {
	return httpRequest<Tag>(`/api/tags/${id}`, {
		method: 'PUT',
		body: input,
		mapErrorBody: mapTagUpdateErrorBody
	});
}

export async function deleteTag(id: number): Promise<void> {
	await httpRequest<void>(`/api/tags/${id}`, { method: 'DELETE', expectNoContent: true });
}

// --- T11-1 一括登録 (docs/ux-plan.md §3) -------------------------------------

/** Mirrors `banto_hub_core::rest::BatchTagFieldErrorResponse`. */
export interface BatchTagFieldError {
	field: string;
	message: string;
}

/**
 * 行番号(0起点)付きのフィールドエラー — mirrors
 * `banto_hub_core::rest::BatchTagRowErrorResponse`. `index` はリクエストの
 * `tags` 配列内の位置（連続登録プレビューの行、将来の T11-2 では CSV の
 * データ行に対応）。
 */
export interface BatchTagRowError {
	index: number;
	fieldErrors: BatchTagFieldError[];
}

/**
 * `POST /api/tags/batch` の応答 — mirrors
 * `banto_hub_core::rest::BatchTagsResponse`。**常に HTTP 200** で返る
 * （`ok: false` は「1件以上のエラーで全体拒否」という通常の応答であって
 * 例外ではない — 認証/権限/DB エラーは通常どおり `httpRequest` が
 * `ProviderError` を投げる）。
 */
export interface BatchTagsResult {
	ok: boolean;
	dryRun: boolean;
	/** 適用された(または dry run で適用されたはずの)件数。`ok: false` なら常に0。 */
	count: number;
	errors: BatchTagRowError[];
	/** `ok && !dryRun` のときだけ存在(実際に作成されたタグ)。 */
	tags?: Tag[];
}

/**
 * T11-1 の一括登録 API。連続登録（`$lib/banto/continuousRegistration.ts`
 * が展開した `TagInput[]`）と、将来の T11-2 CSV インポートが共有する。
 * `dryRun: true` は検証のみで DB 無変更（プレビュー確認後に
 * `dryRun: false` で本適用する2段階フロー — 設計「dry-run 必須」）。
 */
export async function createTagsBatch(tags: TagInput[], dryRun: boolean): Promise<BatchTagsResult> {
	return httpRequest<BatchTagsResult>('/api/tags/batch', {
		method: 'POST',
		body: { tags, dryRun }
	});
}

// --- T18-3b 一括操作 (docs/banto-hub-t18-design.md「T18-3b 一括操作」) -------

/**
 * `POST /api/tags/batch-update` の1行分 - mirrors
 * `banto_hub_core::rest::TagBatchUpdatePayload`（`#[serde(flatten)]` で
 * {@link TagInput} の全フィールドを JSON 直下に展開し、`id` だけ乗せる形。
 * 単票 PUT（`updateTag`）と同じく `expectedRevision` は任意 - 一括操作の
 * 呼び出し元（`+page.svelte`/`$lib/banto/tagBulkOps.ts`）は常に選択時点の
 * {@link Tag.revision} を明示的に渡す。
 */
export interface BatchTagUpdateRow extends TagInput {
	id: number;
}

/**
 * 行番号(0起点)付きのフィールドエラー — mirrors
 * `banto_hub_core::rest::BatchTagUpdateRowErrorResponse`。T11-1 の
 * {@link BatchTagRowError} と同じ形に、行が元々持っている `id` を足した
 * だけ（更新対象なので、クライアントは `index` だけでなく `id` でも行を
 * 突き合わせられる）。
 */
export interface BatchTagUpdateRowError {
	index: number;
	id: number;
	fieldErrors: BatchTagFieldError[];
}

/**
 * `POST /api/tags/batch-update` の応答 — mirrors
 * `banto_hub_core::rest::BatchTagsUpdateResponse`。T11-1 の
 * {@link BatchTagsResult} と同じ「常に HTTP 200、`ok: false` は行ごと
 * エラーの通常応答」契約（あちらの doc comment 参照）。
 */
export interface BatchTagsUpdateResult {
	ok: boolean;
	dryRun: boolean;
	/** 適用された(または dry run で適用されたはずの)件数。`ok: false` なら常に0。 */
	count: number;
	errors: BatchTagUpdateRowError[];
	/** `ok && !dryRun` のときだけ存在(実際に更新されたタグ)。 */
	tags?: Tag[];
}

/**
 * T18-3b の一括更新 API。`createTagsBatch`（T11-1、新規作成）の更新版 -
 * 稼働中は 202 でキュー投入される（`httpRequest` が既存どおり
 * {@link QueuedWhileRunningError} を投げる）ので、呼び出し元は他の
 * 書き込み系呼び出しと同じ catch で扱える。
 */
export async function updateTagsBatch(
	rows: BatchTagUpdateRow[],
	dryRun: boolean
): Promise<BatchTagsUpdateResult> {
	return httpRequest<BatchTagsUpdateResult>('/api/tags/batch-update', {
		method: 'POST',
		body: { tags: rows, dryRun }
	});
}

// --- T12 接続テスト (docs/ux-plan.md §4) -------------------------------------

/**
 * `POST /api/plc-connections/test` のリクエスト — mirrors
 * `banto_hub_core::rest::PlcConnectionTestPayload`。フォームの現在値
 * （未保存でもよい）をそのまま送る。`connectionId` は「保存済み接続の
 * 編集フォームからのテスト」のときだけ付与する（省略時はバックエンドが
 * 「新規作成中の接続」として扱う）。
 */
export interface PlcConnectionTestRequest {
	protocol: PlcProtocol;
	host: string;
	port: number;
	unitId: number;
	simulation: boolean;
	connectionId?: number;
}

/**
 * 接続テスト失敗の理由 — mirrors
 * `banto_hub_core::rest::PlcConnectionTestError`。`message` は
 * 対処ヒント込みの日本語文言なので、UI はそのまま表示すればよい
 * （`kind` は表示の出し分けに使ってもよいが必須ではない）。
 */
export interface PlcConnectionTestError {
	kind: 'tcp' | 'timeout' | 'protocol' | 'device' | 'unsupported';
	message: string;
}

/**
 * `POST /api/plc-connections/test` の応答 — mirrors
 * `banto_hub_core::rest::PlcConnectionTestResponse`。**常に HTTP 200**
 * （`ok: false` は「疎通確認の結果が失敗だった」という通常の応答であって
 * 例外ではない — 認証/権限/CSRF エラーは通常どおり `httpRequest` が
 * `ProviderError` を投げる）。
 */
export interface PlcConnectionTestResult {
	ok: boolean;
	elapsedMs: number;
	error: PlcConnectionTestError | null;
}

/**
 * T12: 接続の保存前に疎通確認する。TCP 接続だけでなく実プロトコルでの
 * 軽い読み出し1回まで行う（docs/ux-plan.md §4）。
 */
export async function testPlcConnection(
	input: PlcConnectionTestRequest
): Promise<PlcConnectionTestResult> {
	return httpRequest<PlcConnectionTestResult>('/api/plc-connections/test', {
		method: 'POST',
		body: input
	});
}
