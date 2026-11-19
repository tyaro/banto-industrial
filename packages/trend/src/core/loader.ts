/**
 * データの取り方の型と、表示範囲の変化に合わせて読み直す仕組み。
 *
 * 部品はデータの取り方を持たない（設計書 §4.1・§4.3）。利用側が `TrendFetch` を渡し、
 * ChronoGazer は `GET /api/collect/history`（Tauri は同じ口）、SCADA は自分の
 * History API をつなぐ。
 *
 * `createRangeLoader` の約束:
 * - 要求が続けて来たら、最後のものだけを `debounceMs` 後に読む。
 * - 新しい要求が来た・`dispose` したら、読み出し中のものを abort する。
 * - 古い結果は `onResult` に渡さない（abort に従わない fetch が遅れて返っても捨てる）。
 * - AbortError は `onError` に渡さない。古い要求の失敗も渡さない。
 * - キャッシュは持たない（後で足す）。
 */

import type { Bin } from './pens';
import type { TimeRange } from './range';

export interface TrendFetchRequest {
	range: TimeRange;
	penIds: readonly string[];
	bins: number;
	signal: AbortSignal;
}

/** ペンの id → 区間の列（`tMs` の昇順）。 */
export type TrendFetch = (req: TrendFetchRequest) => Promise<Record<string, Bin[]>>;

/** `request` に渡す要求（signal はローダーが付ける）。 */
export type TrendLoadRequest = Omit<TrendFetchRequest, 'signal'>;

export interface RangeLoaderOptions {
	fetch: TrendFetch;
	debounceMs?: number;
	onResult: (result: Record<string, Bin[]>, req: TrendLoadRequest) => void;
	onError?: (error: unknown, req: TrendLoadRequest) => void;
}

export interface RangeLoader {
	request(req: TrendLoadRequest): void;
	dispose(): void;
}

/** AbortController の abort で起きた失敗か。 */
export function isAbortError(e: unknown): boolean {
	return (
		typeof e === 'object' &&
		e !== null &&
		'name' in e &&
		(e as { name: unknown }).name === 'AbortError'
	);
}

export function createRangeLoader(options: RangeLoaderOptions): RangeLoader {
	const { fetch, onResult, onError } = options;
	const debounceMs =
		Number.isFinite(options.debounceMs) && (options.debounceMs as number) >= 0
			? (options.debounceMs as number)
			: 200;
	let seq = 0;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let inflight: AbortController | null = null;
	let disposed = false;

	const cancel = () => {
		if (timer !== null) {
			clearTimeout(timer);
			timer = null;
		}
		inflight?.abort();
		inflight = null;
	};

	const run = (req: TrendLoadRequest, my: number) => {
		timer = null;
		const controller = new AbortController();
		inflight = controller;
		const current = () => my === seq && !disposed && !controller.signal.aborted;
		let p: Promise<Record<string, Bin[]>>;
		try {
			p = fetch({ ...req, signal: controller.signal });
		} catch (e) {
			p = Promise.reject(e);
		}
		p.then(
			(result) => {
				if (current()) onResult(result, req);
			},
			(e) => {
				if (current() && !isAbortError(e)) onError?.(e, req);
			}
		).finally(() => {
			if (inflight === controller) inflight = null;
		});
	};

	return {
		request(req) {
			if (disposed) return;
			seq++;
			const my = seq;
			cancel();
			timer = setTimeout(() => run(req, my), debounceMs);
		},
		dispose() {
			disposed = true;
			seq++;
			cancel();
		}
	};
}
