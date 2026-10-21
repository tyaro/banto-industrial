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
 * `run` は `signal` を fetch へ渡す。中断で `run` が reject しても、止めた後の
 * ものとして黙って捨てる（エラー表示にしない）。
 */
export interface SerialPollerOptions<T> {
	/** 前の回が決着してから次の回までの待ち（ミリ秒）。 */
	intervalMs: number;
	/** 1 回分の取得。`signal` は `stop()` で中断される。 */
	run: (signal: AbortSignal) => Promise<T>;
	/** 成功（まだ止められていないときだけ呼ぶ）。 */
	onResult?: (value: T) => void;
	/** 失敗（まだ止められていないときだけ呼ぶ）。 */
	onError?: (error: unknown) => void;
	/** 成功・失敗のどちらでも、決着の最後に呼ぶ（まだ止められていないときだけ）。 */
	onSettled?: () => void;
}

export interface SerialPoller {
	/** 進行中の要求を中断し、以降の回を予約しない。何度呼んでもよい。 */
	stop(): void;
}

/** 作った直後に 1 回目を走らせ、以降は決着のたびに `intervalMs` 後に走らせる。 */
export function createSerialPoller<T>(options: SerialPollerOptions<T>): SerialPoller {
	let generation = 0;
	let stopped = false;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let controller: AbortController | null = null;

	const cycle = async (): Promise<void> => {
		timer = null;
		if (stopped) return;
		const mine = generation;
		const current = new AbortController();
		controller = current;
		try {
			const value = await options.run(current.signal);
			if (stopped || mine !== generation) return;
			options.onResult?.(value);
		} catch (error) {
			if (stopped || mine !== generation) return;
			options.onError?.(error);
		}
		if (stopped || mine !== generation) return;
		options.onSettled?.();
		if (controller === current) controller = null;
		timer = setTimeout(() => void cycle(), options.intervalMs);
	};

	void cycle();

	return {
		stop() {
			if (stopped) return;
			stopped = true;
			generation += 1;
			if (timer !== null) {
				clearTimeout(timer);
				timer = null;
			}
			controller?.abort();
			controller = null;
		}
	};
}
