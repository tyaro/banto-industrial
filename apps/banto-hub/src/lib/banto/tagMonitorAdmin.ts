/**
 * ライブタグモニタ（T10、docs/ux-plan.md §2）のクライアント。
 *
 * **2026-08-31 オーナー決定（案A の見落とし是正）**: catalog（`getCatalog`）・
 * WS 購読（`connectTagStream`）とも、元々は `/api/v1/tags`・`/api/v1/stream`
 * （タグ空間 API）を直接叩いていた。しかし `/api/v1/*` は
 * `require_tag_space_auth`（API キー or セッション bearer）固定で、設計
 * §5.6 の判断により試運転モードのバイパス対象**外**（PLC 書き込み経路と
 * 同じ境界を守るため）。同日に状態ページ（`hubStatus.ts`）を管理系
 * エンドポイントへ切り替えて解消したはずだったが、**このモニタが同じ問題を
 * 抱えていることを見落としていた** - 試運転モード中（未ロックダウン・
 * 未ログイン・API キー未発行）は `getCatalog()` が401になり、行が1つも
 * 出ない不具合の直接の原因だった。
 *
 * `getCatalog` は `apps/banto-hub/core/src/rest.rs` の `admin_tag_catalog`
 * （`GET /api/tag-catalog`、`hubStatus.ts`と同じ管理系ルーター - 試運転
 * モードのバイパスが効き、ロックダウン済みならセッション認証が要る側）を
 * 叩く。ロジックは `/api/v1/tags`（`v1_tags`）ハンドラと
 * `build_catalog_response` を共有しており、`/api/v1/*` 自体はルート・
 * 認証・レスポンス形状とも一切変更していない（機械クライアントの互換性を
 * 壊さないため）。
 *
 * **注意: この管理系エンドポイントの応答は camelCase**
 * （`hubStatus.ts`/`usersAdmin.ts` 等、他の管理系 DTO と同じ命名規則）。
 * `/api/v1/*` 側は意図して snake_case のまま - 混同しないこと。このファイル
 * が外部へ公開する `CatalogTagEntry`/`CatalogResponse` は
 * `/api/v1/*` 時代からの snake_case キーのまま据え置き、サーバーの命名
 * 規則の違いは `fromRawCatalog` の中に閉じ込める（`hubStatus.ts` の
 * `fromRawStatus` と同じ方針）。
 *
 * WS 購読（`connectTagStream`）は難所だった: ブラウザの `WebSocket`
 * コンストラクタはカスタムヘッダを一切送れない（`Authorization` は
 * もちろん、CSRF 用の `X-Banto-Client` も）。管理系ルーター一式は
 * `require_banto_client_header`（CSRF）を被せているため、`/api/tag-catalog`
 * と同じ管理系ルーターに WS を同居させるとブラウザから絶対に繋がらなく
 * なる。そこでサーバー側は `admin_tag_stream_router`（`apps/banto-hub/core/src/rest.rs`）
 * という CSRF レイヤーの外側の専用ルーターを新設し、`/api/tag-stream` を
 * 用意した（ハンドラ自体は `/api/v1/stream` と共有 - `crate::stream::ws_upgrade`）。
 * このクライアントは接続のたびに `sessionStore.commissioningMode`
 * （`$lib/session.svelte.ts`、`$lib/banto/commissioning.ts` の判定結果を
 * ルートガードがキャッシュしたもの）を見て分岐する:
 *
 * - **試運転モード中**（`commissioningMode === true`）: サーバー側
 *   （`require_auth_or_commissioning`）が未ロックダウン中は無条件で
 *   通すため、トークン無し・`Sec-WebSocket-Protocol` オファー無しで
 *   `/api/tag-stream` へ即接続する。
 * - **ロックダウン済み**（従来どおり）: `/api/v1/stream` へ、セッション
 *   token を `Sec-WebSocket-Protocol: bearer, <token>` で運ぶ - 元々の
 *   仕組みをそのまま維持する（`apps/banto-hub/core/src/rest.rs` の
 *   `extract_ws_protocol_token` が受け付ける方式。`new WebSocket(url,
 *   ['bearer', token])` と書くと、ブラウザが自動的にこの形式のヘッダを
 *   送る - トークンが URL やクエリ文字列に出ないので、サーバーの
 *   アクセスログやブラウザ履歴に残らない）。
 *
 * ロックダウン済みでも `/api/tag-stream` 自体は接続できる（サーバー側は
 * 同じ`Sec-WebSocket-Protocol`方式で有効なセッション bearer を要求する -
 * `require_auth_or_commissioning`のdoc comment参照）が、このクライアントは
 * 「ロックダウン済みでは従来どおり」の方針に合わせて`/api/v1/stream`を
 * 使い続ける（変更範囲を試運転モード中の不具合修正だけに絞るため）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER } from './setup';
import { sessionStore } from '$lib/session.svelte';

/** `GET /api/v1/tags`（および管理系 `GET /api/tag-catalog`）の1タグ分
 * （`apps/banto-hub/core/src/hub.rs::TagEntry` と同型）。管理系応答は
 * camelCase で届く（`fromRawCatalog` 参照）が、この型自体は
 * `/api/v1/*` 時代からの snake_case キーのまま据え置く - 呼び出し元
 * （画面側）を変更しないため。 */
export interface CatalogTagEntry {
	external_name: string;
	tag_key: string;
	ids: [number, number, number];
	connection: string;
	group: string;
	name: string;
	address: string;
	data_type: string;
	unit: string | null;
	decimals: number;
	period_ms: number;
	enabled: boolean;
	writable: boolean;
	tag_kind: string;
	expression: string | null;
	retain: boolean;
	simulation: boolean;
}

/** `GET /api/v1/tags` の応答: `{ revision, tags: TagEntry[] }`。 */
export interface CatalogResponse {
	revision: number;
	tags: CatalogTagEntry[];
}

/** サーバー（`GET /api/tag-catalog`、`AdminCatalogTagEntry`）から受け取る
 * camelCase の生レスポンス形の1タグ分。 */
interface RawCatalogTagEntry {
	externalName: string;
	tagKey: string;
	ids: [number, number, number];
	connection: string;
	group: string;
	name: string;
	address: string;
	dataType: string;
	unit: string | null;
	decimals: number;
	periodMs: number;
	enabled: boolean;
	writable: boolean;
	tagKind: string;
	expression: string | null;
	retain: boolean;
	simulation: boolean;
}

/** サーバー（`GET /api/tag-catalog`、`AdminCatalogResponse`）から受け取る
 * camelCase の生レスポンス形。 */
interface RawCatalogResponse {
	revision: number;
	tags: RawCatalogTagEntry[];
}

/** サーバーの camelCase 応答を、このファイルが公開する既存の型
 * （snake_case のキーを持つ `CatalogResponse`）へ変換する - `hubStatus.ts`
 * の `fromRawStatus` と同じ方針（サーバー側の命名規則の変更をこのモジュール
 * 内に閉じ込める）。 */
function fromRawCatalog(raw: RawCatalogResponse): CatalogResponse {
	return {
		revision: raw.revision,
		tags: raw.tags.map((t) => ({
			external_name: t.externalName,
			tag_key: t.tagKey,
			ids: t.ids,
			connection: t.connection,
			group: t.group,
			name: t.name,
			address: t.address,
			data_type: t.dataType,
			unit: t.unit,
			decimals: t.decimals,
			period_ms: t.periodMs,
			enabled: t.enabled,
			writable: t.writable,
			tag_kind: t.tagKind,
			expression: t.expression,
			retain: t.retain,
			simulation: t.simulation
		}))
	};
}

/** `/api/v1/stream` の `op: "data"` の1タグ分（`{ tag, v, q, t }`）。 */
export interface StreamValue {
	tag: string;
	v: number | null;
	q: string;
	t: number;
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

/**
 * 管理系エンドポイント（`/api/tag-catalog`）向け `GET` ヘルパー -
 * `hubStatus.ts` の `httpGet` と同じ形（CSRF ヘッダ + Bearer 併用）。
 * このファイルの doc comment の通り `/api/v1/*` クライアント間でヘルパーを
 * 共有しない方針だが、`getCatalog` 自体が管理系エンドポイントへ切り替わった
 * ため、こちらは意図的に `hubStatus.ts` 側の規約（CSRF ヘッダ必須）に
 * 揃える。
 */
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

/**
 * catalog スナップショットを取得する。管理系 `/api/tag-catalog`
 * （`admin_tag_catalog`）を叩く - このファイル冒頭の doc comment
 * 「2026-08-31 オーナー決定」参照。試運転モード・ロックダウン済みの
 * どちらでも同じ呼び出しで動く（`hubStatus.ts` の `getHubStatus` と同じ -
 * 通常の `fetch` はカスタムヘッダを自由に付けられるので、WS と違って
 * モード分岐が要らない）。
 */
export async function getCatalog(): Promise<CatalogResponse> {
	const raw = await httpGet<RawCatalogResponse>('/api/tag-catalog');
	return fromRawCatalog(raw);
}

// --- WS 購読 -----------------------------------------------------------------

export interface TagStreamHandlers {
	onData: (values: StreamValue[]) => void;
	onConfigChanged: (revision: number) => void;
	/**
	 * 接続確立/切断のたびに呼ばれる（画面側が「値が古いかも」を示すため）。
	 * T18-4b: 切断時（`connected: false`）は `CloseEvent.code` を
	 * `closeCode` として渡す - 画面側がバックプレッシャ切断
	 * （`BACKPRESSURE_CLOSE_CODE` = 1013、`stream.rs` 参照）を通常の再接続と
	 * 区別して表示できるようにするため。`connected: true` の呼び出しでは
	 * 意味を持たないので渡さない（undefined でよい）。
	 */
	onStatusChange?: (connected: boolean, closeCode?: number) => void;
}

const RECONNECT_BASE_DELAY_MS = 1000;
const RECONNECT_MAX_DELAY_MS = 30000;
/** トークン未取得（未ログイン等）時の再試行間隔 - `events.ts` の
 * `createSseEventProvider`'s `tokenWaitDelayMs` と同じ発想（再接続の基本
 * 間隔より短くして、ログイン直後にすぐ繋がるようにする）。 */
const TOKEN_WAIT_DELAY_MS = 500;

function wsUrl(path: string): string {
	const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
	return `${scheme}//${location.host}${path}`;
}

/**
 * `/api/v1/stream` への購読を開始する。T18-4b までは常に `tags: ["*"]`
 * 固定だったが、以後は `getSubscriptionTags()`（呼び出し側が
 * `monitorSubscription.ts::subscriptionPatternsFor` 等で組み立てる）の
 * 戻り値を購読対象にする - 画面側のツリー選択に合わせて購読範囲を絞れる
 * ようにするため（`mode` は引き続き常に `"on_change"`）。`getSubscriptionTags`
 * は呼ぶたびに最新の絞り込み結果を返す関数を渡す想定（`$derived` 相当を
 * クロージャで包んだもの）で、このモジュール自身は絞り込みロジックを
 * 持たない。
 *
 * 戻り値は `{ disconnect, resubscribe }`:
 * - `disconnect()`: ソケットを閉じ、保留中の再接続タイマーを止める
 *   （旧 API の戻り値そのもの）。
 * - `resubscribe()`: 購読範囲（`getSubscriptionTags()` の結果）が変わった
 *   ときに呼ぶ。ソケットが開いていれば現在の購読 id を unsubscribe した上で
 *   id をインクリメントして新しい範囲で再 subscribe する。ソケットが
 *   未接続（`ws === null` または `readyState !== OPEN`）なら何もしない -
 *   次に張られるソケットの `onopen` がその時点の `getSubscriptionTags()` の
 *   結果で購読するので、ここで何もしなくても最終的に正しい範囲に収束する
 *   （no-op で十分という設計）。
 *
 * 購読 id はこの関数のクロージャ内で単調増加させる1つの変数として持つ
 * （初期値1）。新しいソケットが繋がった（再接続含む）だけでは増やさない -
 * その時点の id で最初の subscribe を送るだけで、増やすのは
 * `resubscribe()` が unsubscribe→再 subscribe するときだけ。
 *
 * 再接続は `events.ts::createSseEventProvider` と同じ形（`AbortController`
 * の代わりに `WebSocket.close()`、`setTimeout` ベースの指数バックオフ、
 * トークン未取得時は短い間隔で再試行）を踏襲する - このリポジトリで最初の
 * ブラウザ WS クライアントなので、既存の WS 固有の前例はまだない。
 */
export function connectTagStream(
	handlers: TagStreamHandlers,
	getSubscriptionTags: () => string[]
): { disconnect: () => void; resubscribe: () => void } {
	let stopped = false;
	let ws: WebSocket | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let reconnectDelayMs = RECONNECT_BASE_DELAY_MS;
	let subscriptionId = 1;

	function scheduleReconnect(delayMs: number): void {
		if (stopped) return;
		timer = setTimeout(() => connectOnce(), delayMs);
	}

	/**
	 * 接続確立後（`onopen`以降）に共通で使う配線 - 試運転モード・
	 * ロックダウン済みのどちらの接続先（`/api/tag-stream`・
	 * `/api/v1/stream`）でも同一。このファイル冒頭の doc comment
	 * 「WS 購読は難所だった」の分岐部分参照。
	 */
	function attachHandlers(socket: WebSocket): void {
		ws = socket;

		socket.onopen = () => {
			if (stopped) {
				socket.close();
				return;
			}
			reconnectDelayMs = RECONNECT_BASE_DELAY_MS;
			handlers.onStatusChange?.(true);
			socket.send(
				JSON.stringify({
					op: 'subscribe',
					id: subscriptionId,
					tags: getSubscriptionTags(),
					mode: 'on_change'
				})
			);
		};

		socket.onmessage = (event) => {
			if (typeof event.data !== 'string') return;
			let msg: unknown;
			try {
				msg = JSON.parse(event.data);
			} catch {
				return;
			}
			if (typeof msg !== 'object' || msg === null) return;
			const op = (msg as { op?: unknown }).op;
			if (op === 'data') {
				const values = (msg as { values?: unknown }).values;
				if (Array.isArray(values)) handlers.onData(values as StreamValue[]);
			} else if (op === 'config_changed') {
				const revision = (msg as { revision?: unknown }).revision;
				if (typeof revision === 'number') handlers.onConfigChanged(revision);
			}
			// "event"/"pong"/"error" は無視 - `getSubscriptionTags()` は常に
			// `*` かグループワイルドカード（`{connection}.{group}.*`）しか
			// 返さない（`monitorSubscription.ts` 参照、具体名 `Exact` は
			// 使わない）ので unknown_tag 等のユーザー向けエラーは発生しない
			// (subscribe_core.rs: ワイルドカードは0件マッチでもエラーにしない)。
		};

		socket.onclose = (ev: CloseEvent) => {
			if (ws === socket) ws = null;
			handlers.onStatusChange?.(false, ev.code);
			if (stopped) return;
			scheduleReconnect(reconnectDelayMs);
			reconnectDelayMs = Math.min(reconnectDelayMs * 2, RECONNECT_MAX_DELAY_MS);
		};

		socket.onerror = () => {
			// `onclose` は onerror の後に必ず発火する（WebSocket の仕様）ので、
			// 再接続のスケジューリングはそちらだけで行う。
		};
	}

	/**
	 * `sessionStore.commissioningMode`（`$lib/session.svelte.ts` - ルート
	 * ガードが `$lib/banto/commissioning.ts` の判定結果をキャッシュした
	 * もの）を接続のたびに読み直して分岐する。再接続ループの中で毎回
	 * 評価するので、途中でロックダウンが完了した場合も次の接続試行から
	 * 自然に「ロックダウン済み」側の経路（トークン必須）へ切り替わる -
	 * このファイル冒頭の doc comment「WS 購読は難所だった」参照。
	 */
	function connectOnce(): void {
		if (stopped) return;

		if (sessionStore.commissioningMode) {
			// サーバー側（`require_auth_or_commissioning`）は未ロックダウン中
			// 無条件で通すので、トークンもサブプロトコルオファーも不要 -
			// `apps/banto-hub/core/src/rest.rs` の `admin_tag_stream_router`
			// のdoc comment参照。
			attachHandlers(new WebSocket(wsUrl('/api/tag-stream')));
			return;
		}

		// ロックダウン済み: 従来どおり `/api/v1/stream` へ、セッション token
		// を `Sec-WebSocket-Protocol: bearer, <token>` で運ぶ - this module's
		// doc comment / `rest.rs::extract_ws_protocol_token` 参照。
		const token = currentToken();
		if (token === null) {
			scheduleReconnect(TOKEN_WAIT_DELAY_MS);
			return;
		}
		attachHandlers(new WebSocket(wsUrl('/api/v1/stream'), ['bearer', token]));
	}

	connectOnce();

	function disconnect(): void {
		stopped = true;
		if (timer !== null) clearTimeout(timer);
		ws?.close();
		ws = null;
	}

	function resubscribe(): void {
		// ソケット未接続なら no-op - この関数の doc comment 参照（次の
		// onopen が現行の getSubscriptionTags() の結果で購読するので
		// 収束する）。
		if (ws === null || ws.readyState !== WebSocket.OPEN) return;
		ws.send(JSON.stringify({ op: 'unsubscribe', id: subscriptionId }));
		subscriptionId += 1;
		ws.send(
			JSON.stringify({
				op: 'subscribe',
				id: subscriptionId,
				tags: getSubscriptionTags(),
				mode: 'on_change'
			})
		);
	}

	return { disconnect, resubscribe };
}
