/**
 * T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: タグ削除の
 * 「取り消し」を、削除の**遅延実行**として実現する状態機械の本体。
 *
 * なぜ「削除してから作り直す」ではないのか: 履歴のキー（`tag:<id>` 等、
 * `crates/banto-collect/src/config.rs` の `tag_key`）はレジストリの主キー
 * （SQLite の `AUTOINCREMENT`）由来で、id は再利用されない。削除して
 * 同じ内容で作り直すと必ず新しい id になり、そのタグは自分の過去履歴に
 * 二度と到達できなくなる。そのため「取り消し」は本物の削除を数秒間
 * 遅らせるだけにし、猶予中に取り消されれば `run` を一度も呼ばない
 * （＝サーバーには何も送らない＝id は変わらない）。
 *
 * なぜ同時に1件しか保持しないのか: 猶予中の対象は画面がまとめて
 * `pendingIds` で隠している。複数件を並行して保持できるようにすると
 * 「どの猶予がどのトーストに対応するか」「取り消しはどれを戻すのか」が
 * 曖昧になる。ここでは「猶予中に次の削除が来たら、前のものは（もう
 * 迷う余地が無いので）即座に確定させてから、新しいものを積む」という
 * 単純な規則にして、常に「保留は0件か1件」を保つ。これにより送信順序も
 * 保たれる（前を確定させてから次を積むので、サーバーへ届く順序が
 * ユーザー操作の順序と入れ替わらない）。
 *
 * なぜこのファイルは `.svelte.ts` ではないのか: `deferredDelete.svelte.ts`
 * （こちらは薄いラッパー、`pendingIds` を `$state` で公開するだけ）が
 * この状態機械を包む。ロジック本体をこの `.svelte.ts` の外に出したのは
 * 機能都合ではなく **テスト基盤の制約**による - `vitest.config.ts` の
 * doc comment（H5「純関数のユニットテストのみを対象とし、Svelte
 * コンポーネントは対象外」）のとおり、このリポジトリの vitest は
 * `@sveltejs/vite-plugin-svelte` を導入しない最小構成で、`$state` を含む
 * `.svelte.ts` を直接 import すると `ReferenceError: $state is not
 * defined` になる（`tagRegistryAdmin.test.ts` 等の doc comment 参照）。
 * 状態機械のロジック（猶予・再入ガード・失敗時の扱い）はここに置いて
 * 素の TypeScript として単体テストできるようにし、Svelte 側の反応性は
 * `deferredDelete.svelte.ts` 側の薄いラッパーに限定する。
 */

/**
 * 取り消し猶予の長さ。トースト（`toast.svelte.ts`）の表示時間はこれ以下に
 * すること（レビュー対応・2026-09-03: かつては「これより長く」だったが、
 * それだと猶予切れ後も取り消しボタンが最大でその差分だけ画面に残り、
 * 押しても実行済みの削除は戻せないのに `undo()` が黙って無視する不具合
 * だった。今は `undo()` が「実際に取り消せたか」を真偽値で返し、実行済み
 * なら成功トーストを出さないため、猶予より長く表示する理由は無い -
 * `(app)/tags/+page.svelte` の `scheduleTagDeletion` 参照）。
 */
export const UNDO_WINDOW_MS = 6000;

export interface DeferredDeleteOptions {
	/** 削除対象の id 一覧（`pendingIds` に積まれ、画面はこれで行を隠す）。 */
	ids: number[];
	/** 猶予が切れた（または `flush()` された）ときに実際に呼ぶ削除処理。 */
	run: () => Promise<void>;
	/** `run` が成功したときに呼ぶ後処理（成功トースト・`reload()` 等）。 */
	onExecuted?: () => void | Promise<void>;
	/** `run` が失敗したときに呼ぶ後処理（エラートースト等）。 */
	onError?: (err: unknown) => void;
}

interface PendingEntry {
	ids: number[];
	run: () => Promise<void>;
	onExecuted?: () => void | Promise<void>;
	onError?: (err: unknown) => void;
}

export interface DeferredDeleteCoreCallbacks {
	/** `pendingIds` が変わるたびに呼ばれる（Svelte 側の `$state` 反映用）。 */
	onPendingIdsChange: (pendingIds: ReadonlySet<number>) => void;
}

export class DeferredDeleteCore {
	#pendingIds: Set<number> = new Set();
	/** 猶予タイマーが生きている、まだ実行を始めていないエントリ。同時に高々1件。 */
	#current: PendingEntry | null = null;
	#timer: ReturnType<typeof setTimeout> | null = null;
	/**
	 * いずれかのエントリの `run()` が現在実行中かどうか（トリガーが
	 * タイマー満了・`flush()`・`schedule()` の前倒し確定のいずれでも true）。
	 * `flush()` の再入ガードに使う - 下のコメント参照。
	 */
	#executing = false;
	/** 「外部から」呼ばれた `flush()` が進行中のときの Promise（二重起動防止）。 */
	#flushPromise: Promise<void> | null = null;

	constructor(private readonly callbacks: DeferredDeleteCoreCallbacks) {}

	/** 画面が一覧から隠すべき id の集合。 */
	get pendingIds(): ReadonlySet<number> {
		return this.#pendingIds;
	}

	/** 猶予タイマーが生きているエントリを保持しているか（実行中は含まない）。 */
	get hasPending(): boolean {
		return this.#current !== null;
	}

	/**
	 * 削除を `UNDO_WINDOW_MS` だけ遅延させる。猶予中に別の `schedule` が
	 * 来た場合は、前のエントリを（タイマーを待たず）即座に確定実行してから
	 * 新しいエントリを積む - 送信順序がユーザー操作の順序と入れ替わらない
	 * ようにするため。
	 */
	schedule(options: DeferredDeleteOptions): void {
		const entry: PendingEntry = {
			ids: [...options.ids],
			run: options.run,
			onExecuted: options.onExecuted,
			onError: options.onError
		};

		if (this.#current) {
			if (this.#timer !== null) {
				clearTimeout(this.#timer);
				this.#timer = null;
			}
			const prev = this.#current;
			this.#current = null;
			void this.#run(prev);
		}

		this.#current = entry;
		this.#setPendingIds(new Set([...this.#pendingIds, ...entry.ids]));
		this.#timer = setTimeout(() => {
			this.#timer = null;
			const due = this.#current;
			this.#current = null;
			if (due) void this.#run(due);
		}, UNDO_WINDOW_MS);
	}

	/**
	 * 猶予中のエントリを取り消す。`run` は一度も呼ばれない。
	 *
	 * 戻り値は「実際に取り消せたかどうか」。猶予中のエントリがあってそれを
	 * 取り消した場合のみ `true`。猶予タイマーが切れて既に `run` が実行済み
	 * （または実行中）、あるいは最初から保留が無い場合は `false` を返す -
	 * 呼び出し元（画面）はこれを見て「本当に取り消せた」ときだけ取り消し
	 * 成功のトーストを出すべきで、`false` のときに成功を名乗ると、実際には
	 * 削除済みなのに「取り消しました」と嘘をつくことになる。
	 */
	undo(): boolean {
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		if (this.#current) {
			this.#removeIds(this.#current.ids);
			this.#current = null;
			return true;
		}
		return false;
	}

	/**
	 * 猶予中のエントリがあれば即座に実行し、完了まで待つ。保留が無ければ
	 * 即 resolve する。
	 *
	 * 再入ガード: `run()`（`deleteTag`/`deleteTagsBatch`）は
	 * `tagRegistryAdmin.ts` の `httpRequest` を呼び、`httpRequest` は
	 * 変更系リクエストの直前でこの `flush()` を呼ぶ。つまり `flush()` が
	 * 起動したまさにその実行の**内側から** `flush()` が再度呼ばれる
	 * （＝再入）。この再入呼び出しが `#flushPromise`（外側の `flush()` 自身が
	 * 完了を待っている Promise）を待ってしまうと、外側の完了は内側の
	 * `run()` の完了待ちであり、内側の `run()` の完了はこの再入呼び出しの
	 * 完了待ちになる - 循環して永久に解決しない自己参照デッドロックになる
	 * （実装中に気づいた問題 - 報告書に明記）。そのため `#executing`
	 * （「いま何かの `run()` が実行中」）を見て、実行中の再入呼び出しは
	 * 何もせず即座に戻す（この時点で `#current` は既に空なので、積み残しは
	 * 無い）。これは「進行中の Promise をそのまま返す」という素朴な実装
	 * （それ自体は上記の理由でデッドロックする）とは異なるが、「二重実行
	 * させない」という意図は保っている。
	 */
	async flush(): Promise<void> {
		if (this.#executing) return;
		if (this.#flushPromise) return this.#flushPromise;

		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		const entry = this.#current;
		this.#current = null;
		if (!entry) return;

		const promise = this.#run(entry);
		this.#flushPromise = promise;
		try {
			await promise;
		} finally {
			this.#flushPromise = null;
		}
	}

	async #run(entry: PendingEntry): Promise<void> {
		this.#executing = true;
		try {
			await entry.run();
			this.#removeIds(entry.ids);
			await entry.onExecuted?.();
		} catch (err) {
			this.#removeIds(entry.ids);
			entry.onError?.(err);
		} finally {
			this.#executing = false;
		}
	}

	#removeIds(ids: number[]): void {
		if (ids.length === 0) return;
		const remove = new Set(ids);
		this.#setPendingIds(new Set([...this.#pendingIds].filter((id) => !remove.has(id))));
	}

	#setPendingIds(next: Set<number>): void {
		this.#pendingIds = next;
		this.callbacks.onPendingIdsChange(next);
	}
}
