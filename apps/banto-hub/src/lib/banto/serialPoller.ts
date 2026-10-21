/**
 * 直列ポーラー（R2a-2e の PR #760 への Codex レビュー P2）。
 *
 * `setInterval` で取得を回すと、応答が遅いときに要求が積み上がり、古い応答が
 * 新しい状態を上書きしうる。このポーラーは次の約束を守る:
 *
 * - **重ならない**: 次の回は、前の回が決着（成功・失敗・中断）してから
 *   `intervalMs` 後に予約する（`setTimeout` の連鎖）。
 * - **止めたら中断する**: `stop()` は進行中の要求を `AbortSignal` で中断し、
 *   以降の回を予約しない。
 * - **古い結果は捨てる**: `stop()` の後に決着した回の結果・エラーは
 *   `onResult` / `onError` へ渡さない（世代番号で判定。再起動はポーラーを
 *   作り直す = 新しい世代）。
 *
 * - **固まった回は打ち切る**: `run` が決着しない（サーバーが受け付けたまま
 *   応答しない）と次の回が永久に予約されず表示が古いままになる。回ごとに
 *   期限（`timeoutMs`）を持ち、過ぎたらその回の `AbortController` を中断し、
 *   `PollTimeoutError` を `onError` へ渡して通常どおり `onSettled` と次回予約へ
 *   進む。期限後に遅れて決着した結果・エラーは回ごとの完了フラグで捨てる。
 *
 * `run` は `signal` を fetch へ渡す。中断で `run` が reject しても、止めた後の
 * ものとして黙って捨てる（エラー表示にしない）。
 */
/** 1 回の取得が `timeoutMs` 内に決着しなかったときに `onError` へ渡すエラー。 */
export class PollTimeoutError extends Error {
	constructor(message = '受信の状態の取得が時間切れになりました') {
		super(message);
		this.name = 'TimeoutError';
	}
}

/** `timeoutMs` の既定（ミリ秒）。 */
export const DEFAULT_POLL_TIMEOUT_MS = 10_000;

export interface SerialPollerOptions<T> {
	/** 前の回が決着してから次の回までの待ち（ミリ秒）。 */
	intervalMs: number;
	/**
	 * 1 回分の取得の期限（ミリ秒、既定 10_000）。過ぎたらその回を中断して
	 * `PollTimeoutError` を `onError` へ渡す。0 以下・非有限なら期限なし。
	 */
	timeoutMs?: number;
	/** 1 回分の取得。`signal` は `stop()` または期限切れで中断される。 */
	run: (signal: AbortSignal) => Promise<T>;
	/** 成功（まだ止められていないときだけ呼ぶ）。 */
	onResult?: (value: T) => void;
	/** 失敗・期限切れ（まだ止められていないときだけ呼ぶ）。 */
	onError?: (error: unknown) => void;
	/** 成功・失敗・期限切れのどれでも、決着の最後に呼ぶ（まだ止められていないときだけ）。 */
	onSettled?: () => void;
}

export interface SerialPoller {
	/** 進行中の要求と期限タイマーを中断し、以降の回を予約しない。何度呼んでもよい。 */
	stop(): void;
}

/** 作った直後に 1 回目を走らせ、以降は決着のたびに `intervalMs` 後に走らせる。 */
export function createSerialPoller<T>(options: SerialPollerOptions<T>): SerialPoller {
	const timeoutMs = options.timeoutMs ?? DEFAULT_POLL_TIMEOUT_MS;
	let stopped = false;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let deadline: ReturnType<typeof setTimeout> | null = null;
	let controller: AbortController | null = null;

	const clearDeadline = () => {
		if (deadline !== null) {
			clearTimeout(deadline);
			deadline = null;
		}
	};

	const cycle = (): void => {
		timer = null;
		if (stopped) return;
		const current = new AbortController();
		controller = current;
		// この回が決着済み（成功・失敗・期限切れのいずれか）か。遅れて来た
		// 決着は二重に報告せず捨てる。
		let finished = false;
		const finish = (report: () => void): void => {
			if (finished || stopped) return;
			finished = true;
			clearDeadline();
			report();
			if (stopped) return;
			options.onSettled?.();
			if (stopped) return;
			if (controller === current) controller = null;
			timer = setTimeout(cycle, options.intervalMs);
		};

		if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
			deadline = setTimeout(() => {
				deadline = null;
				finish(() => {
					current.abort();
					options.onError?.(new PollTimeoutError());
				});
			}, timeoutMs);
		}

		// `run` の同期 throw も reject と同じに扱う（呼び出し自体は同期のまま）。
		void (async () => options.run(current.signal))().then(
			(value) => finish(() => options.onResult?.(value)),
			(error: unknown) => finish(() => options.onError?.(error))
		);
	};

	cycle();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			if (timer !== null) {
				clearTimeout(timer);
				timer = null;
			}
			clearDeadline();
			controller?.abort();
			controller = null;
		}
	};
}
