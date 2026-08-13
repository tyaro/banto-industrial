/**
 * ライブタグモニタ（T10、docs/ux-plan.md §2）のクライアント。
 *
 * catalog（`GET /api/v1/tags`）は `hubStatus.ts` と同じ小さな `httpGet`
 * ヘルパーをこのファイル内に複製する - `hubStatus.ts` 冒頭の doc comment
 * が明示する通り、このコードベースでは `/api/v1/*` クライアント間で
 * ヘルパーを共有せず、各ファイルが自己完結する方針（既存の慣例）。
 *
 * **注意: `/api/v1/*` の応答は snake_case**（`apps/banto-hub/core/src/hub.rs`
 * の `TagEntry`・`apps/banto-hub/core/src/rest.rs` の `CatalogResponse` 参照）。
 *
 * WS 購読（`connectTagStream`）はブラウザの `WebSocket` で
 * `/api/v1/stream` へ接続する。ブラウザの `WebSocket` コンストラクタは
 * `Authorization` ヘッダを送れない（ブラウザ API の制約）ため、
 * `apps/banto-hub/core/src/rest.rs` の `extract_ws_protocol_token` が
 * 受け付ける `Sec-WebSocket-Protocol: bearer, <token>` 方式で認証する -
 * `new WebSocket(url, ['bearer', token])` と書くと、ブラウザが自動的に
 * この形式のヘッダを送る（トークンが URL やクエリ文字列に出ないので、
 * サーバーのアクセスログやブラウザ履歴に残らない）。
 */
import { getAuthProvider, ProviderError, type ErrorBody } from '@banto/admin-core';

/** `GET /api/v1/tags` の1タグ分（`apps/banto-hub/core/src/hub.rs::TagEntry` と同型）。 */
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

async function httpGet<T>(path: string): Promise<T> {
	const headers: Record<string, string> = {};
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

export async function getCatalog(): Promise<CatalogResponse> {
	return httpGet<CatalogResponse>('/api/v1/tags');
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

function streamUrl(): string {
	const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
	return `${scheme}//${location.host}/api/v1/stream`;
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

	function connectOnce(): void {
		if (stopped) return;
		const token = currentToken();
		if (token === null) {
			scheduleReconnect(TOKEN_WAIT_DELAY_MS);
			return;
		}

		// `Sec-WebSocket-Protocol: bearer, <token>` - this module's doc
		// comment / `rest.rs::extract_ws_protocol_token` 参照。
		const socket = new WebSocket(streamUrl(), ['bearer', token]);
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
