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
import {
	classifyStreamClose,
	decideAfterSessionProbe,
	shouldProbeSession,
	type SessionProbeResult,
	type StreamCloseAction
} from './streamClose';

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
	/**
	 * #441: サーバーが資格情報を使えないと確認して close `1008` で閉じた
	 * （`classifyStreamClose` が `reconnect` 以外を返した）ときに 1 回呼ばれる。
	 * このとき**再接続はしない**（止まったまま）。呼び出し側は
	 * `recheckSession` ならログイン状態を確かめ直し、`halt` なら
	 * `action.message` を画面に出す。どちらも、続けてよいと分かったら
	 * 戻り値の `resume()` で購読を再開する。`onStatusChange(false, 1008)` の
	 * 後に呼ばれる。
	 */
	onHalt?: (action: Exclude<StreamCloseAction, { kind: 'reconnect' }>) => void;
	/**
	 * #445: 再接続が続けて失敗したときに、ログイン状態を確かめる（画面の移動は
	 * しない。結果だけを返す）。切れている間にセッションが失効すると、再接続は
	 * 認証で拒否されるがブラウザには `1006` しか見えないため。判断は
	 * `streamClose.ts` の `shouldProbeSession` / `decideAfterSessionProbe`。
	 * 結果が `login` なら再接続をやめ、`onHalt({ kind: 'recheckSession',
	 * reason: 'reconnect_rejected' })` を呼ぶ（`1008` + `session_revoked` と
	 * 同じ経路）。reject は `unverified` と同じ扱い。渡さなければ確かめない
	 * （従来どおり再接続を続ける）。**認証の状態（保存しているトークン）を
	 * 変えないこと**（#447 のレビュー: 見捨てた確認の遅れた `401` が
	 * トークンを消すと、ログイン画面へ移らないまま待ち続ける）。
	 *
	 * なお、トークンで繋いでいたのに再接続の時点でトークンが消えていたら、
	 * 確かめずに `onHalt({ kind: 'recheckSession', reason: 'token_cleared' })`
	 * を呼ぶ（最初の接続の前の未ログインの待ちとは区別する）。
	 */
	probeSession?: () => Promise<SessionProbeResult>;
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
 * 戻り値は `{ disconnect, resubscribe, resume }`:
 * - `disconnect()`: ソケットを閉じ、保留中の再接続タイマーを止める
 *   （旧 API の戻り値そのもの）。
 * - `resume()`（#441）: close `1008` で止まった購読を再開する（すぐに
 *   接続し直し、バックオフも初期値へ戻す）。止まっていないとき・
 *   `disconnect()` の後は何もしない。#445 の確認（再接続が続けて失敗し、
 *   `probeSession` が `login` を返した）で止まったときも同じ。
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
): { disconnect: () => void; resubscribe: () => void; resume: () => void } {
	let stopped = false;
	/**
	 * #441: close `1008` で止まっている（再接続しない）。`resume()` で戻る。
	 * #445: 再接続の失敗からの確認で失効が分かったときも止まる。
	 */
	let halted = false;
	let ws: WebSocket | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let reconnectDelayMs = RECONNECT_BASE_DELAY_MS;
	let subscriptionId = 1;
	/** #445: 開く前に閉じた（= 失敗した）再接続が続けて何回あったか。`onopen` で 0 に戻す。 */
	let consecutiveFailures = 0;
	/** #445: ログイン状態の確認が飛行中（同じストリームから重ねて起こさない）。 */
	let probing = false;
	/**
	 * #445: 状態の世代。接続が開いた・`resume()`・`disconnect()` で進める。
	 * 確認を始めたときと世代が違えば、返ってきた結果は捨てる（飛行中の
	 * 応答が、その後に開いた接続などの新しい状態を巻き戻さないように）。
	 */
	let generation = 0;
	/**
	 * #445（#447 のレビュー）: ロックダウン済みで最後に繋いだトークン。次の
	 * 再接続でトークンが消えていたら、未ログインの待ちではなく確認へ進む。
	 */
	let usedToken: string | null = null;

	function scheduleReconnect(delayMs: number): void {
		if (stopped) return;
		timer = setTimeout(() => connectOnce(), delayMs);
	}

	/**
	 * #445: 再接続が続けて失敗したので、ログイン状態を確かめる。再接続の待ち
	 * （バックオフ）とは並行に走らせ、待ちは変えない。判断は
	 * `decideAfterSessionProbe`（`streamClose.ts` の表）。
	 */
	function probeSessionAfterFailures(): void {
		const probe = handlers.probeSession;
		if (probe === undefined || probing) return;
		probing = true;
		const startedAt = generation;
		const failuresAtStart = consecutiveFailures;
		probe()
			.catch((): SessionProbeResult => 'unverified')
			.then((result) => {
				probing = false;
				if (stopped || halted) return;
				if (generation !== startedAt) {
					// 確認の最中に接続が開いた・再開した: 結果は古い状態についての
					// もの。捨てる。ただし新しい世代で数えた失敗がすでに確認に値する
					// なら（確認中だったので起こせなかった）、いま確かめる。
					if (shouldProbeSession(consecutiveFailures)) probeSessionAfterFailures();
					return;
				}
				const step = decideAfterSessionProbe(result, failuresAtStart, consecutiveFailures);
				if (step.kind === 'reconnect') {
					consecutiveFailures = step.consecutiveFailures;
					// 確認の最中に起きた失敗を、確認の前の結果で打ち消さない。
					if (step.probeAgain) probeSessionAfterFailures();
					return;
				}
				// 失効を確認できた: 再接続をやめ、`1008` + `session_revoked` と
				// 同じ確認の経路（`onHalt` → ルートガード）へ合流する。
				haltForRecheck(step.reason);
			});
	}

	/**
	 * #445: 再接続をやめて、ログイン状態の確認（`onHalt` の `recheckSession`
	 * → ルートガード）へ進む。
	 */
	function haltForRecheck(reason: 'reconnect_rejected' | 'token_cleared'): void {
		halted = true;
		if (timer !== null) {
			clearTimeout(timer);
			timer = null;
		}
		// 開く途中のソケットがあれば捨てる（開いていれば世代が進んで
		// ここへは来ない）。後から届く close で再接続を起こさないよう、
		// 先にハンドラを外す（`resume()` の後に届いても二重に張らない）。
		const pending = ws;
		ws = null;
		if (pending !== null) {
			pending.onopen = null;
			pending.onmessage = null;
			pending.onclose = null;
			pending.onerror = null;
			pending.close();
		}
		handlers.onHalt?.({ kind: 'recheckSession', reason });
	}

	/**
	 * 接続確立後（`onopen`以降）に共通で使う配線 - 試運転モード・
	 * ロックダウン済みのどちらの接続先（`/api/tag-stream`・
	 * `/api/v1/stream`）でも同一。このファイル冒頭の doc comment
	 * 「WS 購読は難所だった」の分岐部分参照。
	 */
	function attachHandlers(socket: WebSocket): void {
		ws = socket;
		/** #445: このソケットが開いたか（開く前に閉じたら再接続の失敗として数える）。 */
		let opened = false;

		socket.onopen = () => {
			if (stopped) {
				socket.close();
				return;
			}
			opened = true;
			consecutiveFailures = 0;
			generation += 1;
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
			// #441: 資格情報が使えないと確認された close（`1008`）は再接続
			// しない - 失効したセッションでは再接続が認証で拒否され続ける
			// だけで、利用者に理由も伝わらない。判断は `streamClose.ts`。
			const action = classifyStreamClose(ev.code, ev.reason);
			if (action.kind !== 'reconnect') {
				halted = true;
				handlers.onHalt?.(action);
				return;
			}
			// #445: 開く前に閉じた = 再接続の失敗。ブラウザには拒否の理由が
			// 見えない（`1006`）ので、続けば失効を疑ってログイン状態を確かめる。
			if (!opened) consecutiveFailures += 1;
			scheduleReconnect(reconnectDelayMs);
			reconnectDelayMs = Math.min(reconnectDelayMs * 2, RECONNECT_MAX_DELAY_MS);
			if (shouldProbeSession(consecutiveFailures)) probeSessionAfterFailures();
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
			if (usedToken !== null) {
				// #445（#447 のレビュー）: トークンで繋いでいたのに、再接続の
				// 時点でトークンが消えている（ほかの経路が `401` で消した・
				// ほかのタブでログアウトした）。未ログインの待ち（最初の接続の
				// 前）と違い、待ち続けても誰もログイン状態を確かめないので、
				// 確認へ進む。
				usedToken = null;
				haltForRecheck('token_cleared');
				return;
			}
			scheduleReconnect(TOKEN_WAIT_DELAY_MS);
			return;
		}
		usedToken = token;
		attachHandlers(new WebSocket(wsUrl('/api/v1/stream'), ['bearer', token]));
	}

	connectOnce();

	function disconnect(): void {
		stopped = true;
		generation += 1;
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

	function resume(): void {
		if (stopped || !halted) return;
		halted = false;
		reconnectDelayMs = RECONNECT_BASE_DELAY_MS;
		consecutiveFailures = 0;
		generation += 1;
		connectOnce();
	}

	return { disconnect, resubscribe, resume };
}
