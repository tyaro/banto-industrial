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
	/** 接続確立/切断のたびに呼ばれる（画面側が「値が古いかも」を示すため）。 */
	onStatusChange?: (connected: boolean) => void;
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
 * `/api/v1/stream` への購読を開始する（常に `tags: ["*"]`・
 * `mode: "on_change"` - このページはフィルタを画面側で行うので購読自体は
 * 全タグを対象にする）。戻り値は破棄関数（ソケットを閉じ、保留中の再接続
 * タイマーを止める）。
 *
 * 再接続は `events.ts::createSseEventProvider` と同じ形（`AbortController`
 * の代わりに `WebSocket.close()`、`setTimeout` ベースの指数バックオフ、
 * トークン未取得時は短い間隔で再試行）を踏襲する - このリポジトリで最初の
 * ブラウザ WS クライアントなので、既存の WS 固有の前例はまだない。
 */
export function connectTagStream(handlers: TagStreamHandlers): () => void {
	let stopped = false;
	let ws: WebSocket | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let reconnectDelayMs = RECONNECT_BASE_DELAY_MS;

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
			socket.send(JSON.stringify({ op: 'subscribe', id: 1, tags: ['*'], mode: 'on_change' }));
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
			// "event"/"pong"/"error" は無視 - このページは常に `*` を購読する
			// ので unknown_tag 等のユーザー向けエラーは発生しない
			// (subscribe_core.rs: ワイルドカードは0件マッチでもエラーにしない)。
		};

		socket.onclose = () => {
			if (ws === socket) ws = null;
			handlers.onStatusChange?.(false);
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

	return () => {
		stopped = true;
		if (timer !== null) clearTimeout(timer);
		ws?.close();
		ws = null;
	};
}
