/**
 * T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: タグ削除の
 * 「取り消し」＝削除の遅延実行、を画面へ配線する薄い Svelte 5 runes
 * ラッパー。状態機械の本体（猶予・再入ガード・失敗時の扱い、設計意図の
 * doc comment）は `deferredDeleteCore.ts` にある - このファイルが薄いのは
 * 機能都合ではなく **vitest 制約**による（`deferredDeleteCore.ts` の doc
 * comment「なぜこのファイルは `.svelte.ts` ではないのか」参照）。
 * `pendingIds` だけを `$state` で公開し、画面（`(app)/tags/+page.svelte`）
 * がこれで一覧から行を隠す。
 */
import {
	DeferredDeleteCore,
	UNDO_WINDOW_MS,
	type DeferredDeleteOptions
} from './deferredDeleteCore';

export { UNDO_WINDOW_MS, type DeferredDeleteOptions };

class DeferredDeleteStore {
	#pendingIds = $state<ReadonlySet<number>>(new Set());
	#core = new DeferredDeleteCore({
		onPendingIdsChange: (next) => {
			this.#pendingIds = next;
		}
	});

	/** 画面が一覧から隠すべき id の集合。 */
	get pendingIds(): ReadonlySet<number> {
		return this.#pendingIds;
	}

	/** 猶予タイマーが生きているエントリを保持しているか（実行中は含まない）。 */
	get hasPending(): boolean {
		return this.#core.hasPending;
	}

	/** 削除を `UNDO_WINDOW_MS` だけ遅延させる（詳細は `DeferredDeleteCore.schedule` 参照）。 */
	schedule(options: DeferredDeleteOptions): void {
		this.#core.schedule(options);
	}

	/**
	 * 猶予中のエントリを取り消す。`run` は一度も呼ばれない。戻り値は実際に
	 * 取り消せたかどうか（既に実行済み・実行中、または保留が無ければ
	 * `false` - 詳細は `DeferredDeleteCore.undo` 参照）。
	 */
	undo(): boolean {
		return this.#core.undo();
	}

	/** 猶予中のエントリがあれば即座に実行し、完了まで待つ（詳細は `DeferredDeleteCore.flush` 参照）。 */
	flush(): Promise<void> {
		return this.#core.flush();
	}
}

export const deferredDelete = new DeferredDeleteStore();
