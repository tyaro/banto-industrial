/**
 * Client for the 収集ランタイムの口（#383 段階2b / R1-C。口は C-1〜C-3a =
 * #406/#407/#408 で入っており、**この層は新しい API を足さない**）。
 *
 * `hubAdmin.ts` / `auditLogAdmin.ts` / `tagRegistryAdmin.ts` と同じ Tauri/REST
 * 分岐: Tauri webview は `invoke()`（`collect_*` コマンド、
 * `apps/chronogazer/src-tauri/src/lib.rs`）、組み込みサーバー配下の LAN
 * ブラウザは `fetch()`（`/api/collect*`、`apps/chronogazer/core/src/rest.rs`）。
 * どちらも同じ `chronogazer_core::collect::CollectorService` を通るので、
 * 床も形も経路で割れない。
 *
 * プレーンな `vite dev`/`vite preview`（Rust backend 無し）では収集ランタイム
 * そのものが無いので、すべて `DEMO_MODE_MESSAGE` で reject する
 * （`hubAdmin.ts` の `isHubAvailable()`/`demoModeError()` と同じ作法）。
 *
 * **床（両経路で同じ）**:
 * - 読み取り（`collect_status` / `collect_connections` / `collect_events_list`）
 *   = **viewer 以上**（`chronogazer_core::collect::COLLECT_READ_ROLE`）。
 * - 操作（`collect_start` / `collect_stop` / `collect_restart`）= **editor
 *   以上**（`COLLECT_OPERATION_ROLE`）。画面は editor 未満に操作を**出さない**
 *   （`routes/(app)/tags/+page.svelte` と同じ「見せない」方針）。
 *
 * **理由（`reason`）は状態に載っていない**: `collect_status` が返すのは公開用の
 * [`CollectorStateView`] で、`startFailed` に理由が無い（#407 レビュー P2-2 -
 * 状態の読み取りは viewer にも開いているため、接続先やパスを含みうる文言を
 * 載せない）。**理由が利用者に届く唯一の経路は「操作したとき」**で、
 * - 開始に失敗したら `collect_start` が `Err`（文言に理由が入る）、
 * - 失敗した状態のまま停止すると [`CollectOutcome::status`] が
 *   `startFailed` + `reason`（こちらは内部型の [`CollectorState`]。受け取れる
 *   のは editor 以上 = 操作した本人だけ）。
 * この 2 つを画面へ運ぶのが [`collectOperationDisplay`] で、`reason` を
 * **操作の結果としてだけ**出す。
 *
 * ポーリング・上限まわりの純関数（[`runWithLimit`] ほか）は `hubAdmin.ts` の
 * ものを**そのまま再利用**する（#332/#383 段階1 のレビューで作り込んだもので、
 * 「打ち切ったあとに遅れて解決した応答を採用しない」という**同じ判断**を 2 つ
 * 書き写すと片方だけ直す事故が起きる）。ここでは収集の画面が要る分だけ
 * 再エクスポートして、画面側の import 元を 1 つに保つ。
 */
import { invoke } from '@tauri-apps/api/core';
import type { ListResult } from '@banto/admin-core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';
import {
	isPollGenerationCurrent,
	isPollResultFresh,
	nextPollFailureCount,
	runWithLimit,
	type RunWithLimitOutcome
} from './hubAdmin';

export {
	isPollGenerationCurrent,
	isPollResultFresh,
	nextPollFailureCount,
	runWithLimit,
	type RunWithLimitOutcome
};

// --- ワイヤ型（Rust 側の綴りをそのまま写す） --------------------------------

/**
 * `chronogazer_core::collect::CollectorStateView` の判別共用体（serde の
 * `{"state": ...}` 形）。
 *
 * **5 状態をそのまま見せる**のが画面の役目で、`stopped`（止めた）/
 * `noTargets`（収集対象が無い）/ `startFailed`（起動を試みて失敗した）を
 * 1 つに潰さない - どれも「動いていない」だが、次の一手が違う。
 *
 * **`startFailed` に理由は無い**（このモジュールの doc 参照）。
 */
export type CollectorStateView =
	| { state: 'stopped' }
	| { state: 'starting' }
	| { state: 'running'; groups: number; tags: number }
	| { state: 'noTargets' }
	| { state: 'startFailed' };

/**
 * `chronogazer_core::collect::CollectorState`（**内部型**）。
 *
 * [`CollectorStateView`] と同じ 5 状態・同じ綴りで、違いは **`startFailed` に
 * `reason` が付く**ことだけ。これを受け取れるのは [`CollectOutcome`]
 * （= editor 以上の操作の戻り値）経由だけで、状態の読み取り
 * （[`getCollectStatus`]）には載らない。
 */
export type CollectorState =
	| { state: 'stopped' }
	| { state: 'starting' }
	| { state: 'running'; groups: number; tags: number }
	| { state: 'noTargets' }
	| { state: 'startFailed'; reason: string };

/**
 * `chronogazer_core::collect::CollectOutcome`。
 *
 * **`pending` は「失敗」ではない**: `true` は「上限まで待ったが、まだ終わって
 * いない」で、依頼は**確かにキューに入っている**（後から必ず実行される）。
 * 混雑で入れられなかったときは `pending` ではなく**エラー**が返る - その文言
 * には「この操作は実行されていません」が入る（`collect.rs` の
 * `ENQUEUE_BUSY_MESSAGE`）。画面はこの 2 つを混ぜない
 * （[`collectOperationDisplay`]）。
 */
export interface CollectOutcome {
	status: CollectorState;
	pending: boolean;
}

/**
 * `chronogazer_core::collect::Readout<T>`。読み出し 1 回の結末。
 *
 * 「**走っていない**」「**読めなかった**」「**読めて 0 件**」を別の値にする
 * ためだけの型（docs/implementation-checklist.md §5「エラーを空に潰さない」）。
 * **画面はこの 3 つを別々に見せる** - `unavailable` を「0 件」や「接続なし」に
 * 潰さない。
 */
export type Readout<T> =
	{ state: 'notRunning' } | { state: 'unavailable' } | { state: 'ready'; data: T };

/** [`Readout`] の 3 つのタグ（純関数の入力にするために名前を付けたもの）。 */
export type ReadoutState = Readout<unknown>['state'];

/** `chronogazer_core::collect::ConnectionStatusView`。 */
export type ConnectionStatusView =
	{ status: 'connected' } | { status: 'reconnecting'; attempt: number } | { status: 'stopped' };

/**
 * `chronogazer_core::collect::ConnectionView`（#413）: 接続の状態に
 * `simulation` を添えた形（JSON は平たく並ぶ）。
 *
 * **`simulation` は走っている収集が起動時に使った値**で、レジストリ
 * （`/tags` の接続一覧）の今の値ではない - 切替は「収集を再起動」まで
 * 反映されないので、両者は一時的に食い違いうる。`true` の接続の値は現在値・
 * イベントには出るが、**データファイルには記録されない**。
 */
export type ConnectionView = ConnectionStatusView & { simulation: boolean };

/**
 * `chronogazer_core::collect::CollectEventRow`。
 *
 * **`detail` 列は無い**（C-3a が型でも SQL でも落とした。自由文で、切断理由や
 * 書き込み先のファイルパスを含みうるため）。画面は**無い列を作らない** -
 * 切断理由が要ると判断したら、発生源で「見せてよい理由」を分類するのが筋。
 */
export interface CollectEventRow {
	id: number;
	/** UTC epoch ミリ秒（`collect_events.ts`）。 */
	tsMs: number;
	kind: string;
	/** `conn:<id>`。収集エンジン全体のイベントでは `null`。 */
	connectionKey: string | null;
	/** `tag:<id>`。`threshold_*` だけ非 `null`。 */
	tagKey: string | null;
	/** `H`/`HH`/`L`/`LL`（`threshold_*` だけ）。 */
	level: string | null;
	value: number | null;
}

/**
 * `chronogazer_core::collect::CollectEventList`。イベント一覧 1 ページ分の
 * 応答。
 *
 * `rows`/`totalCount` は監査ログ一覧（`ListResult`）と同じ綴りで、そこに
 * **この応答が使ったスナップショット境界** `asOfId` が 1 つ足してある
 * （#409 レビュー P2-2）。件数も行も `id <= asOfId` で絞られている。
 * **表が空のときは `0`**（`collect_events.id` は 1 から振られるので、
 * 「どの行も含まない境界」）。
 */
export interface CollectEventList extends ListResult<CollectEventRow> {
	asOfId: number;
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is this environment backed by a real collector (Tauri or the embedded server)? */
export function isCollectAvailable(): boolean {
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

/** Same type guard as hubAdmin.ts / auditLogAdmin.ts. */
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

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

/** Same token lookup as hubAdmin.ts / auditLogAdmin.ts. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

function authHeaders(): Record<string, string> {
	const headers: Record<string, string> = { ...CSRF_HEADER };
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;
	return headers;
}

async function errorFromResponse(response: Response): Promise<ProviderError> {
	let body: unknown;
	try {
		body = await response.json();
	} catch {
		return new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}
	if (isErrorBody(body)) return new ProviderError(body);
	return new ProviderError({ kind: 'other', message: `${response.status} ${response.statusText}` });
}

/**
 * `signal` の効き方は `hubAdmin.ts` と同じ非対称: REST は実際にソケットを
 * 畳み、Tauri の `invoke` には中断の口が無いので呼び出し側の「打ち切り済み」
 * フラグ（[`runWithLimit`]）でしか守れない。
 */
async function httpJson<T>(path: string, method: string, signal?: AbortSignal): Promise<T> {
	let response: Response;
	try {
		response = await fetch(path, { method, headers: authHeaders(), signal });
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}
	if (!response.ok) throw await errorFromResponse(response);
	return (await response.json()) as T;
}

// --- 読み取り（viewer 以上） ------------------------------------------------

/**
 * 収集の状態（`viewer` 以上）。**ネットワークもディスクも DB もキューも
 * 通らない**ので、画面はこれをポーリングしてよい。
 */
export async function getCollectStatus(signal?: AbortSignal): Promise<CollectorStateView> {
	if (!isCollectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<CollectorStateView>('collect_status');
	return httpJson<CollectorStateView>('/api/collect', 'GET', signal);
}

/**
 * 接続ごとの状態（`viewer` 以上）。**3 つの結末すべてを返しうる唯一の口**
 * （走っていない / 読めなかった / 読めて 0 件）。
 */
export async function getCollectConnections(
	signal?: AbortSignal
): Promise<Readout<Record<string, ConnectionView>>> {
	if (!isCollectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<Readout<Record<string, ConnectionView>>>('collect_connections');
	return httpJson<Readout<Record<string, ConnectionView>>>(
		'/api/collect/connections',
		'GET',
		signal
	);
}

/**
 * `collect_events` の 1 ページ（**新しい順**、`viewer` 以上）。
 *
 * 総件数は `totalCount`（監査ログ一覧の `ListResult` と同じ綴り）。
 * `limit` はサーバー側で `1..=500` に丸められる（`?limit=` は URL に誰でも
 * 書けるため）。**`notRunning` は返らない** - 過去の記録なので、収集が
 * 止まっていても読めなければ意味が無い。
 *
 * **`asOfId` = スナップショット境界**（#409 レビュー P2-2）。`null` を渡すと
 * サーバーが**その時点の最大 `id`** を境界にして、使った境界を
 * [`CollectEventList.asOfId`] で返す。画面は 1 つの「世代」の最初の応答で
 * それを固定し、同じ世代の後続ブロックにすべて渡す - 渡さないと、ブロック
 * 取得の合間に足されたイベントで `offset` がずれ、**境界で行が重複し、末尾の
 * 行が一覧から漏れる**（判断は `routes/(app)/events/eventBlocks.ts`）。
 */
export async function listCollectEvents(
	offset: number,
	limit: number,
	asOfId: number | null,
	signal?: AbortSignal
): Promise<Readout<CollectEventList>> {
	if (!isCollectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<Readout<CollectEventList>>('collect_events_list', {
			offset,
			limit,
			asOfId
		});
	const query = new URLSearchParams({ offset: String(offset), limit: String(limit) });
	if (asOfId !== null) query.set('asOfId', String(asOfId));
	return httpJson<Readout<CollectEventList>>(`/api/collect/events?${query}`, 'GET', signal);
}

// --- 操作（editor 以上） ----------------------------------------------------

/** 収集の操作 3 種。画面の文言もキーもこの 3 つで引く。 */
export type CollectAction = 'start' | 'stop' | 'restart';

const ACTION_PATH: Record<CollectAction, string> = {
	start: '/api/collect/start',
	stop: '/api/collect/stop',
	restart: '/api/collect/restart'
};

const ACTION_COMMAND: Record<CollectAction, string> = {
	start: 'collect_start',
	stop: 'collect_stop',
	restart: 'collect_restart'
};

/**
 * 収集の開始・停止・再起動（**`editor` 以上**）。
 *
 * **打ち切ってもアプリ側の処理は止まらない**（`hubAdmin.ts` の
 * [`runWithLimit`] と同じ性質）。戻り値の `pending` も「打ち切ったが依頼は
 * 入っている」であって失敗ではない - 言い分けは [`collectOperationDisplay`]。
 */
export async function runCollectAction(
	action: CollectAction,
	signal?: AbortSignal
): Promise<CollectOutcome> {
	if (!isCollectAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<CollectOutcome>(ACTION_COMMAND[action]);
	return httpJson<CollectOutcome>(ACTION_PATH[action], 'POST', signal);
}

// --- ここから下はすべて純関数（collectAdmin.test.ts が総当たりで固定する） ---

/**
 * 5 状態の見出し（純関数）。**どれか 2 つが同じ表示に潰れない**ことがこの
 * 関数の要点で、とくに `stopped` / `noTargets` / `startFailed` は「動いて
 * いない」点だけが同じで原因も次の一手も違う。
 */
export function collectStateLabel(state: CollectorStateView): string {
	switch (state.state) {
		case 'stopped':
			return '停止';
		case 'starting':
			return '開始中';
		case 'running':
			return `収集中（グループ${state.groups}件 / タグ${state.tags}件）`;
		case 'noTargets':
			return '収集対象がありません';
		case 'startFailed':
			return '開始に失敗しました';
	}
}

/**
 * 状態ごとの補足説明（次に何をすればよいか。純関数）。
 *
 * **`startFailed` で「理由」を出そうとしない** - 状態には載っていない
 * （このモジュールの doc）。代わりに**どうすれば理由が見られるか**を書く:
 * 操作すると理由が返る、という事実をそのまま案内する。
 */
export function collectStateDetail(state: CollectorStateView): string {
	switch (state.state) {
		case 'stopped':
			return '収集は動いていません。「収集を開始」で始められます。';
		case 'starting':
			return '開始処理が進んでいます。終わると状態が変わります。';
		case 'running':
			return 'タグの設定を変えたときは「収集を再起動」で反映します（自動では反映されません）。';
		case 'noTargets':
			return '有効なタグが1件もないため、収集するものがありません（失敗ではありません）。「タグ設定」でタグを登録・有効化してから「収集を再起動」を押してください。';
		case 'startFailed':
			return '前回の開始が失敗したままです。失敗した理由はこの状態表示には含まれません - 「収集を開始」を押すと、その結果として理由が表示されます。';
	}
}

/**
 * 操作の応答に載る内部状態（[`CollectorState`]）を、表示用の
 * [`CollectorStateView`] に落とす（純関数）。**落とすのは `reason` だけ**で、
 * 理由は [`startFailedReason`] が別に取り出す - 文言を組み立てる関数が
 * 「状態の名前」と「理由」を別々に扱えるようにするため。
 */
export function toCollectStateView(state: CollectorState): CollectorStateView {
	return state.state === 'startFailed' ? { state: 'startFailed' } : state;
}

/** 操作の応答に理由が載っていれば取り出す（純関数）。無ければ `null`。 */
export function startFailedReason(state: CollectorState): string | null {
	return state.state === 'startFailed' ? state.reason : null;
}

/** 操作ボタンの文言（純関数）。画面・通知・E2E がこの 1 か所を引く。 */
export function collectActionLabel(action: CollectAction): string {
	switch (action) {
		case 'start':
			return '収集を開始';
		case 'stop':
			return '収集を停止';
		case 'restart':
			return '収集を再起動';
	}
}

/**
 * 画面→アプリの**明示操作**（開始・停止・再起動）1 回に許す上限（ms）。
 *
 * `hubAdmin.ts` の `HUB_UI_TIMEOUT_MS` と同じ趣旨（上限が無いと、応答しない
 * アプリに当たったとき `busy` が降りず画面が操作不能になる）で、**値の根拠は
 * こちらの backend に合わせて別に置く**:
 *
 * - `chronogazer_core::collect` の `COLLECT_ENQUEUE_TIMEOUT` = 5 秒
 *   （キューへの投入）と `COLLECT_OPERATION_TIMEOUT` = 30 秒（投入後の完了
 *   待ち）で、**正当に遅い最長は 5 + 30 = 35 秒**。それを超えたら backend 自身が
 *   エラーか `pending` で必ず戻る。
 * - つまり 60 秒まで何も返らないのは「遅い」ではなく「**アプリが応答して
 *   いない**」。Hub の 90 秒をそのまま使わないのは、待っている相手（ローカル
 *   I/O だけ。PLC への接続は別タスク）が違うため。
 *
 * **打ち切りは失敗ではない**（[`collectOperationDisplay`] の `timedOut`）。
 */
export const COLLECT_UI_TIMEOUT_MS = 60000;

/**
 * 読み取り 1 回（状態・接続ごとの状態・イベント一覧）に許す上限（ms）。
 *
 * `hubAdmin.ts` の `SUBSCRIPTION_POLL_TIMEOUT_MS` と同じ理由で必要:
 * **これが無いと連続失敗を数える仕組みも、ポーリングのループごと止まる**
 * （`invoke` にも `fetch` にも上限が無く、TCP は繋がるが応答が返らない相手
 * では `catch` に入らない）。
 *
 * **なぜ 4 秒か**: backend 側の読み取りは
 * `chronogazer_core::collect::COLLECT_READ_TIMEOUT` = 2 秒で必ず打ち切られて
 * `unavailable` を返す（状態の読み取りに至ってはキューすら通らない）。
 * 2 倍の 4 秒まで何も返らないのは、backend が答えたのに**届いていない**
 * ということ。ポーリング間隔（2 秒）の 2 周期ぶんでもある。
 */
export const COLLECT_READ_TIMEOUT_MS = 4000;

/**
 * 読み取りが「恒久的に失敗している」と見なす連続失敗回数（成功で 0 に戻る。
 * `hubAdmin.ts` の `SUBSCRIPTION_POLL_FAILURE_LIMIT` と同じ 2 で、理由も同じ）。
 *
 * **1 回では切り替えない**（スリープ復帰直後の取りこぼしで表示を揺らさない）。
 * 増やしすぎると、サーバープロセスが落ちたあとも古い状態を出し続ける時間が
 * 延びる。
 */
export const COLLECT_POLL_FAILURE_LIMIT = 2;

/** 表示が「今の状態ではない」（取得できていない）か（純関数）。 */
export function isCollectStale(consecutiveFailures: number): boolean {
	return consecutiveFailures >= COLLECT_POLL_FAILURE_LIMIT;
}

/**
 * 状態の見出し（純関数）。取得できていない間も**状態名は残す**（5 状態と
 * 1 対 1 でなくなるので 6 つ目の状態名を作らない）。添えるのは「取得できて
 * いない」という事実だけ。
 */
export function collectStateHeadline(state: CollectorStateView, stale: boolean): string {
	const label = collectStateLabel(state);
	return stale ? `${label}（状態を取得できていません）` : label;
}

/** 取得できていない表示の種類（文言を出し分けるためだけの型）。 */
export type CollectPollSubject = 'status' | 'connections';

/**
 * 取得できていない間に添える一文（純関数）。
 *
 * **いつの表示なのか**を必ず出す - 時刻が無いまま古い表示だけを残すと、
 * それが今の状態に見えてしまう。**表は消さない**（消すと「0 件」に潰れて
 * 別の嘘になる）ので、このひと言が唯一の断り書きになる。
 */
export function collectStaleNote(subject: CollectPollSubject, lastPolledAt: number | null): string {
	const what = subject === 'status' ? '収集の状態' : '接続ごとの状態';
	if (lastPolledAt === null) {
		return `${what}を取得できていません（まだ一度も取得できていません）。`;
	}
	return `${what}を取得できていません。下の表示は${collectTimeLabel(lastPolledAt)}に取得したもので、最新ではありません。`;
}

/**
 * epoch ミリ秒の表示（純関数）。
 *
 * 表示は閲覧している端末のロケール・タイムゾーンに任せる（`hubAdmin.ts` の
 * `hubTimeLabel` と同じ判断。この画面にも揃えるべき独自の時刻書式は無い）。
 * 別モジュールの Hub 用の関数を収集の画面から呼ぶより、同じ 2 行をここに
 * 置くほうが依存が素直（判断ではなく書式なので、食い違っても害が無い）。
 */
export function collectTimeLabel(epochMs: number): string {
	const at = new Date(epochMs);
	return Number.isNaN(at.getTime()) ? String(epochMs) : at.toLocaleString();
}

/**
 * 接続ごとの状態ブロックの見出し文（純関数）。
 *
 * **3 つを別々に見せる**のがこの関数の全部（docs/implementation-checklist.md
 * §5）: 「走っていない」「読めなかった」「読めて 0 件」を同じ文言にしない。
 * とくに `unavailable` を「接続なし」や「0 件」に潰さない - 次のポーリングで
 * 読めるかもしれない、という別の事実だから。
 */
export function collectConnectionsNote(state: ReadoutState, count: number): string {
	switch (state) {
		case 'notRunning':
			return '収集が動いていないため、接続ごとの状態はありません。';
		case 'unavailable':
			return '接続ごとの状態を読み取れませんでした（失敗ではありません - 収集が取り込み中などで応答できなかった可能性があります）。次の取得で読めることがあります。';
		case 'ready':
			return count === 0
				? '収集は動いていますが、接続は1件もありません。'
				: `${count}件の接続があります。`;
	}
}

/** 接続 1 件の状態の表示（純関数）。`attempt` は落とさない（ヘルスの実用情報）。 */
export function connectionStatusLabel(status: ConnectionStatusView): string {
	switch (status.status) {
		case 'connected':
			return '接続中';
		case 'reconnecting':
			return `再接続中（${status.attempt}回目）`;
		case 'stopped':
			return '停止';
	}
}

/**
 * シミュレーション接続の注記（#413）。**走っている収集が**その接続を
 * シミュレータ相手に動かしているときだけ出す（`ConnectionView` の doc）。
 * 「値は記録されません」は `banto-collect` の約束で、
 * `apps/chronogazer/core/tests/simulation_not_recorded.rs` が固定している。
 */
export const SIMULATION_RUNNING_NOTE = 'シミュレーション中（値は記録されません）';

/** 接続の行に添える注記（純関数）。実機相手なら `null`（何も添えない）。 */
export function connectionSimulationNote(view: ConnectionView): string | null {
	return view.simulation ? SIMULATION_RUNNING_NOTE : null;
}

/**
 * イベント一覧の件数・状態の一文（純関数）。
 *
 * ここでも `unavailable` を**空一覧に潰さない**。`notRunning` は
 * `collect_events_list` からは返らないが、型としては [`Readout`] なので
 * 分岐を残す（返らない値のために嘘の文言を書かず、「読み取れませんでした」と
 * 同じ扱いにもしない）。
 */
export function collectEventsNote(state: ReadoutState, totalCount: number): string {
	switch (state) {
		case 'notRunning':
			return '収集が動いていないため、イベントを読み取れませんでした。';
		case 'unavailable':
			return 'イベントを読み取れませんでした（0件ではありません）。「再読み込み」でもう一度試せます。';
		case 'ready':
			return totalCount === 0
				? 'イベントはまだ1件も記録されていません。'
				: `${totalCount.toLocaleString()}件の記録があります（新しい順）。`;
	}
}

/**
 * 明示操作 1 回の結末を、画面に出す 2 行へ落とす（純関数。総当たりで固定する）。
 *
 * **3 つを混ぜない**のがこの関数の要点:
 *
 * | 結末 | 意味 | 出し方 |
 * | --- | --- | --- |
 * | `failed` | 相手がエラーを返した。混雑での**未受付**もここ（backend の文言に「この操作は実行されていません」が入る） | `error`。**`pending` と混ぜない** |
 * | `timedOut` | **こちらが待つのをやめただけ**。処理は続いているかもしれない | `notice`。「失敗しました」と言わない |
 * | `ok` + `pending` | 依頼は**確かにキューに入った**が、まだ終わっていない | `notice`。「完了しました」と言わない |
 * | `ok` + `!pending` | 終わった | `notice`（現在の状態を添える） |
 *
 * `errorText` を呼び出し側から受け取るのは、`errorMessage()`（`ProviderError`
 * の文言化）を通した結果をそのまま出しつつ、この関数を純粋に保つため
 * （`hubAdmin.ts` の `reconfirmStatusFailureNotice` と同じ作法）。
 *
 * **`ok` のときだけ理由が出る**: [`CollectOutcome::status`] は内部型なので、
 * 失敗した状態のまま停止・再起動したときに `startFailed` の `reason` が
 * 載ってくる。状態表示には無い情報なので、ここで必ず見せる。
 */
export interface CollectOperationDisplay {
	/** 受け付けた・完了した・打ち切ったときの文言（`null` = 出さない）。 */
	notice: string | null;
	/** エラーとして出す文言（`null` = 出さない）。 */
	error: string | null;
}

export function collectOperationDisplay(
	action: CollectAction,
	outcome: RunWithLimitOutcome<CollectOutcome>,
	errorText: string | null
): CollectOperationDisplay {
	const label = collectActionLabel(action);
	if (outcome.kind === 'failed') {
		// backend の文言をそのまま出す。「実行されていません」（混雑）なのか
		// 「試して失敗した」（開始の失敗）なのかは backend の文言が言い分けて
		// いるので、ここで勝手にどちらかへ丸めない。
		return { notice: null, error: `「${label}」: ${errorText ?? '理由不明'}` };
	}
	if (outcome.kind === 'timedOut') {
		const seconds = Math.round(COLLECT_UI_TIMEOUT_MS / 1000);
		return {
			notice: `「${label}」の応答が${seconds}秒以内に返りませんでした。待つのをやめただけなので、操作は続いている可能性があります。結果は下の状態表示が追いつきます。`,
			error: null
		};
	}
	const { status, pending } = outcome.value;
	if (pending) {
		return {
			notice: `「${label}」を受け付けました。まだ終わっていません - 結果は下の状態表示が追いつきます。`,
			error: null
		};
	}
	const reason = startFailedReason(status);
	const stateLabel = collectStateLabel(toCollectStateView(status));
	const suffix = reason === null ? '' : ` 開始に失敗した理由: ${reason}`;
	return {
		notice: `「${label}」が完了しました（現在の状態: ${stateLabel}）。${suffix}`,
		error: null
	};
}
