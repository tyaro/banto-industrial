/**
 * 資格情報なしで入る口の順序（#392 B2-2、設計書 docs/chronogazer-service-design.md
 * §3.2・§4 の B2-2 の実装メモ）。`(app)` のガード（`routes/(app)/+layout.ts`）が、
 * 確定した `none` のときにこれを呼ぶ。
 *
 * 1. **`localDesktop`（この PC からの接続、B2-1）**: サービスの host
 *    （`chronogazer-server`）が、この PC からの要求（peer と Host の両方が loopback）
 *    にだけ開く。未初期化の間は admin、初期化後は設定 `auth.local_grant` の role。
 *    `status().grants.localDesktop` が `true` のときだけ入る（未初期化でも初期化済み
 *    でも同じ。2026-10-14 オーナー決定 1: 最初の管理者がいない間もログインなしで
 *    入り、画面の上の案内から初回セットアップへ誘う）。
 * 2. **`publicViewer`（閲覧公開、ADR-0012）**: `server.viewerPublic` が ON のとき。
 *
 * 順序は `localDesktop` が先（オーナー決定 2）: この PC からは、閲覧だけの公開より
 * 強い（設定どおりの）権限で入れるほうを選ぶ。どちらも `grantFallback`（banto
 * admin-core v6.7.0）に任せ、`unverified` はそのまま返し（呼び出し側が 503）、
 * 1 つ目の結果が確定した `none` のときだけ、その ticket で 2 つ目を試す。
 *
 * サーバーの判定（status と発行で共有の 1 つの関数）が本当の境界で、ここは入る口を
 * 選ぶだけ。Tauri・demo の provider は `enterGrant` を持たないので何もしない。
 */
import {
	grantFallback,
	resolveSettled,
	type AuthProvider,
	type GrantStatus,
	type ResolveResult,
	type SessionController,
	type SessionTicket
} from '@banto/admin-core';

/** この PC からの接続の grant の kind（サーバーの `LOCAL_DESKTOP_GRANT_KIND` と同じ）。 */
export const LOCAL_DESKTOP_GRANT_KIND = 'localDesktop';

/** 閲覧公開の grant の kind（banto ADR-0012）。 */
export const PUBLIC_VIEWER_GRANT_KIND = 'publicViewer';

/** `status()` が `localDesktop` を使えると言っているか（無い・`false` は使えない）。 */
export function localDesktopAvailable(status: GrantStatus | undefined): boolean {
	return status?.grants?.[LOCAL_DESKTOP_GRANT_KIND] === true;
}

/**
 * 最初の管理者がまだいない（未初期化）ことを、`localDesktop` のセッションに案内するか
 * （画面の上の帯。`SetupBanner.svelte`）。`status` が読めない・初期化済みなら出さない。
 */
export function needsFirstAdmin(
	localDesktopSession: boolean,
	status: GrantStatus | undefined
): boolean {
	return localDesktopSession && status !== undefined && status.initialized === false;
}

/**
 * 確定した `none`（`ticket`）から、資格情報なしの口へ順に入る（モジュール doc）。
 * 返すのは `grantFallback` と同じ `confirmed` か `unverified`。
 */
export async function enterCredentiallessGrant(
	controller: SessionController,
	provider: Pick<AuthProvider, 'status' | 'enterGrant'>,
	ticket: SessionTicket
): Promise<Exclude<ResolveResult, { outcome: 'superseded' }>> {
	const local = await grantFallback(controller, provider, ticket, {
		kind: LOCAL_DESKTOP_GRANT_KIND,
		available: localDesktopAvailable
	});
	if (local.outcome === 'unverified' || local.snapshot.status !== 'none') return local;
	return grantFallback(controller, provider, local.ticket, {
		kind: PUBLIC_VIEWER_GRANT_KIND
	});
}

/**
 * ログイン画面のボタンで、資格情報なしの口 `kind` を**明示的に選ぶ**（#392 B2-2 の
 * レビュー、オーナー P2-1・P2・Copilot）。ガードの自動の順序（`localDesktop` が先）に
 * 任せて `/monitor` へ移るだけだと、両方使えるときにガードが `localDesktop`（admin に
 * なりうる）を発行し、利用者が選んだ閲覧の制限が効かなかった。ここでセッションを確定
 * させてから移れば、ガードは既にあるセッションを上書きしない（`grantFallback` は確定した
 * `none` のときだけ呼ばれる）。
 *
 * - 既に `kind` のセッションなら、そのまま返す。
 * - 別の active なセッション（`localDesktop`・普通のアカウント。ブラウザの「戻る」で
 *   ログイン画面に来た場合など）は、ボタンを押した = 切り替える明示の操作として、まず
 *   provider の `logout()`（サーバーのトークンも失効させる）で閉じ、確定し直してから
 *   `kind` に入る。
 * - ログイン不要モードの合成のセッション（`kind: 'local'`。デスクトップの窓だけ）は
 *   ログアウトで閉じられないので、切り替えずにそのまま返す。
 *
 * 呼び出し側は結果が `kind` のセッションか（[`enteredKind`]）を確かめてから移る。
 */
export async function enterGrantExplicitly(
	controller: SessionController,
	provider: Pick<AuthProvider, 'status' | 'enterGrant' | 'logout'>,
	kind: string,
	available?: (status: GrantStatus) => boolean
): Promise<Exclude<ResolveResult, { outcome: 'superseded' }>> {
	let current = await resolveSettled(controller, { cause: 'navigation' });
	if (current.outcome === 'unverified') return current;
	if (current.snapshot.status === 'active') {
		if (current.snapshot.kind === kind || current.snapshot.kind === LOGIN_FREE_KIND) {
			return current;
		}
		try {
			await provider.logout();
		} catch {
			// 閉じられたかは下の確定で決める。
		}
		current = await resolveSettled(controller, { cause: 'signal' });
		if (current.outcome === 'unverified' || current.snapshot.status !== 'none') return current;
	}
	if (current.snapshot.status !== 'none') return current;
	return grantFallback(controller, provider, current.ticket, { kind, available });
}

/** ログイン不要モードの合成のセッションの kind（ログアウトでは閉じない）。 */
const LOGIN_FREE_KIND = 'local';

/** 「閲覧のみで続ける」: 閲覧公開を明示的に選ぶ（[`enterGrantExplicitly`]）。 */
export function enterPublicViewer(
	controller: SessionController,
	provider: Pick<AuthProvider, 'status' | 'enterGrant' | 'logout'>
): Promise<Exclude<ResolveResult, { outcome: 'superseded' }>> {
	return enterGrantExplicitly(controller, provider, PUBLIC_VIEWER_GRANT_KIND);
}

/** 「この PC として続ける」: この PC からの接続を明示的に選ぶ（[`enterGrantExplicitly`]）。 */
export function enterThisPc(
	controller: SessionController,
	provider: Pick<AuthProvider, 'status' | 'enterGrant' | 'logout'>
): Promise<Exclude<ResolveResult, { outcome: 'superseded' }>> {
	return enterGrantExplicitly(
		controller,
		provider,
		LOCAL_DESKTOP_GRANT_KIND,
		localDesktopAvailable
	);
}

/** 結果が `kind` の確定した active なセッションか（呼び出し側はこれが真のときだけ移る）。 */
export function enteredKind(result: ResolveResult, kind: string): boolean {
	return (
		result.outcome === 'confirmed' &&
		result.snapshot.status === 'active' &&
		result.snapshot.kind === kind
	);
}

/** ログイン画面（`routes/login/+page.svelte`）に出す、資格情報なしの口と初回セットアップの選択。 */
export interface LoginEntries {
	/** 「この PC として続ける」（`localDesktop`、オーナー決定 3）。 */
	continueAsThisPc: boolean;
	/** 「閲覧のみで続ける」（`publicViewer`、I2b）。 */
	continueAsViewer: boolean;
	/**
	 * 初回セットアップの「この PC からはログインなしで使う（管理者）」（オーナー決定 7、
	 * 既定でチェック）。未初期化で `localDesktop` が使えるとき（= サービスの host の、この
	 * PC からの画面）だけ。
	 */
	localGrantOptIn: boolean;
	/**
	 * ログインの欄の上の「単体ブラウザ（デモ）モードは admin / admin でログインできます」
	 * （#612）。demo だけ - サービス・LAN の画面（`server`）や Tauri の窓で出すと、その
	 * アカウントがあるように読めて誤解を招く。初期化済み（または `status` が無い）のとき
	 * だけ（ログインの欄が出る条件と同じ）。
	 */
	demoAccountHint: boolean;
}

/**
 * ログイン画面の出し分け（純関数）。資格情報なしの口と初回セットアップの選択は
 * LAN/サービスの HTTP の画面（`server` モード）だけで、Tauri の窓・demo には出さない
 * （Tauri の窓のログイン不要は M11 の受け持ち、demo には発行するバックエンドが無い）。
 * `status` が読めなければ出さない。デモのアカウントの案内は demo だけ（#612）。
 */
export function loginEntries(
	mode: 'tauri' | 'server' | 'demo',
	status: GrantStatus | undefined
): LoginEntries {
	const demoAccountHint = mode === 'demo' && (status === undefined || status.initialized);
	if (mode !== 'server' || status === undefined) {
		return {
			continueAsThisPc: false,
			continueAsViewer: false,
			localGrantOptIn: false,
			demoAccountHint
		};
	}
	const local = localDesktopAvailable(status);
	return {
		continueAsThisPc: status.initialized && local,
		continueAsViewer: status.grants?.[PUBLIC_VIEWER_GRANT_KIND] === true,
		localGrantOptIn: !status.initialized && local,
		demoAccountHint
	};
}
