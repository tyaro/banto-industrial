/**
 * ストリームの close を受けたときの判断（#441）。
 *
 * #430（PR #439）から、サーバーは開いているストリーム（`/api/tag-stream`・
 * `/api/v1/stream`）の資格情報を 15 秒ごとに照合し直し、使えないと**確認
 * できたら** close コード `1008`（Policy Violation、`stream.rs` の
 * `REVOKED_CLOSE_CODE`）と理由文で閉じる。以前の画面はこれを通常の切断と
 * 同じく再接続していたので、失効したセッションでは認証で拒否される再接続が
 * 続き、利用者に理由も伝わらなかった。
 *
 * 判断は close コードと理由文だけで決まる純関数 {@link classifyStreamClose}
 * にまとめ、`streamClose.test.ts` で表にして確かめる:
 *
 * | close | 理由文 | 扱い |
 * | --- | --- | --- |
 * | `1008` | `session_revoked` | 再接続しない。ログイン状態を確かめ直す（`recheckSession`） |
 * | `1008` | `commissioning_ended` | 同上（ロックダウンされたのでログインが要る状態へ） |
 * | `1008` | `api_key_*`・未知・空 | 再接続しない。理由を画面に出す（`halt`） |
 * | `1008` 以外（`1006`・`1000`・`1013` など） | 何でも | 従来どおり再接続（`reconnect`） |
 *
 * `1008` 以外を一律に再接続にしているのは、この変更の前の挙動をそのまま
 * 残すため（`1013` のバックプレッシャ切断・ネットワークの一時的な切断・
 * サーバーの再起動はどれも再接続で戻る）。
 *
 * **切れている間の失効（#445）**: ストリームがすでに切れている間に
 * セッションが失効すると、再接続のハンドシェイクが認証で拒否される。
 * ブラウザの WebSocket API からは拒否の理由が見えず `1006` にしかならない
 * ので、上の表だけでは通常の切断として再接続を繰り返す。そこで「開く前に
 * 閉じた」再接続（= 失敗した再接続）を数え、続けて
 * {@link RECONNECT_FAILURES_BEFORE_SESSION_PROBE} 回失敗したらログイン状態を
 * 確かめる（{@link shouldProbeSession}）。確かめた結果の扱いは
 * {@link decideAfterSessionProbe}:
 *
 * | 確認の結果 | 扱い | 続けて失敗した回数 |
 * | --- | --- | --- |
 * | `login`（失効を確認できた） | 再接続をやめ、`1008` + `session_revoked` と同じ `recheckSession` へ合流する（ルートガードが `/login` へ送る） | - |
 * | `session`（まだ有効） | 再接続を続ける | 0 に戻す（次の確認はまた 2 回失敗してから） |
 * | `unverified`（照合できない・到達不能・時間切れ） | 再接続を続ける（バックオフはそのまま伸びる） | そのまま（次の失敗でまた確かめる = 確認は再接続 1 回につき高々 1 回で、間隔はバックオフ以上） |
 *
 * 接続が開いたら（`onopen`）数え直す。確認はバックオフの待ちと並行に走らせ、
 * 待ちそのものは短くも長くもしない。
 */

import type { ProtectedRouteDecision } from './sessionGuard';

/** 資格情報が使えないと確認できたときの close コード（`stream.rs` の `REVOKED_CLOSE_CODE`）。 */
export const REVOKED_CLOSE_CODE = 1008;

/**
 * ログイン状態を確かめ直す理由。`session_revoked` / `commissioning_ended` は
 * サーバーの close の理由文。`reconnect_rejected`（#445）はクライアントが
 * 付ける: 再接続が続けて拒否され、確かめたら失効していた。サーバーが
 * この理由文で閉じることは無い（{@link classifyStreamClose} は受け付けない）。
 */
export type SessionRecheckReason = 'session_revoked' | 'commissioning_ended' | 'reconnect_rejected';

export type StreamCloseAction =
	/** 通常の切断。従来どおり再接続する。 */
	| { kind: 'reconnect' }
	/**
	 * 再接続しない。ログイン状態を確かめ直す（`(app)/+layout.ts` のルート
	 * ガード = `resolveProtectedSession` の経路）。
	 */
	| { kind: 'recheckSession'; reason: SessionRecheckReason }
	/** 再接続しない。`message` を画面に出す（利用者の操作で再接続できる）。 */
	| { kind: 'halt'; reason: string; message: string };

/** サーバーが送る理由文のうち、ログイン状態を確かめ直すもの。 */
const SESSION_RECHECK_REASONS: ReadonlySet<string> = new Set<SessionRecheckReason>([
	'session_revoked',
	'commissioning_ended'
]);

/** `api_key_*` の理由文ごとの説明（`stream.rs` の `api_key_verdict` の分類と同じ 4 つ）。 */
const API_KEY_MESSAGES: Readonly<Record<string, string>> = {
	api_key_revoked: 'API キーが失効したため、サーバーがリアルタイム更新を止めました。',
	api_key_expired: 'API キーの有効期限が切れたため、サーバーがリアルタイム更新を止めました。',
	api_key_tripped: 'API キーが停止（トリップ）されたため、サーバーがリアルタイム更新を止めました。',
	api_key_not_found: 'API キーが見つからないため、サーバーがリアルタイム更新を止めました。'
};

/** 未知の理由文（空を含む）のときの説明。理由文はそのまま添える。 */
export function unknownRevocationMessage(reason: string): string {
	const shown = reason === '' ? '（理由の記載なし）' : reason;
	return `サーバーが資格情報を受け付けなくなったため、リアルタイム更新を止めました（理由: ${shown}）。`;
}

export function classifyStreamClose(code: number, reason: string): StreamCloseAction {
	if (code !== REVOKED_CLOSE_CODE) return { kind: 'reconnect' };
	if (SESSION_RECHECK_REASONS.has(reason)) {
		return { kind: 'recheckSession', reason: reason as SessionRecheckReason };
	}
	const message = Object.hasOwn(API_KEY_MESSAGES, reason)
		? API_KEY_MESSAGES[reason]
		: unknownRevocationMessage(reason);
	return { kind: 'halt', reason, message };
}

// --- 切れている間の失効（#445） ------------------------------------------

/** 再接続が続けて何回失敗したらログイン状態を確かめるか。 */
export const RECONNECT_FAILURES_BEFORE_SESSION_PROBE = 2;

/**
 * 再接続が失敗したときの、ログイン状態の確認の結果。ルートガードと同じ
 * 3 分類（`sessionGuard.ts` の `decideProtectedRoute`）をそのまま使う。
 */
export type SessionProbeResult = ProtectedRouteDecision;

/** 続けて `consecutiveFailures` 回失敗した今、ログイン状態を確かめるか。 */
export function shouldProbeSession(consecutiveFailures: number): boolean {
	return consecutiveFailures >= RECONNECT_FAILURES_BEFORE_SESSION_PROBE;
}

export type SessionProbeStep =
	/** 再接続を続ける。失敗の回数を `consecutiveFailures` にする。 */
	| { kind: 'reconnect'; consecutiveFailures: number }
	/** 再接続をやめ、`1008` + `session_revoked` と同じ確認の経路へ合流する。 */
	| { kind: 'recheckSession'; reason: 'reconnect_rejected' };

/** ログイン状態の確認の結果から、次の扱いを決める（表はこのファイルの冒頭）。 */
export function decideAfterSessionProbe(
	result: SessionProbeResult,
	consecutiveFailures: number
): SessionProbeStep {
	switch (result) {
		case 'login':
			return { kind: 'recheckSession', reason: 'reconnect_rejected' };
		case 'session':
			return { kind: 'reconnect', consecutiveFailures: 0 };
		case 'unverified':
			return { kind: 'reconnect', consecutiveFailures };
	}
}

/**
 * 同時に何度呼ばれても、実行中の 1 回に相乗りさせる（#441: 複数の
 * ストリームが同時に `1008` を受けても、ログイン状態の確認 = ログイン画面
 * への移動やエラー画面の表示を 1 回にする。#445: 再接続の失敗からの確認も
 * 同じ）。実行が終われば（成功でも失敗でも）次の呼び出しは新しく実行する。
 */
export function createSingleFlight<T>(run: () => Promise<T>): () => Promise<T> {
	let inFlight: Promise<T> | null = null;
	return () => {
		if (inFlight === null) {
			inFlight = (async () => {
				try {
					return await run();
				} finally {
					inFlight = null;
				}
			})();
		}
		return inFlight;
	};
}
