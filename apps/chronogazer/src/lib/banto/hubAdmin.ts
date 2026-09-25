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
	/**
	 * #446: 保存済みのキーが Hub でトリップしている（REST が
	 * `403 {"error":"key_tripped"}`）。`forbidden`（読み取り権限が無い）とも
	 * `authFailed`（キーが無効）とも違い、**キーは捨てない** - 管理者が解除
	 * すれば同じキーで戻る。
	 */
	| { state: 'keyTripped' }
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
	/**
	 * 呼び出し側が往復を打ち切るための signal（レビュー P2-3）。`abort` で
	 * `fetch` が投げる例外は下の `catch` に落ち、他の通信失敗と同じ
	 * `NETWORK_ERROR_MESSAGE` になる - 打ち切りは呼び出し側が知っていること
	 * なので、ここで種類を分ける必要が無い。
	 */
	signal?: AbortSignal;
}

async function httpJson<T>(path: string, init: HttpJsonInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers = authHeaders(hasBody ? { 'Content-Type': 'application/json' } : undefined);

	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: hasBody ? JSON.stringify(init.body) : undefined,
			signal: init.signal
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) throw await errorFromResponse(response);
	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

/**
 * `admin`-only: 保存済み設定での現在状態。キーの発行は行わない。
 *
 * `signal` の効き方は [`getHubSubscription`] と同じ（REST だけが実際に往復を
 * 畳み、Tauri は呼び出し側の「打ち切り済み」フラグでしか守れない）。明示操作
 * 全体の上限については [`HUB_UI_TIMEOUT_MS`] を参照。
 */
export async function getHubStatus(signal?: AbortSignal): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_status');
	return httpJson<HubView>('/api/hub', { method: 'GET', signal });
}

/**
 * `admin`-only: 購読の状態だけ（#383 段階1）。
 *
 * **ネットワーク（Hub への往復）を伴わない**ので、設定画面を開いている間
 * だけポーリングしてよい。`getHubStatus()` は catalog を毎回取り直すので
 * ポーリングには使わない。
 *
 * `signal` は**REST 経路でだけ効く**（レビュー P2-3）。Tauri の `invoke` に
 * 中断の口は無いので、そちらは呼び出し側の「打ち切り済み」フラグ
 * （[`readSubscriptionWithLimit`]）でしか守れない。この非対称は意図的:
 * REST は実際にソケットを畳めるので畳み、Tauri は**遅れて解決した応答を
 * 採用しない**ことだけを保証する。
 */
export async function getHubSubscription(signal?: AbortSignal): Promise<HubSubscription> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubSubscription>('hub_subscription');
	return httpJson<HubSubscription>('/api/hub/subscription', { method: 'GET', signal });
}

/**
 * `admin`-only: 接続（試運転中の Hub にのみ `read` キーを自己発行）。
 *
 * **`signal` で打ち切っても Hub 側の処理は止まらない**（[`HUB_UI_TIMEOUT_MS`]）。
 * この操作は非冪等なので、打ち切った側は「失敗した」と言ってはいけない。
 */
export async function connectHub(endpoint: string, signal?: AbortSignal): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_connect', { endpoint });
	return httpJson<HubView>('/api/hub/connect', { method: 'POST', body: { endpoint }, signal });
}

/** `admin`-only: タグ一覧の再取得。 */
export async function refreshHubCatalog(signal?: AbortSignal): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_refresh_catalog');
	return httpJson<HubView>('/api/hub/refresh', { method: 'POST', signal });
}

/** `admin`-only: 選択タグの保存。空配列も正当な入力。 */
export async function setHubSelectedTags(tags: string[], signal?: AbortSignal): Promise<void> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('hub_set_selected_tags', { tags });
		return;
	}
	await httpJson<void>('/api/hub/selected-tags', {
		method: 'PUT',
		body: { tags },
		expectNoContent: true,
		signal
	});
}

/**
 * `admin`-only: ロックダウン済み Hub 向けの手動連携。`key` は平文なので
 * 画面側は `type="password"` で受け取り、ここから先は保存先（OS キーリング）
 * まで一方通行で流れる。
 *
 * [`connectHub`] と同じく**打ち切っても止まらない**非冪等な操作
 * （[`HUB_UI_TIMEOUT_MS`]）。
 */
export async function adoptHubKey(
	endpoint: string,
	key: string,
	signal?: AbortSignal
): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri')
		return invokeCommand<HubView>('hub_adopt_manual_key', { endpoint, key });
	return httpJson<HubView>('/api/hub/adopt-key', {
		method: 'POST',
		body: { endpoint, key },
		signal
	});
}

/** `admin`-only: 切断（ローカルの設定とキーリングのみ。Hub 側のキーは残る）。 */
export async function disconnectHub(signal?: AbortSignal): Promise<HubView> {
	if (!isHubAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<HubView>('hub_disconnect');
	return httpJson<HubView>('/api/hub', { method: 'DELETE', signal });
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
		case 'keyTripped':
			return 'キーがトリップ中';
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
		case 'keyTripped':
			return 'HubがこのAPIキーをトリップ（一時停止）させています。Hubの管理者に解除を依頼してください。解除されると同じキーのまま、数分以内に自動で購読を再開します（この画面を開き直すとすぐに確認します）。';
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
	// #446: トリップ中はキーを捨てさせない（管理者が解除すれば同じキーで戻る）。
	// 入力欄を出すと「新しいキーに替えよ」と読めるので出さない - 接続の状態が
	// `keyTripped` なら `effectiveCredentialGuidance` が常にトリップの案内を
	// 返すので、下の 3 つの条件はどれも真にならない。
	const guidance = effectiveCredentialGuidance(status, subscription?.lastError ?? null);
	return (
		needsManualKey(status) ||
		// 購読だけ拒否された（WS のハンドシェイクだけが 401/403）。ただし
		// 理由がトリップなら、上と同じくキーを替えさせない。
		(subscription?.state === 'unauthorized' && guidance?.action !== 'askAdmin') ||
		// #446: 失効・期限切れ・存在しないキーは、世代を止めた後（`stopped` +
		// `lastError`）も「新しい API キーを設定」へ誘導する。案内だけ出して
		// 入力欄が無い、という形にしない。
		guidance?.action === 'replaceKey'
	);
}

/**
 * Hub が close 1008 で購読を打ち切った理由（`banto_tagclient::ErrorKind` の
 * `as_str`、#446）。`HubSubscription.lastError` に載る。
 */
export type HubCredentialRejection =
	'key_revoked' | 'key_expired' | 'key_tripped' | 'key_not_found' | 'credential_rejected';

/**
 * 理由ごとの次の一手（#446）。
 *
 * - `askAdmin`: トリップ。Hub の管理者が解除すれば**同じキーで戻る**ので、
 *   キーを捨てさせない。アプリは数分おきに自動で確かめる。
 * - `replaceKey`: 失効・期限切れ・存在しない。**同じキーでは二度と戻らない**
 *   ので、新しい API キーを設定してもらう（アプリは自動で再試行しない）。
 * - `checkHub`: 理由を判別できない 1008（新しい Hub の理由など）。管理者に
 *   確認してもらい、アプリは数分おきに自動で確かめる。
 */
export type HubCredentialAction = 'askAdmin' | 'replaceKey' | 'checkHub';

export interface HubCredentialGuidance {
	reason: HubCredentialRejection;
	action: HubCredentialAction;
	message: string;
}

const REPLACE_KEY_STEPS =
	'新しいAPIキーを設定してください（「接続」でキーを再発行するか、Hubの管理画面で発行したAPIキーをこの画面の入力欄から採用してください）。同じキーのままでは自動で再開しません。';

/**
 * `lastError` から、Hub が購読を打ち切った理由ごとの案内を作る（純関数、
 * `hubAdmin.test.ts` が表で固定する）。close 1008 の分類でなければ `null`
 * （通信系のエラーや理由の無い 401/403 は従来の文言に任せる）。
 *
 * 「自動で再開します」と書くのはトリップと判別できない理由だけ - それぞれ
 * 見張り（`chronogazer_core::hub` の `retry_pace`）が 2 分に 1 回確かめに
 * 行く経路がある。失効・期限切れ・存在しないは見張りが再試行しないので、
 * 自動で戻るとは書かない。
 */
export function hubCredentialGuidance(lastError: string | null): HubCredentialGuidance | null {
	switch (lastError) {
		case 'key_tripped':
			return {
				reason: 'key_tripped',
				action: 'askAdmin',
				message:
					'HubがこのAPIキーをトリップ（一時停止）させたため、購読を止めています。Hubの管理者に解除を依頼してください。解除されると同じキーのまま、数分以内に自動で再開します（この画面を開き直すとすぐに確認します）。'
			};
		case 'key_revoked':
			return {
				reason: 'key_revoked',
				action: 'replaceKey',
				message: `HubでこのAPIキーが失効したため、購読を止めています。${REPLACE_KEY_STEPS}`
			};
		case 'key_expired':
			return {
				reason: 'key_expired',
				action: 'replaceKey',
				message: `このAPIキーの有効期限が切れたため、購読を止めています。${REPLACE_KEY_STEPS}`
			};
		case 'key_not_found':
			return {
				reason: 'key_not_found',
				action: 'replaceKey',
				message: `HubにこのAPIキーが見つからないため、購読を止めています。${REPLACE_KEY_STEPS}`
			};
		case 'credential_rejected':
			return {
				reason: 'credential_rejected',
				action: 'checkHub',
				message:
					'Hubがこのキーでの購読を打ち切りました（理由を判別できません）。Hubの管理者に確認してください。数分おきに自動で確認します。'
			};
		default:
			return null;
	}
}

/**
 * 接続の状態と突き合わせた、購読の案内（#446、純関数）。
 *
 * 接続の状態（REST の答え）は**今のキーについての最新の判定**で、購読の
 * `lastError`（close 1008 の理由）はそれより前の出来事のこともある。両者が
 * 「キーを捨てるな」と「新しいキーを」を同時に言わないように、食い違うときは
 * 接続の状態に合わせる:
 *
 * | 接続の状態 | 購読の案内 |
 * | --- | --- |
 * | `keyTripped` | 理由に関わらずトリップの案内（REST が「トリップ中」と言っている） |
 * | `authFailed` / `forbidden` / `needsPairing` | トリップの案内は出さない（REST が「キーが無効／権限が無い／発行が要る」と言っている）。それ以外の理由はそのまま |
 * | それ以外 | `lastError` の案内のまま |
 *
 * バックエンド（`chronogazer_core::hub` の `credential_rejection_after_status`）
 * も同じ向きで記憶を直すので、ここは表示側の二重の守り。
 */
export function effectiveCredentialGuidance(
	status: HubStatus | null,
	lastError: string | null
): HubCredentialGuidance | null {
	if (status?.state === 'keyTripped') return hubCredentialGuidance('key_tripped');
	const guidance = hubCredentialGuidance(lastError);
	if (
		guidance?.action === 'askAdmin' &&
		(status?.state === 'authFailed' ||
			status?.state === 'forbidden' ||
			status?.state === 'needsPairing')
	) {
		return null;
	}
	return guidance;
}

/**
 * 購読ブロックに案内を**独立した行**で出すか（純関数）。
 *
 * `unauthorized` のときは [`hubSubscriptionDetail`] が案内そのものを説明文に
 * するので、二重に出さない。`stopped`（`status()` などが世代を止めた後）は
 * 説明文が停止の理由（「保存済みのAPIキーがHubに拒否された…」など）になる
 * ので、理由ごとの案内を別の行で添える。
 */
export function hubCredentialGuidanceLine(
	subscription: HubSubscription | null,
	status: HubStatus | null = null
): string | null {
	if (!subscription || subscription.state === 'unauthorized') return null;
	// 接続の状態の説明（`hubStatusDetail`）がトリップの案内そのものなので、
	// 同じ画面に 2 回出さない。
	if (status?.state === 'keyTripped') return null;
	if (subscription.state === 'stopped' && !subscription.reason) return null;
	return effectiveCredentialGuidance(status, subscription.lastError)?.message ?? null;
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
 * P2-A）。即座に失敗する障害なら `SUBSCRIPTION_POLL_MS`（2 秒）× この回数
 * ≒ 4 秒、**応答が返ってこない障害なら**
 * `SUBSCRIPTION_POLL_TIMEOUT_MS`（4 秒）× この回数 ≒ 8 秒で切り替わる。
 *
 * **1 回では切り替えない**: 一過性の取りこぼし（スリープ復帰直後の 1 発など）
 * で表示を揺らさないため。逆に数を増やしすぎると、サーバープロセスが落ちた
 * あとも「受信中」と最後の値を出し続ける時間が延びる。
 */
export const SUBSCRIPTION_POLL_FAILURE_LIMIT = 2;

/**
 * 購読状態の読み取り 1 回に許す上限（ms、レビュー P2-3）。
 *
 * **なぜ 4 秒か**: この読み取りはネットワークを伴わないメモリ参照で、
 * ポーリング間隔は `SUBSCRIPTION_POLL_MS`（2 秒）。2 周期ぶん待っても返って
 * こない相手は、もう応答していないと見てよい。短くしすぎると混み合った
 * LAN ブラウザで正常な往復を打ち切ってしまい、長くすると「嘘の表示」が
 * 残る時間が延びる。
 *
 * これが無いと**連続失敗を数える仕組みが空振りする**: `getHubSubscription()`
 * は Tauri の `invoke` にも `fetch` にも上限が無く、TCP は繋がるが応答が
 * 返らない相手では `catch` に入らないまま止まる。`pollThenSchedule()` は
 * 読み取りの完了後に次を予約するので、**ポーリングのループごと止まった**
 * （ユーザーがボタンを押すまで永久に「受信中」＋最後の値）。上限を入れた
 * ことで、読み取りは必ず有限時間で戻り、次の予約も必ず行われる。
 *
 * 失敗判定は `SUBSCRIPTION_POLL_FAILURE_LIMIT`（2 回）なので、最悪でも
 * 4 秒 × 2 = 8 秒程度で「取得できていません」に切り替わる。
 */
export const SUBSCRIPTION_POLL_TIMEOUT_MS = 4000;

/**
 * 購読状態の読み取り 1 回の結末（レビュー P2-3）。
 *
 * `failed`（相手がエラーを返した／届かなかった）と `timedOut`（上限まで何も
 * 返ってこなかった）を**別の値**にしているのは、連続失敗の数え方は同じでも
 * 原因が違うため（`timedOut` は「繋がってはいるが応答しない」）。
 */
export type SubscriptionReadOutcome =
	{ kind: 'ok'; value: HubSubscription } | { kind: 'failed' } | { kind: 'timedOut' };

/**
 * 上限付きで 1 回走らせた往復の結末（[`runWithLimit`]）。
 *
 * `failed`（相手がエラーを返した／届かなかった）と `timedOut`（上限まで何も
 * 返ってこなかった）を**別の値**にしているのは、呼び出し側の次の一手が違う
 * ため: `failed` は「失敗した」と言い切ってよいが、`timedOut` は
 * **こちらが待つのをやめただけ**で、相手の処理は続いているかもしれない。
 *
 * `failed` が `error` を運ぶのは、明示操作が既存のエラー文言
 * （`errorMessage()`）をそのまま出せるようにするため。購読ポーリングは
 * 文言を使わないので、[`readSubscriptionWithLimit`] が落として渡す。
 */
export type RunWithLimitOutcome<T> =
	{ kind: 'ok'; value: T } | { kind: 'failed'; error: unknown } | { kind: 'timedOut' };

/**
 * 画面→アプリの往復を 1 回だけ走らせ、**上限を過ぎたら打ち切る**（レビュー
 * P2-3 で購読ポーリング用に作ったものを、明示操作にも使えるよう一般化）。
 *
 * 往復そのものを引数に取るので、`vi.useFakeTimers()` と「解決しない
 * Promise」だけでテストできる（即座に reject するモックでは、この欠陥は
 * 再現できない）。
 *
 * 打ち切りは 2 段構え:
 * 1. `AbortSignal` を往復に渡す（REST 経路は実際にソケットを畳む）。
 * 2. **試行ごとの「打ち切り済み」フラグ**。Tauri の `invoke` は中断できない
 *    ので、遅れて解決した応答が `ok` に化けないようここで止める - 通すと
 *    呼び出し側が「最新の状態を取得できた」と記録し、嘘の表示が新鮮扱いに
 *    戻る。
 *
 * **打ち切ってもアプリ側の処理は止まらない**: `abort` で畳めるのはこちらの
 * `fetch` だけで、axum のハンドラも Tauri のコマンドも走り続ける。呼び出し側
 * に渡した `signal` は「もう採用しない」という合図でもあるので、**遅れて解決
 * した応答で画面を書き換える前に `signal.aborted` を見ること**。
 */
export async function runWithLimit<T>(
	run: (signal: AbortSignal) => Promise<T>,
	timeoutMs: number
): Promise<RunWithLimitOutcome<T>> {
	const controller = new AbortController();
	/** この試行はもう打ち切った（以後の解決は採用しない）。 */
	let abandoned = false;
	let timer: ReturnType<typeof setTimeout> | null = null;

	const expiry = new Promise<RunWithLimitOutcome<T>>((resolve) => {
		timer = setTimeout(() => {
			abandoned = true;
			controller.abort();
			resolve({ kind: 'timedOut' });
		}, timeoutMs);
	});

	const attempt = run(controller.signal).then(
		(value): RunWithLimitOutcome<T> => (abandoned ? { kind: 'timedOut' } : { kind: 'ok', value }),
		(error): RunWithLimitOutcome<T> =>
			abandoned ? { kind: 'timedOut' } : { kind: 'failed', error }
	);

	try {
		return await Promise.race([attempt, expiry]);
	} finally {
		if (timer !== null) clearTimeout(timer);
	}
}

/**
 * 購読状態を 1 回だけ読み、**上限を過ぎたら打ち切る**（レビュー P2-3）。
 *
 * 中身は [`runWithLimit`] そのもので、違いは**エラーの中身を落とす**ことだけ:
 * ポーリングの失敗はトーストにしない（明示操作のエラー表示を上書きしない）
 * ので、数える以上のことをしない。
 */
export async function readSubscriptionWithLimit(
	read: (signal: AbortSignal) => Promise<HubSubscription>,
	timeoutMs: number = SUBSCRIPTION_POLL_TIMEOUT_MS
): Promise<SubscriptionReadOutcome> {
	const outcome = await runWithLimit(read, timeoutMs);
	return outcome.kind === 'failed' ? { kind: 'failed' } : outcome;
}

/**
 * 画面→アプリの**明示操作**（`getHubStatus` / `connectHub` /
 * `refreshHubCatalog` / `setHubSelectedTags` / `adoptHubKey` /
 * `disconnectHub`）1 回に許す上限（ms）。
 *
 * **なぜ要るか**: この 6 つには上限が無かった。`hubAdmin.ts` の `fetch` にも
 * Tauri の `invoke` にも上限が無いので、同じ PC のアプリ側（`banto-serve` /
 * Tauri プロセス）が応答しなくなると `HubSection.svelte` の `run()` が
 * `busy = true` のまま戻らず、**画面が操作不能になる（復旧はアプリの再起動
 * のみ）**。購読ポーリング（[`SUBSCRIPTION_POLL_TIMEOUT_MS`]）より症状は重い -
 * あちらは黙って古い値を出し続けるだけだが、こちらは操作そのものができない。
 *
 * **なぜ 90 秒か**: バックエンドは既に自前の上限を持っている
 * （`apps/chronogazer/core/src/hub.rs`）。読み取りは
 * `HUB_OPERATION_TIMEOUT` = 15 秒、Hub 側にキーを作る `connect` だけは
 * `HUB_MUTATING_TIMEOUT` = 60 秒。さらに**全操作が `begin_operation()` の
 * 操作ロックを取る**ので、`connect`（最大 60 秒）の後ろに読み取り（最大
 * 15 秒）が並ぶと、**正当に遅い最長は 60 + 15 ≒ 75 秒**になり得る。90 秒は
 * それを十分に超えるので、**発火は「遅い」ではなく「アプリが応答していない」
 * を意味する**。
 *
 * **なぜ読み取り用と変更用で分けないか**: 読み取りも同じ操作ロックに並ぶので、
 * 読み取りだけ短くすると `connect` の後ろに付いた正常な `getHubStatus()` を
 * 見限ってしまう（バックエンド側で 15/60 に分かれているのは**ロックを取った
 * 後の 1 往復**に対する上限で、順番待ちを含まない）。画面側は順番待ちを含む
 * 待ち時間しか観測できないため、1 つの値にする。
 *
 * **打ち切りは失敗ではない**: 上限が来てもアプリ側の処理は止まらない
 * （[`runWithLimit`] の doc 参照）。文言は [`hubAbandonedDisplay`] が作る。
 *
 * **この定数を使うのは、まだ「アプリが応答しているかどうか分からない」
 * 往復だけ**: 明示操作そのもの（この定数）と、手で押す「状態を再取得」
 * （`reconfirmStatus`、[`reconfirmStatusFailureNotice`] 参照）。一方、
 * この上限で**打ち切ったあとに自動で走る読み直し**（`rereadAfterAbandon`）
 * だけは、時点で「90 秒応答しなかった」ことが確定済みという別の前提に
 * 立つため、短縮された [`HUB_UI_REREAD_TIMEOUT_MS`] を使う - 使い分けの
 * 理由はそちらの doc を参照。
 */
export const HUB_UI_TIMEOUT_MS = 90000;

/**
 * **自動の読み直し**（`HubSection.svelte` の `rereadAfterAbandon()` が呼ぶ
 * `getHubStatus()`）専用の上限（ms、#400 オーナー決定 2026-09-20）。
 *
 * [`HUB_UI_TIMEOUT_MS`] より短くしてよい・短くすべき理由は、この読み直しが
 * **他の明示操作や `reconfirmStatus()` とは前提が違う**ことにある:
 *
 * - **ここに来た時点で、直前の操作は [`HUB_UI_TIMEOUT_MS`]（90 秒）応答
 *   しなかったことが確定している**。つまり「遅い」ではなく「固まって
 *   いる」。もう一度 90 秒待っても新しい情報は増えず、**画面の停止が
 *   合計 180 秒になるだけ**（しかもこれは例外的な最悪ケースではなく、
 *   この経路に入る典型的な結果）。
 * - **正当に遅いだけの最長ケースはこの経路に入らない**: バックエンドの
 *   `HUB_MUTATING_TIMEOUT`（60 秒、`hub.rs`）に操作ロックの順番待ちで
 *   加算される読み取り（15 秒）を足しても ≒ 75 秒で、[`HUB_UI_TIMEOUT_MS`]
 *   の 90 秒に届かない。つまり `rereadAfterAbandon()` が呼ばれる時点で
 *   相手が正当に遅いだけという可能性は既に排除されている。だから 15 秒に
 *   縮めても、正常な応答を見限るリスクが無い。
 *
 * **`reconfirmStatus()`（利用者が押す「状態を再取得」）には使わない** -
 * あちらは押されるタイミングが任意で、そのときアプリは単に忙しいだけ
 * （別の `connect` が操作ロックを最大 60 秒握っている、など）かもしれない。
 * 15 秒にすると、正常に進行中の読み取りを見限って「再取得も失敗しました」
 * と表示し、利用者を止めたままにしてしまう。[`HUB_UI_TIMEOUT_MS`] のまま
 * にする理由はこれ。
 */
export const HUB_UI_REREAD_TIMEOUT_MS = 15000;

/**
 * 明示操作を打ち切ったあと、画面に何を出して何を更新してよいか
 * （[`hubAbandonedDisplay`] の出力）。
 */
export interface HubAbandonedDisplay {
	/** 打ち切りとして出す文言。`null` = 打ち切っていないので何も足さない。 */
	notice: string | null;
	/** 読み直せた状態を画面に反映してよいか。 */
	applyStatus: boolean;
	/**
	 * **現在の接続先に依存する変更操作を止めるか**（オーナーレビュー P2-1）。
	 *
	 * 打ち切った操作はバックエンドで**完了している可能性がある**ので、状態を
	 * 読み直せなかった時点で、**画面が持っている接続先と、次の操作が実際に触る
	 * 接続先が食い違いうる**。`setHubSelectedTags` は**タグ名しか送らない**ので、
	 * バックエンドの接続先が Hub B に変わっていると **B の購読設定に A のタグ
	 * 選択が保存される** - #397 で塞いだ「下書きが接続先の境界を越える」型が、
	 * 打ち切り経路で再発する。
	 *
	 * **`connect` / `adopt` も止める**。打ち切った `connect` がまだ走っている
	 * かもしれない状態で押し直せると、**Hub 側にキーを二重に発行**しうる
	 * （#395 の P1-2 と同じ残留リスクを増やす）。
	 *
	 * 止めるのは変更操作だけで、**画面全体は操作不能にしない** - 読み取り専用の
	 * 「状態を再取得」（[`nextStatusUnconfirmed`]）で必ず抜けられる。
	 */
	blocksChanges: boolean;
}

/**
 * 打ち切ったあとの見せ方（純関数 - `hubAdmin.test.ts` が総当たりで固定する）。
 *
 * 軸は 2 つ:
 * - `abandoned`: 上限（[`HUB_UI_TIMEOUT_MS`]）で待つのをやめたか。
 * - `statusReread`: 打ち切った直後の `getHubStatus()`（同じ上限）が読めたか。
 *
 * **「失敗しました」と言わないのが要点**。打ち切ったのは**待ち時間**であって
 * 操作ではなく、`connectHub` / `adoptHubKey` は Hub 側にキーを発行・保存する
 * 非冪等な操作なので、打ち切った後に成功していることがある。「接続できません
 * でした」と言い切ると、Hub に残ったキーの存在が利用者に見えなくなる
 * （`hub.rs` の `SettingsMirror::mark_flushed` と `HUB_MUTATING_TIMEOUT` の
 * 「残留リスク」= #395 で直した P1-2 と同じ話。コード上の表記は
 * 「#394 のレビュー P1-2」）。
 *
 * 読み直せたときも文言は残す（読み直しは**今の状態**を写しただけで、打ち切った
 * 操作が完了したことの証明ではない）。読み直せなかったときは**状態を作らない** -
 * 「不明」という状態を新設せず、前の表示を残したまま、最新ではないと書く。
 *
 * オーナーレビュー P2-1 で 3 つ目の出力 `blocksChanges` が増えた。警告を出す
 * だけでは足りない（`busy` が降りて変更操作が再び押せてしまう）ので、**同じ
 * 判断からそのまま「止めるか」も返す**。読み直せなかったときだけ true。
 */
export function hubAbandonedDisplay(
	abandoned: boolean,
	statusReread: boolean
): HubAbandonedDisplay {
	if (!abandoned) return { notice: null, applyStatus: false, blocksChanges: false };
	const seconds = Math.round(HUB_UI_TIMEOUT_MS / 1000);
	if (statusReread) {
		return {
			notice: `アプリが${seconds}秒以内に応答しませんでした。待つのをやめただけなので、操作は続いている可能性があります。下の表示は、そのあとに読み直した現在の状態です。`,
			applyStatus: true,
			blocksChanges: false
		};
	}
	return {
		// **何ができない状態か**と**何を押せばよいか**まで書く（警告だけ出して
		// 操作を止めないと、接続先を確認できないまま保存が通ってしまう）。
		notice: `アプリが${seconds}秒以内に応答しませんでした。待つのをやめただけなので、操作は続いている可能性があります。現在の状態も読み取れなかったため、下の表示は最新ではありません。今の接続先を確認できるまで、接続・切断・キーの採用・一覧の更新・選択の保存とタグの選択は止めています。「状態を再取得」を押して、現在の状態を読み直してください。`,
		applyStatus: false,
		blocksChanges: true
	};
}

/**
 * 「状態を再取得」1 回の結末（[`nextStatusUnconfirmed`]）。
 */
export type StatusRereadOutcome = 'ok' | 'failed';

/**
 * 「**接続状態を再確認できていない**」フラグの遷移（純関数、総当たりで固定する）。
 *
 * 立てるのは [`hubAbandonedDisplay`] の `blocksChanges`（打ち切ったうえに
 * 読み直せなかった）、**降ろせるのは状態を読めて `applyView()` で反映できた
 * ときだけ**。読めなかったときは `current` のまま返す - ここで降ろすと、確認
 * できていない接続先に対して変更操作が再び通ってしまう（「成功を確かめる前に
 * フラグを降ろさない」）。
 *
 * `busy` とは別軸なのが要点。`busy` は「今この操作の最中か」で、`run()` の
 * `finally` で必ず降りる。こちらは「**画面が今の接続先を知っているか**」で、
 * 読み直せるまで降りない。
 */
export function nextStatusUnconfirmed(current: boolean, reread: StatusRereadOutcome): boolean {
	return reread === 'ok' ? false : current;
}

/**
 * 「状態を再取得」を押した結末の生の種類（[`runWithLimit`] の `kind` そのもの）。
 *
 * [`StatusRereadOutcome`] より一段細かい - あちらは `nextStatusUnconfirmed` の
 * 入力として `failed`/`timedOut` を「読めなかった」に畳んでよいが、
 * [`reconfirmStatusFailureNotice`] は**文言を分ける**ために区別が要る。
 */
export type StatusRereadDetailedOutcome = 'ok' | 'failed' | 'timedOut';

/**
 * 「状態を再取得」を押したのに読み直せなかったときの注記（純関数、
 * `hubAdmin.test.ts` が総当たりで固定する）。
 *
 * **今までは失敗しても何も変わらず、押しても無反応に見えていた**
 * （この PR 全体が潰している「画面が本当のことを言わない」型そのもの）。
 * ここは必ず結果を出す。
 *
 * `hubAbandonedDisplay` と同じ言い分けを引き継ぐ: **`timedOut`（上限で
 * 待つのをやめただけ）を「操作が失敗した」とは言わない** - 打ち切りは
 * 操作の中止ではなく、読み取り自体はアプリ側で続いているかもしれない。
 * 一方 `failed` は実際にエラーが返ってきているので、失敗したと言い切って
 * よい（`errorMessage(outcome.error)` の文字列を `errorText` として渡す -
 * ここでは受け取るだけにして純関数のまま保つ）。
 *
 * `ok` は呼び出し側が `statusUnconfirmedNotice` ごと消す（`nextStatusUnconfirmed`
 * が同時に `statusUnconfirmed` を降ろす）ので `null` を返す。
 *
 * 止めている操作と次の一手（もう一度「状態を再取得」を押す）は、
 * `failed`/`timedOut` のどちらでも文中に残す。文言は `hubError` ではなく
 * `statusUnconfirmedNotice` に置く（`run()` の `beginRun()` は `hubError` を
 * 消すが `statusUnconfirmedNotice` には触らないため、この操作の結果が
 * 次の操作で勝手に消えない）。
 */
export function reconfirmStatusFailureNotice(
	outcome: StatusRereadDetailedOutcome,
	errorText: string | null
): string | null {
	if (outcome === 'ok') return null;
	const seconds = Math.round(HUB_UI_TIMEOUT_MS / 1000);
	if (outcome === 'timedOut') {
		return `状態の再取得も、アプリが${seconds}秒以内に応答しませんでした。待つのをやめただけなので、読み取りは続いている可能性があります。今の接続先を確認できるまで、接続・切断・キーの採用・一覧の更新・選択の保存とタグの選択は止めたままです。もう一度「状態を再取得」を押してください。`;
	}
	return `状態の再取得に失敗しました（${errorText ?? '理由不明'}）。今の接続先を確認できるまで、接続・切断・キーの採用・一覧の更新・選択の保存とタグの選択は止めたままです。もう一度「状態を再取得」を押してください。`;
}

/**
 * 選択の保存 1 回の結末（[`saveSelectionWithLimits`]）。
 */
export interface SaveSelectionOutcome {
	/**
	 * 保存本体（`setHubSelectedTags`）の結末。**この明示操作の成否はこれだけで
	 * 決まる** - 後続の読み取りが返らなくても `timedOut` にはならない。
	 */
	save: RunWithLimitOutcome<void>;
	/**
	 * 保存後の購読の読み直し。保存本体が成功したときだけ走る（`null` = 走らせて
	 * いない）。失敗・打ち切りは**購読状態の取得失敗**として数える
	 * （[`pollFailureOutcome`] → [`nextPollFailureCount`]）。
	 */
	subscriptionReread: SubscriptionReadOutcome | null;
}

/**
 * 選択の保存と、その直後の購読の読み直しを**別々の予算で**走らせる
 * （オーナーレビュー P2-2）。
 *
 * **なぜ分けるか**: 以前は保存も読み直しも 1 つの `runWithLimit(action,
 * HUB_UI_TIMEOUT_MS)` の内側にあった。保存本体が成功していても読み直しだけが
 * 返らないと**操作全体が `timedOut`** になり、「保存しました」と「操作は続いて
 * いる可能性があります」が同時に出る（結果が未確定であるかのような通知）。
 * **保存本体が成功した時点で明示操作の成否を確定させる**のが直し方で、
 * 「読み直しにも上限を足して全体の枠内に残す」のでは、保存本体が上限近くまで
 * かかったケースで同じ問題が残る。
 *
 * 保存後に読み直す意図は元のまま: **バックエンドは保存時に古い世代を止めて
 * いる**ので、次のポーリング（最大 2 秒）まで停止済みの古い値を「受信中」と
 * して出し続けないよう、ここで取り直す。ただしこれは**ベストエフォート**で、
 * 失敗しても保存の成功表示は消さない。
 *
 * `onSaved` は保存本体が `ok` のときだけ、読み直しに入る**前**に 1 回だけ
 * 呼ぶ（画面が「保存しました」を出すのがここ）。
 */
export async function saveSelectionWithLimits(
	save: (signal: AbortSignal) => Promise<void>,
	readSubscription: (signal: AbortSignal) => Promise<HubSubscription>,
	onSaved: () => void,
	saveTimeoutMs: number = HUB_UI_TIMEOUT_MS,
	subscriptionTimeoutMs: number = SUBSCRIPTION_POLL_TIMEOUT_MS
): Promise<SaveSelectionOutcome> {
	const saved = await runWithLimit(save, saveTimeoutMs);
	if (saved.kind !== 'ok') return { save: saved, subscriptionReread: null };
	onSaved();
	const subscriptionReread = await readSubscriptionWithLimit(
		readSubscription,
		subscriptionTimeoutMs
	);
	return { save: saved, subscriptionReread };
}

/**
 * 読み取りの結末を連続失敗カウンタの入力に畳む（純関数）。
 *
 * **`timedOut` は失敗として数える**: 応答が返らないのだから、画面に出ている
 * 購読状態が今の状態だとは言えない。「起こす条件」と「実際にやる条件」を
 * 1 つの述語にしておく（呼び出し側で分岐を書き分けない）。
 */
export function pollFailureOutcome(outcome: SubscriptionReadOutcome): 'ok' | 'failed' {
	return outcome.kind === 'ok' ? 'ok' : 'failed';
}

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
 * サーバーから届いた view を反映した結果（[`applyServerSelection`] の出力）。
 */
export interface ServerSelectionOutcome {
	/** 画面に出す選択。 */
	selected: string[];
	/** 反映後の未保存フラグ。 */
	unsaved: boolean;
	/** 接続先が変わったため、未保存の下書きを捨てたか（**黙って捨てない**ための合図）。 */
	discardedForEndpointChange: boolean;
}

/**
 * サーバーから届いた view を反映するとき、画面に出す選択（純関数）。
 *
 * **未保存の変更があるときはサーバーの値で上書きしない**。`status`/`tags`/
 * `subscription`/`keyName` など他のフィールドは従来どおり上書きしてよい -
 * 問題になるのは編集中の `selected` だけ。
 *
 * ただし**未保存の下書きは接続先ごとのもの**（レビュー P2-2）。別の Hub に
 * 繋ぎ直すと `tags` は新しい Hub のものに入れ替わるのに、下書きだけが旧 Hub
 * のまま残り、**画面に出ていないタグ名が「選択を保存」の payload に混入した**
 * （保存は表示中のチェックボックスから組み直さず `selected` をそのまま送る）。
 * バックエンドは endpoint が変われば選択を引き継がない設計なので、フロント
 * だけがその境界を越えていた。
 *
 * 判定に使うのは**サーバーが返した endpoint 同士**（`selectionEndpoint` と
 * `view.endpoint`）で、入力欄の下書き（`endpointDraft`）とは比べない - まだ
 * 接続していない入力値なので、URL を打ち込んだ瞬間に「別の Hub」と判定して
 * しまう。`view.endpoint === null`（未設定・切断後）も「同じ Hub ではない」
 * 側に倒す。
 *
 * **catalog との積集合は取らない**: 同じ Hub で一時的に見えなくなっただけの
 * タグ（refresh が空振りした・権限が一瞬揺れた）の選択まで失う。判定は
 * **接続先の同一性のみ**。
 */
export function applyServerSelection(
	current: readonly string[],
	serverSelected: readonly string[],
	unsaved: boolean,
	selectionEndpoint: string | null,
	viewEndpoint: string | null
): ServerSelectionOutcome {
	const sameHub =
		selectionEndpoint !== null && viewEndpoint !== null && selectionEndpoint === viewEndpoint;
	if (unsaved && sameHub) {
		return { selected: [...current], unsaved: true, discardedForEndpointChange: false };
	}
	return {
		selected: [...serverSelected],
		unsaved: false,
		discardedForEndpointChange: unsaved
	};
}

/**
 * 接続先の変更で未保存の下書きを捨てたことを伝える一文。
 *
 * #378「未保存の入力を黙って捨てない」の対で、捨てるのが正しい場面でも
 * **捨てたことは画面に出す**。**保存成功の文言（`savedNotice`）とは別の行に
 * 出し、文言も混ぜない** - 「保存しました」の隣に出ると、捨てたものが保存
 * されたように読めてしまう。
 *
 * 「別の Hub に接続した」と言い切らないのは、接続に失敗して記録が消えた場合
 * （`view.endpoint === null`）も同じ経路を通るため - どちらも「この下書きの
 * 宛先がもう無い」であって、起きたことは同じ。
 */
export const SELECTION_DISCARDED_NOTICE =
	'接続先が変わったため、未保存だった選択は破棄しました。表示しているのは、今の接続先に保存されている選択です。';

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
 * 選択の編集状態（未保存フラグ）に何が起きたか。**サーバーからの反映
 * （`applyView`）はこの一覧に無い**: 同じ Hub なら未保存フラグを動かさず、
 * 別の Hub なら [`applyServerSelection`] が反映と同時にフラグを降ろすので、
 * 遷移の判断がこの関数と 2 箇所に分かれない。
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
export function hubSubscriptionDetail(
	subscription: HubSubscription,
	status: HubStatus | null = null
): string {
	const guidance = effectiveCredentialGuidance(status, subscription.lastError);
	if (subscription.state === 'stopped') {
		if (subscription.reason) return subscription.reason;
		// #446: 1008 の理由で止まっているなら「まもなく自動で再試行します」と
		// 一律に言わない（失効などは再試行しない）。
		if (guidance) return guidance.message;
		if (subscription.lastError) {
			return `購読は停止しています（エラー: ${subscription.lastError}）。まもなく自動で再試行します。`;
		}
		return '購読していません。';
	}
	if (subscription.state === 'unauthorized') {
		// #446: Hub が close 1008 で理由を付けて打ち切ったなら、理由ごとの
		// 案内（トリップは管理者に解除を依頼、失効などは新しいキー）を出す。
		// 接続の状態と食い違うときは接続の状態に合わせる
		// （`effectiveCredentialGuidance`）。
		if (guidance) return guidance.message;
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
