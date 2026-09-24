/**
 * **ブロック単位の遅延取得キャッシュ**（汎用部）。#409 で `/events` 用に
 * 作った `routes/(app)/events/eventBlocks.ts` の判断を、行の型と失敗の型から
 * 切り離してここへ出したもの（#410: `/audit-log` が同じ欠陥を持っていた
 * ので、同じ直し方を二度書かない）。
 *
 * ここが決めるのは:
 *
 * 1. **どのブロックを取るか**（[`blocksToFetch`]。表示範囲・総件数の有無・
 *    失敗したブロックを 1 つの述語にまとめる）、
 * 2. **何を渡して取るか**（[`blockRequest`]。世代のスナップショット境界
 *    `asOfId`）、
 * 3. **応答をどこに入れるか / 採らないか**（[`applyOutcome`]。世代違いの応答と、
 *    画面ごとの方針（[`BlockPolicy`]）が「採れない」と判断した応答は入れない）、
 * 4. **失敗をどう持つか**（**ブロック単位**。別ブロックの成功で消さない）。
 *
 * **画面ごとに違うもの**はアダプタ側に残す（`/events` = `Readout` の 3 状態と
 * 収集イベントの口、`/audit-log` = 並べ替え・絞り込みと、保持期間の削除による
 * スナップショット失効）。汎用部はそれを [`BlockPolicy`] と失敗の型 `F`
 * だけで受け取る。
 *
 * ### 世代とスナップショット境界
 *
 * サーバーは各リクエスト時点のデータを `ORDER BY … LIMIT … OFFSET …` で返す。
 * ブロック取得の**合間に行が追加される**と `OFFSET` がずれ、境界で行が重複し、
 * 末尾の行が一覧から漏れる。そこで**同じ世代のブロックは同じデータ集合から
 * 取る**: 世代の最初の応答が返した境界（`id` の最大値）を固定し、後続ブロックに
 * すべて渡す。これは `id` が `AUTOINCREMENT`（単調増加・再利用なし）である
 * ことに依拠する - 使う側の表がそうであることを確かめてから使うこと。
 *
 * 境界は**追加**にしか効かない。**削除**（保持期間など）で `OFFSET` がずれる
 * かどうかは表によって違うので、[`BlockPolicy.reject`] で応答ごとに判断させる。
 *
 * ### 自動では再試行しない
 *
 * 今の世代で失敗したブロックは、スクロールでは取り直さない。取り直すのは
 * 新しい世代（「再読み込み」）だけで、失敗の表示はそのブロックの取得が成功
 * するまで残る。
 */

/** 1 ブロックの件数（`/audit-log` の元の `AuditLogWindow` と同じ 200）。 */
export const BLOCK_SIZE = 200;

/** 境界つきの 1 ページ（`ListResult` と同じ綴り + 使った境界 `asOfId`）。 */
export interface SnapshotList<Row> {
	rows: Row[];
	totalCount: number;
	asOfId: number;
}

/** 世代の境界と、その境界で数えた総件数。 */
export interface Snapshot {
	asOfId: number;
	totalCount: number;
}

/** 読めた結末。失敗の型 `F` はアダプタが決める（`kind` は `'ready'` 以外）。 */
export interface ReadyOutcome<Row> {
	kind: 'ready';
	list: SnapshotList<Row>;
}

/** 1 ブロックの取得の結末。 */
export type BlockOutcome<Row, F> = ReadyOutcome<Row> | F;

/** 1 ブロック分の要求。 */
export interface BlockRequest {
	block: number;
	offset: number;
	limit: number;
	/** 世代のスナップショット境界。`null` = この世代の最初の取得（境界を決めさせる）。 */
	asOfId: number | null;
}

/** ブロックキャッシュの状態（**不変**。純関数が新しい値を返す）。 */
export interface BlockCache<F> {
	/** 「再読み込み」で進む。世代違いの応答は採らない。 */
	generation: number;
	/** この世代の境界と総件数。`null` = この世代はまだ 1 度も読めていない。 */
	snapshot: Snapshot | null;
	loaded: ReadonlySet<number>;
	inFlight: ReadonlySet<number>;
	/** ブロックごとの失敗と、それを記録した世代。 */
	failed: ReadonlyMap<number, { failure: F; generation: number }>;
	/** 一度でも読めたか（「まだ読み込んでいない」と「0 件」の言い分け）。 */
	everRead: boolean;
	/** 画面に出している総件数（世代をまたいで保つ - 読めなかったときに 0 に潰さない）。 */
	totalCount: number;
}

/** 画面ごとの方針。 */
export interface BlockPolicy<Row, F> {
	/**
	 * 世代の境界が決まった後の `ready` 応答を**採れない**なら、その理由を
	 * 失敗として返す（`null` = 採る）。採らなかった応答は、そのブロックの
	 * 失敗として残る（黙って取り直すと無限に取り直すことがある）。
	 */
	reject(snapshot: Snapshot, list: SnapshotList<Row>): F | null;
	/**
	 * この失敗が今の世代で 1 つでも起きたら、**この世代ではもう取らない**
	 * （取っても採れないことが分かっている）。省略時は止めない。
	 */
	haltsGeneration?(failure: F): boolean;
}

/** [`applyOutcome`] の結果。行の書き込みは画面（`$state`）に任せる。 */
export interface ApplyResult<Row, F> {
	cache: BlockCache<F>;
	/** 非 `null` なら、行の配列をこの長さで**作り直す**（世代の最初の応答）。 */
	resetRows: number | null;
	/** 非 `null` なら、この位置から行を書き込む。 */
	write: { offset: number; rows: Row[] } | null;
}

function isReady<Row, F>(outcome: BlockOutcome<Row, F>): outcome is ReadyOutcome<Row> {
	return (outcome as { kind: string }).kind === 'ready';
}

export function initialCache<F>(): BlockCache<F> {
	return {
		generation: 0,
		snapshot: null,
		loaded: new Set(),
		inFlight: new Set(),
		failed: new Map(),
		everRead: false,
		totalCount: 0
	};
}

/** 表示範囲 `[start, end)` が跨ぐブロック番号。空範囲は空配列。 */
export function blocksFor(start: number, end: number): number[] {
	if (end <= start) return [];
	const firstBlock = Math.floor(Math.max(start, 0) / BLOCK_SIZE);
	const lastBlock = Math.floor((end - 1) / BLOCK_SIZE);
	const blocks: number[] = [];
	for (let b = firstBlock; b <= lastBlock; b++) blocks.push(b);
	return blocks;
}

/** 今の世代に、世代を止める失敗があるか。 */
export function isGenerationHalted<Row, F>(
	cache: BlockCache<F>,
	policy?: Pick<BlockPolicy<Row, F>, 'haltsGeneration'>
): boolean {
	const halts = policy?.haltsGeneration;
	if (!halts) return false;
	for (const record of cache.failed.values()) {
		if (record.generation === cache.generation && halts(record.failure)) return true;
	}
	return false;
}

/**
 * **何を取るかを決める唯一の述語**（スクロールでも「再読み込み」でも同じ）。
 *
 * 取りたいのは:
 * - 表示範囲のブロック、
 * - **総件数をまだ取れていない世代では先頭ブロック**（表示範囲が空でも）。
 *   総件数 0 のあいだ `BantoGrid` が通知する表示範囲は `{0, 0}` なので、
 *   これを取りこぼすと初回失敗の後・0 件の後に**要求が 1 本も出ない**、
 * - **前の世代で失敗したブロック**（「再読み込み」が取り直す対象）。
 *
 * ただし、すでに取れている・飛行中・**今の世代で失敗した**ブロックは除く。
 * 世代を止める失敗（[`BlockPolicy.haltsGeneration`]）があれば何も取らない。
 *
 * **この世代でまだ境界が決まっていない間は 1 ブロックしか投げない**: 並列に
 * 投げると、それぞれが別の境界（別のデータ集合）を返してしまう。
 */
export function blocksToFetch<Row, F>(
	cache: BlockCache<F>,
	start: number,
	end: number,
	policy?: Pick<BlockPolicy<Row, F>, 'haltsGeneration'>
): number[] {
	if (isGenerationHalted(cache, policy)) return [];
	const wanted = new Set<number>(blocksFor(start, end));
	if (cache.snapshot === null) wanted.add(0);
	for (const [block, record] of cache.failed) {
		if (record.generation !== cache.generation) wanted.add(block);
	}
	const candidates = [...wanted]
		.sort((a, b) => a - b)
		.filter(
			(block) =>
				!cache.loaded.has(block) &&
				!cache.inFlight.has(block) &&
				cache.failed.get(block)?.generation !== cache.generation
		);
	if (cache.snapshot !== null) return candidates;
	return cache.inFlight.size > 0 ? [] : candidates.slice(0, 1);
}

/** 1 ブロックの要求（この世代の境界を必ず載せる）。 */
export function blockRequest<F>(cache: BlockCache<F>, block: number): BlockRequest {
	return {
		block,
		offset: block * BLOCK_SIZE,
		limit: BLOCK_SIZE,
		asOfId: cache.snapshot?.asOfId ?? null
	};
}

export function markInFlight<F>(cache: BlockCache<F>, blocks: readonly number[]): BlockCache<F> {
	const inFlight = new Set(cache.inFlight);
	for (const block of blocks) inFlight.add(block);
	return { ...cache, inFlight };
}

/**
 * 「再読み込み」= **新しい世代**。境界を外し（新しい行はここで入る）、
 * 取得済み・飛行中を捨てる。
 *
 * **失敗は捨てない**: 失敗の表示は、そのブロックの取得が成功するまで残す。
 * 世代が変わったことで [`blocksToFetch`] の対象に戻る（= 取り直す）。
 *
 * **飛行中を捨てるので `loading` はここで降りる**（「唯一の回復導線が、
 * 回復したいときだけ押せなくなる」の再発防止）。
 */
export function newGeneration<F>(cache: BlockCache<F>): BlockCache<F> {
	return {
		...cache,
		generation: cache.generation + 1,
		snapshot: null,
		loaded: new Set(),
		inFlight: new Set()
	};
}

/**
 * **問い合わせそのものが変わった**（並べ替え・絞り込みの変更）= 新しい世代で、
 * かつ**前の問い合わせの結果を何も持ち越さない**: ブロック番号は別の集合を
 * 指すので、失敗も総件数も「読めたか」も捨てる（前の絞り込みの件数を新しい
 * 絞り込みの件数として見せない）。
 */
export function newQuery<F>(cache: BlockCache<F>): BlockCache<F> {
	return {
		...newGeneration(cache),
		failed: new Map(),
		everRead: false,
		totalCount: 0
	};
}

/**
 * 1 ブロックの結末を取り込む。
 *
 * - **世代違いの応答は採らない**（飛行中の古い応答が新しい状態を巻き戻さない）、
 * - 失敗は**そのブロックに**記録する（別ブロックの成功で消えない）、
 * - 境界が決まった後の応答は `policy.reject` に通し、採れなければ失敗として残す、
 * - 世代の最初の `ready` で境界と総件数を固定し、**行の配列を作り直す**。
 */
export function applyOutcome<Row, F>(
	cache: BlockCache<F>,
	block: number,
	generation: number,
	outcome: BlockOutcome<Row, F>,
	policy: BlockPolicy<Row, F>
): ApplyResult<Row, F> {
	if (generation !== cache.generation) return { cache, resetRows: null, write: null };

	const inFlight = new Set(cache.inFlight);
	inFlight.delete(block);
	const failed = new Map(cache.failed);

	if (!isReady(outcome)) {
		failed.set(block, { failure: outcome, generation });
		return { cache: { ...cache, inFlight, failed }, resetRows: null, write: null };
	}

	if (cache.snapshot !== null) {
		const rejected = policy.reject(cache.snapshot, outcome.list);
		if (rejected !== null) {
			failed.set(block, { failure: rejected, generation });
			return { cache: { ...cache, inFlight, failed }, resetRows: null, write: null };
		}
	}

	failed.delete(block);
	const loaded = new Set(cache.loaded);
	loaded.add(block);
	const firstOfGeneration = cache.snapshot === null;
	const snapshot = cache.snapshot ?? {
		asOfId: outcome.list.asOfId,
		totalCount: outcome.list.totalCount
	};
	return {
		cache: {
			...cache,
			inFlight,
			failed,
			loaded,
			snapshot,
			everRead: true,
			totalCount: snapshot.totalCount
		},
		resetRows: firstOfGeneration ? snapshot.totalCount : null,
		write: { offset: block * BLOCK_SIZE, rows: outcome.list.rows }
	};
}

/** 取得を 1 本走らせる関数。**結末を返し、reject しない**約束（しても拾う）。 */
export type BlockFetcher<Row, F> = (request: BlockRequest) => Promise<BlockOutcome<Row, F>>;

/** 画面（`$state`）への書き戻し口。 */
export interface BlockSink<Row, F> {
	/** 行の配列をこの長さで作り直す。 */
	resetRows(length: number): void;
	writeRows(offset: number, rows: Row[]): void;
	update(cache: BlockCache<F>): void;
}

/**
 * 上の純関数を繋ぐだけの駆動役（Svelte に依存しないので vitest から素で
 * 回せる）。画面はこれに表示範囲と「再読み込み」を伝え、`sink` で受け取る。
 */
export class BlockLoader<Row, F> {
	#cache: BlockCache<F> = initialCache<F>();
	#start = 0;
	#end = 0;
	readonly #fetcher: BlockFetcher<Row, F>;
	readonly #sink: BlockSink<Row, F>;
	readonly #policy: BlockPolicy<Row, F>;
	/** reject したときの失敗の作り方（`F` はアダプタしか作れない）。 */
	readonly #onThrow: (err: unknown) => F;

	constructor(
		fetcher: BlockFetcher<Row, F>,
		sink: BlockSink<Row, F>,
		policy: BlockPolicy<Row, F>,
		onThrow: (err: unknown) => F
	) {
		this.#fetcher = fetcher;
		this.#sink = sink;
		this.#policy = policy;
		this.#onThrow = onThrow;
	}

	/** テストと画面の点検用（状態の読み取りだけ）。 */
	get cache(): BlockCache<F> {
		return this.#cache;
	}

	/** 表示範囲が変わった（初回も含む）。 */
	setRange(start: number, end: number): void {
		this.#start = start;
		this.#end = end;
		this.#pump();
	}

	/** 手で押す「再読み込み」= 新しい世代（新しい行もここで入る）。 */
	reload(): void {
		this.#cache = newGeneration(this.#cache);
		this.#sink.update(this.#cache);
		this.#pump();
	}

	/**
	 * 問い合わせが変わった（並べ替え・絞り込み）。前の問い合わせの行は
	 * **すぐに消す**（新しい問い合わせの行として見せない）。
	 */
	restart(): void {
		this.#cache = newQuery(this.#cache);
		this.#sink.resetRows(0);
		this.#sink.update(this.#cache);
		this.#pump();
	}

	#pump(): void {
		const blocks = blocksToFetch(this.#cache, this.#start, this.#end, this.#policy);
		if (blocks.length === 0) {
			this.#sink.update(this.#cache);
			return;
		}
		const generation = this.#cache.generation;
		const requests = blocks.map((block) => blockRequest(this.#cache, block));
		this.#cache = markInFlight(this.#cache, blocks);
		this.#sink.update(this.#cache);
		for (const request of requests) {
			void this.#fetcher(request)
				// 結末を返す約束。それでも reject したら、黙って `loading` を
				// 抱えたままにしない。
				.catch((err: unknown): BlockOutcome<Row, F> => this.#onThrow(err))
				.then((outcome) => this.#settle(request.block, generation, outcome));
		}
	}

	#settle(block: number, generation: number, outcome: BlockOutcome<Row, F>): void {
		const result = applyOutcome(this.#cache, block, generation, outcome, this.#policy);
		this.#cache = result.cache;
		if (result.resetRows !== null) this.#sink.resetRows(result.resetRows);
		if (result.write) this.#sink.writeRows(result.write.offset, result.write.rows);
		this.#sink.update(this.#cache);
		// 境界が決まった直後に残りのブロックを投げる（世代の最初の 1 本だけを待つ）。
		this.#pump();
	}
}
