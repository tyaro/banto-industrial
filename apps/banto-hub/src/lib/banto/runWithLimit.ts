/**
 * 画面→サーバーの往復 1 回に**上限を掛ける**汎用ヘルパ（#428）。chronogazer の
 * `src/lib/banto/hubAdmin.ts` の `runWithLimit` と同じもの（banto-hub には
 * `hubAdmin.ts` が無いので単独のファイルにした）。
 *
 * `/audit-log` のブロック取得（`routes/(app)/audit-log/+page.svelte`）が使う:
 * **reject ではなく無応答**の相手だと飛行中のブロックが残り続け、`loading` が
 * 降りず「再読み込み」が押せなくなるため。
 */

/**
 * 上限付きで 1 回走らせた往復の結末。
 *
 * `failed`（相手がエラーを返した／届かなかった）と `timedOut`（上限まで何も
 * 返ってこなかった）を**別の値**にしているのは、呼び出し側の次の一手が違う
 * ため: `failed` は「失敗した」と言い切ってよいが、`timedOut` は
 * **こちらが待つのをやめただけ**で、相手の処理は続いているかもしれない。
 */
export type RunWithLimitOutcome<T> =
	{ kind: 'ok'; value: T } | { kind: 'failed'; error: unknown } | { kind: 'timedOut' };

/**
 * 往復を 1 回だけ走らせ、**上限を過ぎたら打ち切る**。
 *
 * 打ち切りは 2 段構え:
 * 1. `AbortSignal` を往復に渡す（`fetch` は実際にソケットを畳む）。
 * 2. **試行ごとの「打ち切り済み」フラグ**。遅れて解決した応答が `ok` に
 *    化けないようここで止める。
 *
 * **打ち切ってもサーバー側の処理は止まらない**（`abort` で畳めるのはこちらの
 * `fetch` だけ）。
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
