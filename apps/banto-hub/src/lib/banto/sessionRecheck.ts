/**
 * ストリームの close・再接続の失敗の後に、ログイン状態を確かめ直す（#441・#445）。
 *
 * banto v2.0.0（#260、設計 §6.2）から、確認そのものは SessionController の
 * 役目（`GET /api/auth/identity` の 1 往復、期限 10 秒、同時の確認は 1 本に
 * まとめる、答えは開始時の資格情報と照合してから採る、確認できなければ
 * 1 秒から倍々で 30 秒までの背景の確認）。v1 でここに持っていた独自の照合
 * （`/api/auth/check` の直接 `fetch`・開始時のトークンとの照合・single-flight・
 * 期限の `AbortController`）は削除した。ここに残るのは、ストリームの出来事を
 * controller に伝える配線と、試運転モードの扱い（policy runner）だけ。
 *
 * ## `1008` + `session_revoked` / `commissioning_ended` で閉じられた（#441）
 *
 * {@link recheckSessionAfterStreamClose}: `controller.signal('app:stream-closed')`
 * で失効の可能性を伝え、`invalidateAll()` でルートガード（`(app)/+layout.ts`）を
 * 走らせ直す。ガードは画面を開いたときと同じ経路（試運転の policy runner の
 * `guard` → 通常の確認）で判断する:
 *
 * - 失効を確認できた（確定した `none`）→ `/login` へ移る
 * - 照合できなかった（`500`・到達不能・期限切れ）→ 再試行付きのエラー画面
 *   （`routes/+error.svelte`）。保存しているトークンは消さない
 * - まだ有効 → 画面はそのまま（呼び出し側がストリームを再開する）
 * - `commissioning_ended`: ガードの policy runner がロックダウンを確認して
 *   試運転の合成セッションを `end` し、通常の確認へ進む（トークンが無ければ
 *   `/login`）
 *
 * signal の後に始めた確認だけがこの要求に答える（controller の I-9）ので、
 * 複数のストリームが同時に閉じても確認はまとまる（v1 の single-flight は不要）。
 * 試運転の合成セッションの間（adopt 中）は signal は provider に問い合わせない
 * （S-45）。終了の判断はガードの policy runner が行う。
 *
 * ## 再接続が続けて失敗した（#445）
 *
 * {@link probeSessionAfterReconnectFailures}: **画面を動かさずに**確かめ、
 * `session` / `login` / `unverified` を返す（写し方は `streamClose.ts` の
 * `sessionProbeResultOf`、扱いは同じファイルの `decideAfterSessionProbe`）。
 *
 * - 試運転の合成セッションが確定している間: policy runner の `recheck`
 *   （試運転の状態が読めなければ `unverified` で画面を保つ。ロックダウンが
 *   確定していれば `end` してから通常の確認）。
 * - それ以外: `resolveSettled(controller, { cause: 'signal' })`（要求自体が
 *   signal の stamp を進める、S-58）。確認できなければ `unverified`（画面と
 *   トークンはそのまま。controller は背景の確認を続け、その間に `none` が
 *   確定すれば保護レイアウトの配線①が /login へ送る）。
 *
 * 失効を確認できた（`login`）ときは、呼び出し側（`connectTagStream`）が
 * `onHalt` → 上の {@link recheckSessionAfterStreamClose} へ合流する。`none` の
 * 確定で generation が動くので、配線①の `invalidateAll()` とここの
 * `invalidateAll()` が重なることがあるが、どちらもガードを走らせ直すだけで
 * 無害（SvelteKit は後の呼び出しを勝たせる）。
 */
import { invalidateAll } from '$app/navigation';
import { getSessionController, resolveSettled } from '@banto/admin-core';
import { isCommissioningSession, runCommissioningPolicy } from '$lib/banto/commissioningPolicy';
import { sessionProbeResultOf, type SessionProbeResult } from './streamClose';

/** ストリームが `1008` + `session_revoked` / `commissioning_ended` で閉じられた（#441）。 */
export async function recheckSessionAfterStreamClose(): Promise<void> {
	getSessionController().signal('app:stream-closed');
	await invalidateAll();
}

/**
 * 再接続が続けて失敗したときの確認（#445）。画面も保存しているトークンも
 * 動かさず、`session` / `login` / `unverified` を返す。reject しない。
 */
export async function probeSessionAfterReconnectFailures(): Promise<SessionProbeResult> {
	const controller = getSessionController();
	const result = isCommissioningSession(controller.snapshot)
		? await runCommissioningPolicy(controller, { mode: 'recheck' })
		: await resolveSettled(controller, { cause: 'signal' });
	return sessionProbeResultOf(result);
}
