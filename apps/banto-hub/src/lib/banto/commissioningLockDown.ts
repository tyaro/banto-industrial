/**
 * 試運転モードのロックダウンの後、ログイン画面へ移る順序（banto v3.0.0、
 * ADR-0017）。設定画面のセキュリティカテゴリ（`SecuritySection.svelte`）が使う。
 *
 * v3.0.0 から試運転のセッションは `adopt` ではなく、サーバーが発行した grant の
 * トークンで確定する通常のセッション（kind `commissioning`）。サーバーは
 * `POST /api/commissioning/lock-down` の保存の直後に、`commissioning` の grant の
 * トークンを**全部失効**させる。だからロックダウンの後の確認は、通常のログアウトと
 * 同じ形で足りる（`end()` も ticket の事前取得も無い）。
 *
 * 順序:
 *
 * 1. `lockDown()`（`POST /api/commissioning/lock-down`）。失敗したら投げ直す
 *    （何も変えない。呼び出し側がエラーを表示する）。
 * 2. ここから /login への遷移が終わるまで、保護レイアウトの配線①の
 *    `refreshAll()` を止める（`leaveForLogin`）。確定で generation が動くので、
 *    止めないと配線①の再 load が `goto('/login')` に勝つ（banto の E2E「5a」と
 *    同型、`logout.svelte.ts` の doc）。
 * 3. `resolveSettled(controller, { cause: 'signal' })` で今の資格情報について
 *    確定する。自分の grant のトークンはサーバー側で失効済みなので、
 *    `GET /api/auth/identity` が 401 になり、provider がトークンを消して確定した
 *    `none` になる。確定した `none` のときだけ /login へ移る（`'left'`）。保存して
 *    いたアカウントのトークンが有効なら、そのアカウントで続ける（`'stayed'`、
 *    配線①が画面を作り直す）。確認できなければ `'unverified'`（配線①の再 load が
 *    503 の再試行画面にする）。
 *
 * /login へ移った後に保護画面へ戻ると、ガードの `grantFallback` は
 * `grants.commissioning` が false なので何も発行せず、`none` のまま /login になる。
 */
import { getSessionController, resolveSettled, type SessionController } from '@banto/admin-core';
import { leaveForLogin } from './logout.svelte';

/** ロックダウンの後の行き先（`logout.svelte.ts` の `LogoutOutcome` と同じ 3 値）。 */
export type LockDownOutcome = 'left' | 'stayed' | 'unverified';

export interface LockDownAndLeaveOptions {
	/** `POST /api/commissioning/lock-down`。reject はそのまま投げ直す。 */
	lockDown: () => Promise<unknown>;
	/** /login へ移る（`() => goto(resolveAppPath('/login'))`）。 */
	goToLogin: () => Promise<void>;
	controller?: SessionController;
}

export async function lockDownAndLeave(options: LockDownAndLeaveOptions): Promise<LockDownOutcome> {
	const controller = options.controller ?? getSessionController();
	await options.lockDown();

	let outcome: LockDownOutcome = 'unverified';
	await leaveForLogin(async () => {
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
