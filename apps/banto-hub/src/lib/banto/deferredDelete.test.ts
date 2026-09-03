/**
 * T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: 削除の取り消しを
 * 実現する状態機械 `DeferredDeleteCore`（`deferredDeleteCore.ts`）のユニット
 * テスト。
 *
 * **なぜ `deferredDelete.svelte.ts` を直接テストしないのか**
 * （`deferredDeleteCore.ts` の doc comment「なぜこのファイルは `.svelte.ts`
 * ではないのか」参照）: このリポジトリの vitest は `@sveltejs/vite-plugin-
 * svelte` を導入しない最小構成（`vitest.config.ts` の doc comment、
 * `tagRegistryAdmin.test.ts` の doc comment参照）で、`$state` を含む
 * `.svelte.ts` を直接 import すると `ReferenceError: $state is not defined`
 * になる。状態機械のロジックはすべて rune 非依存の `deferredDeleteCore.ts`
 * に切り出してあるので、ここではそちらを直接テストする -
 * `deferredDelete.svelte.ts` はこのロジックを `$state` でラップして
 * `pendingIds` を公開するだけの薄いラッパーであり、固有のロジックを
 * 持たない。
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { DeferredDeleteCore, UNDO_WINDOW_MS } from './deferredDeleteCore';

/** `onPendingIdsChange` は `deferredDelete.svelte.ts` 側の `$state` 反映用フックなので、ここでは no-op でよい。 */
function makeCore(): { core: DeferredDeleteCore } {
	const core = new DeferredDeleteCore({ onPendingIdsChange: () => {} });
	return { core };
}

beforeEach(() => {
	vi.useFakeTimers();
});

afterEach(() => {
	vi.useRealTimers();
});

describe('DeferredDeleteCore.schedule / 猶予', () => {
	it('猶予中は run が呼ばれない', () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		expect(run).not.toHaveBeenCalled();
		expect(core.pendingIds.has(1)).toBe(true);
		expect(core.hasPending).toBe(true);

		vi.advanceTimersByTime(UNDO_WINDOW_MS - 1);
		expect(run).not.toHaveBeenCalled();
	});

	it('猶予が切れたら run が1回だけ呼ばれる', async () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		// `advanceTimersByTimeAsync` はタイマー発火後に生じる Promise
		// チェーン（`await entry.run()` 等）もマイクロタスクとして消化して
		// くれる - フェイクタイマーは `setTimeout` 系だけを模すので、
		// マイクロタスク待ちには `vi.waitFor`（内部で実タイマーに依存し
		// フェイクタイマーと相性が悪い）ではなくこちらを使う。
		await vi.advanceTimersByTimeAsync(UNDO_WINDOW_MS);
		expect(run).toHaveBeenCalledTimes(1);

		// タイマーが再度発火しても増えないことも確認する。
		await vi.advanceTimersByTimeAsync(UNDO_WINDOW_MS * 3);
		expect(run).toHaveBeenCalledTimes(1);
	});

	it('猶予が切れると pendingIds からも消え、onExecuted が呼ばれる', async () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		const onExecuted = vi.fn();
		core.schedule({ ids: [1, 2], run, onExecuted });

		await vi.advanceTimersByTimeAsync(UNDO_WINDOW_MS);

		expect(onExecuted).toHaveBeenCalledTimes(1);
		expect(core.pendingIds.has(1)).toBe(false);
		expect(core.pendingIds.has(2)).toBe(false);
	});
});

describe('DeferredDeleteCore.undo', () => {
	it('undo() すると run が呼ばれない', () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		core.undo();
		expect(core.pendingIds.has(1)).toBe(false);
		expect(core.hasPending).toBe(false);

		vi.advanceTimersByTime(UNDO_WINDOW_MS * 2);
		expect(run).not.toHaveBeenCalled();
	});

	// レビュー対応（2026-09-03、UX-40）: undo() の戻り値が「実際に取り消せた
	// か」を正しく表すこと。呼び出し元（(app)/tags/+page.svelte の
	// scheduleTagDeletion）はこれを見て、既に実行済みの削除を「取り消し
	// ました」と偽らないようにする。
	it('猶予中の undo() は true を返す', () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		expect(core.undo()).toBe(true);
	});

	it('保留が無い状態の undo() は false を返す', () => {
		const { core } = makeCore();
		expect(core.undo()).toBe(false);
	});

	it('猶予が切れて実行された後の undo() は false を返し、run は増えない', async () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		await vi.advanceTimersByTimeAsync(UNDO_WINDOW_MS);
		expect(run).toHaveBeenCalledTimes(1);

		expect(core.undo()).toBe(false);
		expect(run).toHaveBeenCalledTimes(1);
	});

	it('flush() で前倒し実行された後の undo() は false を返す', async () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		await core.flush();
		expect(run).toHaveBeenCalledTimes(1);

		expect(core.undo()).toBe(false);
		expect(run).toHaveBeenCalledTimes(1);
	});
});

describe('DeferredDeleteCore.flush', () => {
	it('flush() で即時実行される（猶予タイマーを待たない）', async () => {
		const { core } = makeCore();
		const run = vi.fn(async () => {});
		core.schedule({ ids: [1], run });

		await core.flush();

		expect(run).toHaveBeenCalledTimes(1);
		expect(core.pendingIds.has(1)).toBe(false);
		expect(core.hasPending).toBe(false);
	});

	it('保留が無ければ flush() は即 resolve する', async () => {
		const { core } = makeCore();
		await expect(core.flush()).resolves.toBeUndefined();
	});

	it('flush() の再入で二重実行されない（run 自身が flush() を呼んでも run は1回だけ）', async () => {
		const { core } = makeCore();
		let reentered = false;
		const run = vi.fn(async () => {
			// httpRequest フックが deleteTag/deleteTagsBatch の内側から
			// flush() を再度呼ぶ状況を模す。
			if (!reentered) {
				reentered = true;
				await core.flush();
			}
		});
		core.schedule({ ids: [1], run });

		await core.flush();

		expect(run).toHaveBeenCalledTimes(1);
	});

	it('2つの flush() 呼び出しが重なっても run は1回だけ（外部からの二重呼び出し）', async () => {
		const { core } = makeCore();
		let resolveRun: () => void = () => {};
		const run = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					resolveRun = resolve;
				})
		);
		core.schedule({ ids: [1], run });

		const p1 = core.flush();
		const p2 = core.flush();
		resolveRun();
		await Promise.all([p1, p2]);

		expect(run).toHaveBeenCalledTimes(1);
	});
});

describe('DeferredDeleteCore: 猶予中に別の schedule が来た場合', () => {
	it('先のものが即座に実行されてから、新しいものが積まれる', async () => {
		const { core } = makeCore();
		const runA = vi.fn(async () => {});
		const runB = vi.fn(async () => {});

		core.schedule({ ids: [1], run: runA });
		core.schedule({ ids: [2], run: runB });

		// runA は新しい schedule() の呼び出しの中で同期的に起動される。
		expect(runA).toHaveBeenCalledTimes(1);
		expect(runB).not.toHaveBeenCalled();
		// 両方とも一覧からは隠れている（A は実行中、B は猶予中）。
		expect(core.pendingIds.has(2)).toBe(true);

		await vi.advanceTimersByTimeAsync(UNDO_WINDOW_MS);
		expect(runB).toHaveBeenCalledTimes(1);
	});
});

describe('DeferredDeleteCore: run が失敗した場合', () => {
	it('onError が呼ばれ、pendingIds がクリアされる', async () => {
		const { core } = makeCore();
		const error = new Error('サーバーエラー');
		const run = vi.fn(async () => {
			throw error;
		});
		const onError = vi.fn();
		const onExecuted = vi.fn();
		core.schedule({ ids: [1], run, onExecuted, onError });

		await core.flush();

		expect(onError).toHaveBeenCalledWith(error);
		expect(onExecuted).not.toHaveBeenCalled();
		expect(core.pendingIds.has(1)).toBe(false);
	});
});
