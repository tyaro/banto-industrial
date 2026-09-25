/**
 * ストリームが `1008` + `session_revoked` / `commissioning_ended` で閉じられた
 * ときに、ログイン状態を確かめ直す（#441）。
 *
 * 新しい判断は持たない: `invalidateAll()` で `(app)/+layout.ts` のルート
 * ガードを走らせ直すだけ。ガードは画面を開いたときと同じ経路
 * （`fetchCommissioningStatusOrNull` → `decideProtectedRoute` =
 * `resolveProtectedSession`、#436）で判断する:
 *
 * - 失効を確認できた（`401` など）→ `/login` へ移る
 * - 照合できなかった（`500`・到達不能）→ 再試行付きのエラー画面
 *   （`routes/+error.svelte`）。保存しているトークンは消さない
 * - まだ有効 → 画面はそのまま（呼び出し側がストリームを再開する）
 * - `commissioning_ended`: 試運転モードではなくなったので、ガードは
 *   ログインを求める側へ進む（トークンが無ければ `/login`）
 *
 * {@link createSingleFlight} で包み、複数のストリームが同時に閉じられても
 * 確認（= 画面の移動・エラー表示）は 1 回にする。
 *
 * **再接続が続けて失敗したとき（#445）** は {@link probeSessionAfterReconnectFailures}
 * で先に確かめる。こちらは**画面を動かさない**（結果を返すだけ）: 再接続が
 * 失敗するのは多くはネットワークの一時的な切断・サーバーの再起動で、その
 * ときに上の `invalidateAll()` を走らせると、照合できないのでエラー画面へ
 * 移ってしまう（一時的な切断でモニタを失う）。判断はルートガードと同じ部品
 * （`fetchCommissioningStatusOrNull` → `shouldBypassLoginForCommissioning` →
 * `decideProtectedRoute`）で行い、失効を確認できた（`login`）ときだけ、
 * 呼び出し側（`connectTagStream`）が `onHalt` → 上の
 * {@link recheckSessionAfterStreamClose} へ合流する（ガードが改めて判断して
 * `/login` へ送る）。`1008` の経路と同時に起きても、画面の移動は同じ
 * single-flight で 1 回になる。
 */
import { invalidateAll } from '$app/navigation';
import { getAuthProvider } from '@banto/admin-core';
import {
	fetchCommissioningStatusOrNull,
	shouldBypassLoginForCommissioning
} from '$lib/banto/commissioning';
import { decideProtectedRoute } from '$lib/banto/sessionGuard';
import { sessionStore } from '$lib/session.svelte';
import { createSingleFlight, type SessionProbeResult } from './streamClose';

export const recheckSessionAfterStreamClose: () => Promise<void> = createSingleFlight(() =>
	invalidateAll()
);

/**
 * 確認がこの時間で終わらなければ `unverified`（照合できない）として扱う。
 * ネットワークが切れている間の要求は、失敗せずに返ってこないことがある。
 * 上限が無いと飛行中のまま次の確認が起こせず、失効に気づけなくなる。
 */
export const SESSION_PROBE_TIMEOUT_MS = 10_000;

async function probeOnce(): Promise<SessionProbeResult> {
	const status = await fetchCommissioningStatusOrNull();
	// ルートガードは取得の失敗を「ロックダウン済み」に倒す（安全側）が、
	// ここでは画面を動かすかどうかの判断なので「照合できない」にする。
	// 試運転モード中にネットワークが切れただけでログイン画面へ送らないため。
	if (status === null) return 'unverified';
	if (shouldBypassLoginForCommissioning(status)) return 'session';
	// ストリームは試運転モードのつもり（資格情報なしで `/api/tag-stream`）で
	// 繋ごうとしているが、サーバーはロックダウン済み: ガードを走らせ直さない
	// と繋がらない（`commissioning_ended` と同じ）。ガードがログインを求める
	// 側へ進める。
	if (sessionStore.commissioningMode) return 'login';
	return decideProtectedRoute(getAuthProvider());
}

function withTimeout(
	run: () => Promise<SessionProbeResult>,
	timeoutMs: number
): Promise<SessionProbeResult> {
	return new Promise((resolve) => {
		const timer = setTimeout(() => resolve('unverified'), timeoutMs);
		run().then(
			(result) => {
				clearTimeout(timer);
				resolve(result);
			},
			() => {
				clearTimeout(timer);
				resolve('unverified');
			}
		);
	});
}

/**
 * 再接続が続けて失敗したときの確認（#445）。画面は動かさず、ルートガードと
 * 同じ 3 分類（`session` / `login` / `unverified`）を返す。single-flight。
 */
export const probeSessionAfterReconnectFailures: () => Promise<SessionProbeResult> =
	createSingleFlight(() => withTimeout(probeOnce, SESSION_PROBE_TIMEOUT_MS));
