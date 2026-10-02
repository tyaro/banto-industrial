/**
 * 試運転モードのロックダウンの後、ログイン画面へ移る順序（banto v2.0.0 #260、
 * 設計 §6.2・S-45・S-62）。設定画面のセキュリティカテゴリ
 * （`SecuritySection.svelte`）が使う。
 *
 * admin-template 流のログアウト（`logoutAndLeave`: `logout()` →
 * `resolveSettled()` → `none` なら /login）に置き換えては**いけない**: 試運転の
 * 合成セッションは `adopt` で確定しているので、adopt 中の `resolveSettled()` は
 * provider に問い合わせず常に試運転のセッションを `confirmed` で返す（S-45）。
 * ログイン画面へ行けず「ログアウトできませんでした」になる。終わらせるのは
 * アプリの方針の `end()` だけ。
 *
 * 順序:
 *
 * 1. `controller.ticket()` を**ロックダウンの前に**取る（adopt 中なので epoch
 *    だけ、I-21。別タブのログインの `storage` イベントでは失効しない、S-62）。
 * 2. `lockDown()`（`POST /api/commissioning/lock-down`）。失敗したら投げ直す
 *    （何も終わらせない。呼び出し側がエラーを表示する）。
 * 3. ここから /login への遷移が終わるまで、保護レイアウトの配線①の
 *    `invalidateAll()` を止める（`leaveForLogin`）。`end()` で generation が
 *    動くので、止めないと配線①の再 load が `goto('/login')` に勝つ
 *    （banto の E2E「5a」と同型、`logout.svelte.ts` の doc）。
 * 4. `end('commissioning-locked', ticket)`。`false`（ロックダウンの間に別の判定が
 *    先に adopt し直した＝ epoch が進んだ）なら、まだ試運転の合成セッションが
 *    確定していることを確かめてから、新しい ticket でやり直す（上限あり）。
 *    すでに試運転でなければ（ガードの policy runner が先に `end` した）終わらせる
 *    ものは無い。
 * 5. `resolveSettled(controller, { cause: 'signal' })` で今の資格情報について
 *    確定する。確定した `none` のときだけ /login へ移る（`'left'`）。保存して
 *    いたトークンが有効なら、そのアカウントで続ける（`'stayed'`、配線①が画面を
 *    作り直す）。確認できなければ `'unverified'`（配線①の再 load が 503 の
 *    再試行画面にする）。
 *
 * v1 は `lockDown()` の後に `getAuthProvider().logout()` で保存していたトークンを
 * 消してから /login へ移っていた。v2 では消さない（試運転の終了はトークンの
 * 消去ではない。トークンが有効ならそのアカウントのセッションとして確定する）。
 */
import { getSessionController, resolveSettled, type SessionController } from '@banto/admin-core';
import { leaveForLogin } from './logout.svelte';
import { COMMISSIONING_LOCKED_REASON, isCommissioningSession } from './commissioningPolicy';

/** ロックダウンの後の行き先（`logout.svelte.ts` の `LogoutOutcome` と同じ 3 値）。 */
export type LockDownOutcome = 'left' | 'stayed' | 'unverified';

/** `end()` のやり直しを含めた回数の上限。 */
export const LOCK_DOWN_END_MAX_ROUNDS = 3;

export interface LockDownAndLeaveOptions {
	/** `POST /api/commissioning/lock-down`。reject はそのまま投げ直す。 */
	lockDown: () => Promise<unknown>;
	/** /login へ移る（`() => goto('/login')`）。 */
	goToLogin: () => Promise<void>;
	controller?: SessionController;
	maxRounds?: number;
}

/**
 * 試運転の合成セッションを `end` する。`ticket` が失効していたら、まだ試運転の
 * 間だけ新しい ticket でやり直す。終わらせた（またはもう試運転ではない）なら
 * `true`、上限に達したら `false`。
 */
export function endCommissioningSession(
	controller: SessionController,
	ticket: ReturnType<SessionController['ticket']>,
	maxRounds: number = LOCK_DOWN_END_MAX_ROUNDS
): boolean {
	let t = ticket;
	for (let round = 0; round < maxRounds; round++) {
		if (!isCommissioningSession(controller.snapshot)) return true;
		if (controller.end(COMMISSIONING_LOCKED_REASON, t)) return true;
		t = controller.ticket();
	}
	return !isCommissioningSession(controller.snapshot);
}

export async function lockDownAndLeave(options: LockDownAndLeaveOptions): Promise<LockDownOutcome> {
	const controller = options.controller ?? getSessionController();
	const ticket = controller.ticket();
	await options.lockDown();

	let outcome: LockDownOutcome = 'unverified';
	await leaveForLogin(async () => {
		if (!endCommissioningSession(controller, ticket, options.maxRounds)) {
			// 試運転のセッションが動き続けた（実際には起きない: ticket と end は同期）。
			outcome = 'unverified';
			return;
		}
		const result = await resolveSettled(controller, { cause: 'signal' });
		if (result.outcome === 'confirmed' && result.snapshot.status === 'none') {
			await options.goToLogin();
			outcome = 'left';
			return;
		}
		outcome = result.outcome === 'unverified' ? 'unverified' : 'stayed';
	});
	return outcome;
}
