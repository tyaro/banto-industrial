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
 * で先に確かめる。こちらは**画面も認証の状態も動かさない**（結果を返すだけ）:
 *
 * - 再接続が失敗するのは多くはネットワークの一時的な切断・サーバーの再起動で、
 *   そのときに上の `invalidateAll()` を走らせると、照合できないのでエラー画面
 *   へ移ってしまう（一時的な切断でモニタを失う）。
 * - 照合は `AuthProvider.check()` を**使わない**（#447 のレビュー）。`check()`
 *   は `401` を受けると**その時点で保存されている**トークンを消す。時間切れで
 *   見捨てた確認に遅れて `401` が返ると、トークンだけが消えてログイン画面へ
 *   移らないまま待ち続ける、または確認の最中にログインし直した新しい
 *   トークンまで消す。そこで確認は、開始時に読んだトークンで
 *   `/api/auth/check` を自分で呼び、結果を分類するだけにする
 *   （{@link classifySessionCheckResponse}、`check()` と同じ分類）。時間切れ
 *   では `AbortController` で要求そのものを止める。確認の最中にトークンが
 *   変わっていたら、その結果は古いトークンについての答えなので `unverified`
 *   にする（次の失敗でまた確かめる）。
 * - 試運転モードの状態は `fetchCommissioningStatusOrNull`（副作用なし）で読み、
 *   ルートガードと同じ `shouldBypassLoginForCommissioning` で判断する。
 *
 * 失効を確認できた（`login`）ときだけ、呼び出し側（`connectTagStream`）が
 * `onHalt` → 上の {@link recheckSessionAfterStreamClose} へ合流する（ガードが
 * 改めて**その時点のトークンで**判断して `/login` へ送る。トークンを消すのも
 * ガードの `check()`）。`1008` の経路と同時に起きても、画面の移動は同じ
 * single-flight で 1 回になる。
 */
import { invalidateAll } from '$app/navigation';
import { getAuthProvider } from '@banto/admin-core';
import {
	fetchCommissioningStatusOrNull,
	shouldBypassLoginForCommissioning
} from '$lib/banto/commissioning';
import { CSRF_HEADER } from '$lib/banto/setup';
import { sessionStore } from '$lib/session.svelte';
import {
	classifySessionCheckResponse,
	createSingleFlight,
	type SessionProbeResult
} from './streamClose';

export const recheckSessionAfterStreamClose: () => Promise<void> = createSingleFlight(() =>
	invalidateAll()
);

/**
 * 確認がこの時間で終わらなければ `unverified`（照合できない）として扱い、
 * 要求を止める。ネットワークが切れている間の要求は、失敗せずに返ってこない
 * ことがある。上限が無いと飛行中のまま次の確認が起こせず、失効に気づけなくなる。
 */
export const SESSION_PROBE_TIMEOUT_MS = 10_000;

/** 保存されているトークン（`createHttpAuthProvider` の `getToken`）。読むだけ。 */
function storedToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

async function probeOnce(signal: AbortSignal): Promise<SessionProbeResult> {
	// 開始時点の事実で判断する（途中で変わったら、下で結果を捨てる）。
	const assumedCommissioning = sessionStore.commissioningMode;
	const token = storedToken();

	const status = await fetchCommissioningStatusOrNull();
	if (signal.aborted) return 'unverified';
	// ルートガードは取得の失敗を「ロックダウン済み」に倒す（安全側）が、
	// ここでは画面を動かすかどうかの判断なので「照合できない」にする。
	// 試運転モード中にネットワークが切れただけでログイン画面へ送らないため。
	if (status === null) return 'unverified';
	if (shouldBypassLoginForCommissioning(status)) return 'session';
	// ストリームは試運転モードのつもり（資格情報なしで `/api/tag-stream`）で
	// 繋ごうとしているが、サーバーはロックダウン済み: ガードを走らせ直さない
	// と繋がらない（`commissioning_ended` と同じ）。ガードがログインを求める
	// 側へ進める。
	if (assumedCommissioning) return 'login';
	if (token === null) return 'login';

	let result: SessionProbeResult;
	try {
		const response = await fetch('/api/auth/check', {
			method: 'GET',
			headers: { ...CSRF_HEADER, Authorization: `Bearer ${token}` },
			signal
		});
		let body: unknown = undefined;
		if (response.ok) {
			try {
				body = await response.json();
			} catch {
				body = undefined;
			}
		}
		result = classifySessionCheckResponse(response.status, body);
	} catch {
		// 到達不能、または時間切れで止めた。
		return 'unverified';
	}
	// 確認の最中にトークンが変わった（ログインし直した・ほかの経路が消した）:
	// この答えは古いトークンについてのもの。
	if (storedToken() !== token) return 'unverified';
	return result;
}

function withTimeout(
	run: (signal: AbortSignal) => Promise<SessionProbeResult>,
	timeoutMs: number
): Promise<SessionProbeResult> {
	const controller = new AbortController();
	return new Promise((resolve) => {
		const timer = setTimeout(() => {
			controller.abort();
			resolve('unverified');
		}, timeoutMs);
		run(controller.signal).then(
			(result) => {
				clearTimeout(timer);
				resolve(controller.signal.aborted ? 'unverified' : result);
			},
			() => {
				clearTimeout(timer);
				resolve('unverified');
			}
		);
	});
}

/**
 * 再接続が続けて失敗したときの確認（#445）。画面も保存しているトークンも
 * 動かさず、`session` / `login` / `unverified` を返す。single-flight。
 */
export const probeSessionAfterReconnectFailures: () => Promise<SessionProbeResult> =
	createSingleFlight(() => withTimeout(probeOnce, SESSION_PROBE_TIMEOUT_MS));
