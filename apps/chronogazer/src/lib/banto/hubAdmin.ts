/**
 * Client for the `admin`-only banto-hub 接続 API（#332 chronogazer 分）。
 *
 * `usersAdmin.ts`/`auditLogAdmin.ts`/`backupsAdmin.ts` と同じ Tauri/REST
 * 分岐: Tauri webview は `invoke()`（`hub_*` コマンド、
 * `apps/chronogazer/src-tauri/src/lib.rs`）、組み込みサーバー配下の LAN
 * ブラウザは `fetch()`（`/api/hub/*`、`apps/chronogazer/core/src/rest.rs`）。
 * どちらも同じ `chronogazer_core::hub::HubService` を呼ぶので、状態も
 * キーリングも 1 つしかない。
 *
 * プレーンな `vite dev`/`vite preview`（Rust backend 無し）では Hub 接続を
 * 保持する場所そのものが無いため、すべて `DEMO_MODE_MESSAGE` で reject
 * する（`backupsAdmin.ts` の `isBackupsAvailable()`/`demoModeError()` と
 * 同じ作法）。
 *
 * **平文の API キーはこの層を一方向にしか通らない**: `adoptHubKey` の引数
 * として送るだけで、応答型（[`HubView`]）にキーを含むフィールドは無い。
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

/**
 * `banto_hub_bootstrap::HubStatus` の判別共用体（serde の
 * `{"state": ...}` 形をそのまま写したもの）。6 状態は issue #332 の受入
 * 条件で、**エラーを「タグ 0 件」に潰さない**ことがこの型の要点:
 * `connected` の `tagCount: 0` は「接続できているがタグが 1 件も無い」と
 * いう正常な状態で、失敗とは別物。
 */
export type HubStatus =
	| { state: 'notConfigured' }
	| { state: 'connected'; tagCount: number }
	| { state: 'authFailed' }
	| { state: 'forbidden' }
	| { state: 'unreachable'; cause: HubUnreachableCause }
	| { state: 'needsPairing' };

/** `banto_hub_bootstrap::UnreachableCause`。 */
export type HubUnreachableCause = 'transport' | 'protocol' | 'server_error' | 'invalid_endpoint';

/** Mirrors `chronogazer_core::hub::HubTagView`. */
export interface HubTag {
	externalName: string;
	name: string;
	dataType: string;
	unit: string | null;
	tagKind: string;
}

/**
 * Mirrors `chronogazer_core::hub::HubView`.
 *
 * `tags` が `null` なのは「catalog をこの往復では読めていない」という意味で、
 * `[]`（読めた結果タグが 0 件）とは**別物**。画面はこの区別をそのまま
 * 出す（受入条件「接続済み・利用可能なタグなし」）。
 */
export interface HubView {
	status: HubStatus;
	endpoint: string | null;
	keyName: string | null;
	selectedTags: string[];
	tags: HubTag[] | null;
	subscription: HubSubscription;
}

/** `banto_tagclient::TagClientConnectionState` の綴りそのまま。 */
export type HubSubscriptionState =
	'stopped' | 'connecting' | 'handshaking' | 'live' | 'rebinding' | 'reconnecting' | 'unauthorized';

/** Mirrors `chronogazer_core::hub::HubValueView`. */
export interface HubValue {
	tag: string;
	/** `null` は「値がまだ無い」であって 0 ではない。 */
	v: number | null;
	q: string;
	t: number;
	valueSource: string;
}

/**
 * Mirrors `chronogazer_core::hub::HubSubscriptionView`（#383 段階1）。
 *
 * 接続設定の 6 状態（[`HubStatus`]）とは**別軸**: 購読が張れなくても
 * `status` は汚れず、張れない理由は `reason` に出る。`values` が入るのは
 * `state === 'live'` のときだけ（Live でないのに古い値を出さないという
 * `banto-tagclient` の規約にそのまま乗る）。
 */
export interface HubSubscription {
	state: HubSubscriptionState;
	reason: string | null;
	subscribedCount: number;
	/**
	 * 選んだのに Hub のタグ一覧に無かった external name（Hub から消えた／
	 * 権限で見えない）。空表示に潰さない。
	 */
	unresolved: string[];
	/**
	 * そのままでは購読要求に載せられなかった external name（名前にカンマを
	 * 含む・空白だけ、または他の名前と同じタグ（安定 ID）を指す重複）。
	 * `unresolved` とは**理由も次の一手も違う**ので混ぜない。
	 */
	unsupported: string[];
	lastError: string | null;
	lastValueAt: number | null;
	values: HubValue[];
}

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Is this environment backed by a real Hub bootstrap (Tauri or the embedded server)? */
export function isHubAvailable(): boolean {
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

/** Same type guard as usersAdmin.ts / auditLogAdmin.ts / backupsAdmin.ts. */
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

/** Same token lookup as usersAdmin.ts/auditLogAdmin.ts/backupsAdmin.ts. */
function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

function authHeaders(extra?: Record<string, string>): Record<string, string> {
	const headers: Record<string, string> = { ...CSRF_HEADER, ...extra };
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

interface HttpJsonInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
}

async function httpJson<T>(path: string, init: HttpJsonInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers = authHeaders(hasBody ? { 'Content-Type': 'application/json' } : undefined);

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

	if (!response.ok) throw await errorFromResponse(response);
	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

/** `admin`-only: 保存済み設定での現在状態。キーの発行は行わない。 */
export async function getHubStatus(): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_status');
	return httpJson<HubView>('/api/hub', { method: 'GET' });
}

/**
 * `admin`-only: 購読の状態だけ（#383 段階1）。
 *
 * **ネットワーク（Hub への往復）を伴わない**ので、設定画面を開いている間
 * だけポーリングしてよい。`getHubStatus()` は catalog を毎回取り直すので
 * ポーリングには使わない。
 */
export async function getHubSubscription(): Promise<HubSubscription> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubSubscription>('hub_subscription');
	return httpJson<HubSubscription>('/api/hub/subscription', { method: 'GET' });
}

/** `admin`-only: 接続（試運転中の Hub にのみ `read` キーを自己発行）。 */
export async function connectHub(endpoint: string): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_connect', { endpoint });
	return httpJson<HubView>('/api/hub/connect', { method: 'POST', body: { endpoint } });
}

/** `admin`-only: タグ一覧の再取得。 */
export async function refreshHubCatalog(): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_refresh_catalog');
	return httpJson<HubView>('/api/hub/refresh', { method: 'POST' });
}

/** `admin`-only: 選択タグの保存。空配列も正当な入力。 */
export async function setHubSelectedTags(tags: string[]): Promise<void> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('hub_set_selected_tags', { tags });
		return;
	}
	await httpJson<void>('/api/hub/selected-tags', {
		method: 'PUT',
		body: { tags },
		expectNoContent: true
	});
}

/**
 * `admin`-only: ロックダウン済み Hub 向けの手動連携。`key` は平文なので
 * 画面側は `type="password"` で受け取り、ここから先は保存先（OS キーリング）
 * まで一方通行で流れる。
 */
export async function adoptHubKey(endpoint: string, key: string): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<HubView>('hub_adopt_manual_key', { endpoint, key });
	return httpJson<HubView>('/api/hub/adopt-key', { method: 'POST', body: { endpoint, key } });
}

/** `admin`-only: 切断（ローカルの設定とキーリングのみ。Hub 側のキーは残る）。 */
export async function disconnectHub(): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_disconnect');
	return httpJson<HubView>('/api/hub', { method: 'DELETE' });
}

/**
 * 6 状態の見出し（純関数 - `hubAdmin.test.ts` が固定する）。
 *
 * 「接続済み」と「接続済み・利用可能なタグなし」を別文言にするのが受入
 * 条件の要点で、`tagCount === 0` を失敗扱いにしない。
 */
export function hubStatusLabel(status: HubStatus): string {
	switch (status.state) {
		case 'notConfigured':
			return '未設定';
		case 'connected':
			return status.tagCount === 0
				? '接続済み・利用可能なタグなし'
				: `接続済み（タグ${status.tagCount}件）`;
		case 'authFailed':
			return '認証に失敗';
		case 'forbidden':
			return '権限が不足';
		case 'unreachable':
			return 'Hubに到達できません';
		case 'needsPairing':
			return '連携が必要';
	}
}

/** 状態ごとの補足説明（次に何をすればよいか）。純関数。 */
export function hubStatusDetail(status: HubStatus): string {
	switch (status.state) {
		case 'notConfigured':
			return '接続先のURLを入力して「接続」を押してください。';
		case 'connected':
			return status.tagCount === 0
				? 'Hubにはまだタグが登録されていません。Hub側でタグを登録すると、ここに表示されます。'
				: '購読するタグを選んで保存できます。';
		case 'authFailed':
			return '保存済みのAPIキーが無効です。「接続」でキーを再発行できます（Hubがロックダウン済みの場合は連携が必要です）。';
		case 'forbidden':
			return '保存済みのAPIキーに読み取り権限がありません。Hubの管理画面で読み取り権限のあるキーを発行し、下の欄から採用してください。';
		case 'unreachable':
			return `${hubUnreachableCauseLabel(status.cause)} 接続先のURLとHubの稼働状況を確認してください。`;
		case 'needsPairing':
			return 'Hubはロックダウン済みのため、このアプリが自分でAPIキーを発行することはできません。Hubの管理画面で読み取り用のAPIキーを発行し、下の欄に貼り付けてください。';
	}
}

/** `UnreachableCause` の日本語化。純関数。 */
export function hubUnreachableCauseLabel(cause: HubUnreachableCause): string {
	switch (cause) {
		case 'transport':
			return '応答がありません（接続できませんでした）。';
		case 'protocol':
			return '応答の形式が想定と異なります。';
		case 'server_error':
			return 'Hubがエラーを返しました。';
		case 'invalid_endpoint':
			return 'リダイレクトが返されました（接続先が別のサーバーを指している可能性があります）。';
		default:
			return '原因を特定できませんでした。';
	}
}

/** 手動キーの入力欄を出すべき接続状態か（純関数）。 */
export function needsManualKey(status: HubStatus): boolean {
	return status.state === 'needsPairing' || status.state === 'forbidden';
}

/**
 * 手動キーの入力欄を出すべきか（接続状態と購読状態の**両方**を見る純関数）。
 *
 * catalog は読めていて WS のハンドシェイクだけが 401/403 だと、接続状態は
 * `connected` のまま購読だけ `unauthorized` になる。バックエンドでこの 2 つを
 * 別軸にしておくのは正しい（購読の失敗で 6 状態を汚さない）が、**UI で合流
 * させないとユーザーに直す手段が無くなる** - 接続側の状態だけを見ていると
 * 手動キーの導線が出ないため。#383 段階1 の受入「`Unauthorized` は既存の
 * 認証・手動キーの導線へ合流させる」はこれを指す。
 */
export function showManualKeyEntry(
	status: HubStatus,
	subscription: HubSubscription | null
): boolean {
	return needsManualKey(status) || subscription?.state === 'unauthorized';
}

/**
 * 飛行中だったポーリングの応答を今も適用してよいか（純関数）。
 *
 * ポーリング同士は直列化できても、**明示操作（接続・切断・一覧更新など）
 * との競合は残る**: 接続の前に飛ばしたポーリングが `connect()` の応答より
 * 後に着くと、新しい状態を古い状態で上書きしてしまう。明示操作の結果を
 * 反映するたびに進める番号を送信前に覚えておき、**着いたときに番号が
 * 変わっていたら捨てる**。
 */
export function isPollResultFresh(sentAtSeq: number, currentSeq: number): boolean {
	return sentAtSeq === currentSeq;
}

/**
 * 停止・再開を跨いだポーリングの世代が今も現役か（純関数）。
 *
 * `isPollResultFresh` が潰すのは**明示操作との競合**、こちらが潰すのは
 * **停止と再開の競合**: タブを隠した瞬間に飛んでいた要求が再表示後に解決
 * すると、停止したはずのループが次のタイマを張り、`startPolling()` の新しい
 * ループと**二重に回り続ける**。停止のたびに進む世代番号を送信時に覚えて
 * おき、**応答の適用と次回の予約の両方**をこれで守る。
 */
export function isPollGenerationCurrent(sentAtGeneration: number, current: number): boolean {
	return sentAtGeneration === current;
}

/**
 * 購読状態の取得が「恒久的に失敗している」と見なす連続失敗回数（レビュー
 * P2-A）。`SUBSCRIPTION_POLL_MS`（2 秒）× この回数 ≒ 4 秒。
 *
 * **1 回では切り替えない**: 一過性の取りこぼし（スリープ復帰直後の 1 発など）
 * で表示を揺らさないため。逆に数を増やしすぎると、サーバープロセスが落ちた
 * あとも「受信中」と最後の値を出し続ける時間が延びる。
 */
export const SUBSCRIPTION_POLL_FAILURE_LIMIT = 2;

/**
 * 連続失敗回数の遷移（純関数）。**成功で 0 に戻す**のが要点 - 復帰したら
 * 1 回の成功で通常表示に戻る。
 */
export function nextPollFailureCount(current: number, outcome: 'ok' | 'failed'): number {
	return outcome === 'ok' ? 0 : current + 1;
}

/**
 * 購読状態の表示が「今の状態ではない」（取得できていない）か（純関数）。
 *
 * これが true の間も**値の表は消さない**: 消すと「0 件」に潰れて別の嘘に
 * なる。代わりに見出しで言い切らず（[`hubSubscriptionHeadline`]）、最後に
 * 取得できた時刻を添える（[`hubPollStaleNote`]）。
 */
export function isSubscriptionStale(consecutiveFailures: number): boolean {
	return consecutiveFailures >= SUBSCRIPTION_POLL_FAILURE_LIMIT;
}

/**
 * 購読状態の見出し（純関数）。取得できていない間は**「受信中」と言い切ら
 * ない** - 既存の状態名（[`hubSubscriptionLabel`]）はそのまま残し、
 * 「取得できていない」ことだけを添える（状態名を増やすと 7 状態目を
 * 作ったことになり、`banto-tagclient` の状態と 1 対 1 で無くなる）。
 */
export function hubSubscriptionHeadline(state: HubSubscriptionState, stale: boolean): string {
	const label = hubSubscriptionLabel(state);
	return stale ? `${label}（状態を取得できていません）` : label;
}

/**
 * 取得できていない間に添える一文（純関数）。**いつの表示なのか**を出す -
 * 時刻が無いまま古い値だけを見せると、それが今の値に見えてしまう。
 *
 * `lastPolledAt` はブラウザ側の epoch ミリ秒（最後に購読状態を取得できた
 * 時刻）で、Hub 側の値の時刻（`HubValue.t`）とは別物。
 */
export function hubPollStaleNote(lastPolledAt: number | null): string {
	if (lastPolledAt === null) {
		return '購読状態を取得できていません（まだ一度も取得できていません）。下の表示は最新ではありません。';
	}
	return `購読状態を取得できていません。下の表示は${hubTimeLabel(lastPolledAt)}に取得したもので、最新ではありません。`;
}

// --- レビュー P2-B: 編集中（未保存）のタグ選択を黙って捨てない -------------
//
// `applyView()` はサーバーの `selectedTags` をそのまま `selected` に入れて
// いたため、チェックを変えてから「選択を保存」の隣にある「一覧を更新」
// （や「接続」）を押すと、確認も警告もなく元に戻っていた。#378 で決めた
// 「未保存の入力を黙って捨てない」方針の対象から、ここだけ漏れていた。

/** 2 つの選択が同じ集合か（純関数）。並び順は問わない - 画面のチェックは順序を持たない。 */
export function sameSelection(a: readonly string[], b: readonly string[]): boolean {
	if (a.length !== b.length) return false;
	const inB = new Set(b);
	return a.every((name) => inB.has(name));
}

/**
 * サーバーから届いた view を反映するとき、画面に出す選択（純関数）。
 *
 * **未保存の変更があるときはサーバーの値で上書きしない**。`status`/`tags`/
 * `subscription`/`keyName` など他のフィールドは従来どおり上書きしてよい -
 * 問題になるのは編集中の `selected` だけ。
 */
export function applyServerSelection(
	current: readonly string[],
	serverSelected: readonly string[],
	unsaved: boolean
): string[] {
	return unsaved ? [...current] : [...serverSelected];
}

/**
 * 「サーバー側の選択と異なります」の注記と、明示的に捨てる導線（「サーバーの
 * 内容に戻す」）を出すか（純関数）。
 *
 * 未保存でも**中身が同じに戻っている**なら出さない（触っただけで元に戻した
 * ときに警告を出しても、直す対象が無い）。
 */
export function showsServerSelectionDiff(
	current: readonly string[],
	serverSelected: readonly string[],
	unsaved: boolean
): boolean {
	return unsaved && !sameSelection(current, serverSelected);
}

/**
 * 選択の編集状態（未保存フラグ）に何が起きたか。`applyView` は**この一覧に
 * 無い**: サーバーからの反映は未保存フラグを動かさない（未保存なら保ち、
 * 未保存でなければ保たれるものが無い）。
 */
export type SelectionEvent = 'edited' | 'saved' | 'discarded' | 'disconnected';

/**
 * 未保存フラグの遷移（純関数、総当たりでテストする）。
 *
 * `disconnected` で false にしてよいのは、**切断は接続レコードごと消す**
 * から - 保存先が無くなるので、未保存の選択を残しても戻す先が無い。
 */
export function nextSelectionUnsaved(current: boolean, event: SelectionEvent): boolean {
	switch (event) {
		case 'edited':
			return true;
		case 'saved':
		case 'discarded':
		case 'disconnected':
			return false;
	}
}

/**
 * 未解決・購読不可の一覧に添える「残りはどうなっているか」の一文（純関数）。
 *
 * 1 件も購読できていないのに「残りのタグは購読しています」と言うと**嘘に
 * なる**（選んだ全部が未解決／購読不可のとき）。件数で出し分ける。
 */
export function hubRemainderNote(subscribedCount: number): string {
	return subscribedCount > 0
		? '残りのタグだけを購読しています。'
		: '購読できるタグが他にないため、購読していません。';
}

/**
 * Hub の時刻（`ValuesSnapshot.t` / `ValueEntry.t`）の表示（純関数）。
 *
 * `t` は **epoch ミリ秒**（tag-server-design.md §5.3 のワイヤ形）。表示は
 * 閲覧している端末のロケール・タイムゾーンに任せる（この画面には他に
 * 揃えるべき独自の時刻書式が無く、ユーザーの環境で自然に読める形が最も
 * 誤解が少ない）。
 */
export function hubTimeLabel(epochMs: number): string {
	const at = new Date(epochMs);
	return Number.isNaN(at.getTime()) ? String(epochMs) : at.toLocaleString();
}

/**
 * 購読全体の最終受信時刻の表示（純関数）。
 *
 * **まだ一度も受信していないことを明示する** - 空欄や「0」に潰すと、
 * 「受信していない」のか「表示できていない」のか区別が付かなくなる。
 *
 * バックエンドは「同じ購読が止まっているだけ」なら時刻を残す（いつまで
 * データが来ていたかは診断に効く）。そのため **`live` でないときは、同じ行
 * から今は受信していないと分かる**ようにする - 時刻だけを出すと、止まって
 * いるのに受信し続けているように読めてしまう。
 */
export function hubLastValueLabel(lastValueAt: number | null, state: HubSubscriptionState): string {
	if (lastValueAt === null) return 'まだ受信していません';
	const at = hubTimeLabel(lastValueAt);
	if (state === 'live') return at;
	if (state === 'stopped') return `${at}（購読は停止しています）`;
	return `${at}（現在は受信していません）`;
}

/**
 * 購読状態の見出し（純関数 - `hubAdmin.test.ts` が固定する）。
 *
 * `connecting` と `handshaking` は**意図的に同じ文言**にしている（運用上は
 * どちらも「つなぎに行っている最中」で、区別しても次の一手が変わらない）。
 * それ以外は互いに潰さない - 特に `live` / `reconnecting` / `unauthorized`
 * は「値が来ている／来ていない／権限の問題」という別々の事実。
 */
export function hubSubscriptionLabel(state: HubSubscriptionState): string {
	switch (state) {
		case 'live':
			return '受信中';
		case 'connecting':
		case 'handshaking':
			return '接続中';
		case 'rebinding':
			return '再バインド中';
		case 'reconnecting':
			return '再接続中';
		case 'unauthorized':
			return '認証エラー';
		case 'stopped':
			return '停止';
	}
}

/**
 * 購読状態の補足説明（純関数）。`stopped` のときは Rust 側が付けた
 * `reason` をそのまま併記する（「なぜ止まっているか」を空欄にしない）。
 *
 * `stopped` で `reason` が無いこともある: ワーカーが終端エラーで止まると
 * 世代は残ったまま（つまり `subscribedCount > 0`）`state = 'stopped'` +
 * `lastError` になる。このとき件数を根拠に「購読しています」と言うと
 * **状態表示（停止）と説明（購読中）が矛盾する**ので、`stopped` のうちは
 * 件数を理由にしない。
 *
 * 同じ理由で、**「購読しています」と言い切れるのは `live` のときだけ**。
 * `connecting`/`handshaking`/`rebinding`/`reconnecting` は値を受けていない
 * （`values` も空）進行中の状態なので、件数に触れるときも「購読しようと
 * しています」と、**まだ受信していないことが分かる**言い方にする。
 */
export function hubSubscriptionDetail(subscription: HubSubscription): string {
	if (subscription.state === 'stopped') {
		if (subscription.reason) return subscription.reason;
		if (subscription.lastError) {
			return `購読は停止しています（エラー: ${subscription.lastError}）。まもなく自動で再試行します。`;
		}
		return '購読していません。';
	}
	if (subscription.state === 'unauthorized') {
		// タグ一覧は読めていても購読だけ拒否されることがある（WS のハンド
		// シェイクだけが 401/403）。ユーザーにとっては接続の状態表示が何で
		// あれ「認証が通っていない」なので、接続側の `authFailed` と同じ
		// 導線（再接続で再発行 / 管理者発行のキーを採用）へ誘導する。
		// 入力欄はこのブロックより上にあるので「下の欄」とは言わない。
		return 'Hubがこのキーでの購読を拒否しました（認証が通っていません）。「接続」でキーを再発行するか、Hubの管理画面で発行したAPIキーをこの画面の入力欄から採用してください。';
	}
	if (subscription.state === 'live') {
		return `${subscription.subscribedCount}件のタグを購読しています。`;
	}
	// ここから下はすべて「進行中でまだ受信していない」状態。
	switch (subscription.state) {
		case 'connecting':
		case 'handshaking':
			return `Hubに接続しています（${subscription.subscribedCount}件のタグを購読しようとしています）。`;
		case 'rebinding':
			return `タグの対応を取り直しています（${subscription.subscribedCount}件）。Hub側でタグが変更された可能性があります。`;
		case 'reconnecting':
			return `再接続を待っています（${subscription.subscribedCount}件のタグを購読しようとしています）。`;
	}
}
