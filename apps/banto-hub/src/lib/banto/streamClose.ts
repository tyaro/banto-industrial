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
 */

/** 資格情報が使えないと確認できたときの close コード（`stream.rs` の `REVOKED_CLOSE_CODE`）。 */
export const REVOKED_CLOSE_CODE = 1008;

/** ログイン状態を確かめ直す理由文。 */
export type SessionRecheckReason = 'session_revoked' | 'commissioning_ended';

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

/**
 * 同時に何度呼ばれても、実行中の 1 回に相乗りさせる（#441: 複数の
 * ストリームが同時に `1008` を受けても、ログイン状態の確認 = ログイン画面
 * への移動やエラー画面の表示を 1 回にする）。実行が終われば（成功でも失敗
 * でも）次の呼び出しは新しく実行する。
 */
export function createSingleFlight(run: () => Promise<void>): () => Promise<void> {
	let inFlight: Promise<void> | null = null;
	return () => {
		if (inFlight === null) {
			inFlight = (async () => {
				try {
					await run();
				} finally {
					inFlight = null;
				}
			})();
		}
		return inFlight;
	};
}
