/**
 * ストリームの close・再接続の失敗の後に、ログイン状態を確かめ直す（#441・#445）。
 *
 * 確認そのものは SessionController の役目（`GET /api/auth/identity` の 1 往復、
 * 期限 10 秒、同時の確認は 1 本にまとめる、答えは開始時の資格情報と照合してから
 * 採る、確認できなければ 1 秒から倍々で 30 秒までの背景の確認）。ここに残るのは、
 * ストリームの出来事を controller に伝える配線だけ。banto v3.0.0（ADR-0017）から
 * 試運転モードも grant のトークンによる通常のセッションなので、試運転専用の分岐
 * （policy runner）は無い。
 *
 * ## `1008` + `session_revoked` で閉じられた（#441）
 *
 * {@link recheckSessionAfterStreamClose}: `controller.signal('app:stream-closed')`
 * で失効の可能性を伝え、`refreshAll()` でルートガード（`(app)/+layout.ts`）を
 * 走らせ直す:
 *
 * - 失効を確認できた（確定した `none`）→ ガードが `grantFallback` を試み、試運転の
 *   grant が出せれば（未ロックダウン かつ loopback）新しい grant で続け、出せなければ
 *   `/login` へ移る（ロックダウンで grant が失効した場合）
 * - 照合できなかった（`500`・到達不能・期限切れ）→ 再試行付きのエラー画面
 *   （`routes/+error.svelte`）。保存しているトークンは消さない
 * - まだ有効 → 画面はそのまま（呼び出し側がストリームを再開する）
 *
 * signal の後に始めた確認だけがこの要求に答える（controller の I-9）ので、
 * 複数のストリームが同時に閉じても確認はまとまる。
 *
 * ## 再接続が続けて失敗した（#445）
 *
 * {@link probeSessionAfterReconnectFailures}: **画面を動かさずに**
 * `resolveSettled(controller, { cause: 'signal' })`（要求自体が signal の stamp を
 * 進める、S-58）で確かめ、`session` / `login` / `unverified` を返す（写し方は
 * `streamClose.ts` の `sessionProbeResultOf`、扱いは同じファイルの
 * `decideAfterSessionProbe`）。確認できなければ `unverified`（画面とトークンは
 * そのまま。controller は背景の確認を続け、その間に `none` が確定すれば保護
 * レイアウトの配線①が /login へ送る）。試運転の grant も通常のセッションと同じ:
 * 失効が確定すれば `login` を返し、ガードが grant を再発行するか /login へ送る。
 *
 * 失効を確認できた（`login`）ときは、呼び出し側（`connectTagStream`）が
 * `onHalt` → 上の {@link recheckSessionAfterStreamClose} へ合流する。`none` の
 * 確定で generation が動くので、配線①の `refreshAll()` とここの
 * `refreshAll()` が重なることがあるが、どちらもガードを走らせ直すだけで
 * 無害（SvelteKit は後の呼び出しを勝たせる）。
 */
import { refreshAll } from '$app/navigation';
import { getSessionController, resolveSettled } from '@banto/admin-core';
import { sessionProbeResultOf, type SessionProbeResult } from './streamClose';

/** ストリームが `1008` + `session_revoked` で閉じられた（#441）。 */
export async function recheckSessionAfterStreamClose(): Promise<void> {
	getSessionController().signal('app:stream-closed');
	await refreshAll();
}

/**
 * 再接続が続けて失敗したときの確認（#445）。画面も保存しているトークンも
 * 動かさず、`session` / `login` / `unverified` を返す。reject しない。
 */
export async function probeSessionAfterReconnectFailures(): Promise<SessionProbeResult> {
	const result = await resolveSettled(getSessionController(), { cause: 'signal' });
	return sessionProbeResultOf(result);
}
