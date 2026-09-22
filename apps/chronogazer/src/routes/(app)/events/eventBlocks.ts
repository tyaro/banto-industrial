/**
 * イベント一覧（`/events`）の**ブロックキャッシュの判断**（#409 オーナー
 * レビュー P2 の 3 件）。`tagsPageLogic.ts` / `hubAdmin.ts` と同じ作法で、
 * 状態遷移を**純関数**に出して表テストで固定し、画面（`+page.svelte`）は
 * これを呼ぶだけにする。
 *
 * ここが決めるのは 5 つ:
 *
 * 1. **どのブロックを取るか**（[`blocksToFetch`]。表示範囲・総件数の有無・
 *    失敗したブロックを 1 つの述語にまとめる - 「再読み込みで何を取り直すか」
 *    を別々に列挙しない）、
 * 2. **何を渡して取るか**（[`blockRequest`]。世代のスナップショット境界
 *    `asOfId`）、
 * 3. **応答をどこに入れるか**（[`applyOutcome`]。世代違いの応答は採らない）、
 * 4. **失敗をどう持つか**（[`applyOutcome`]。**ブロック単位**。別ブロックの
 *    成功で消さない）、
 * 5. **画面に何を出すか**（[`viewState`]）。
 *
 * ### なぜ「世代」と `asOfId`（スナップショット境界）が要るか
 *
 * サーバーは各リクエスト時点のデータを `ORDER BY ts DESC, id DESC LIMIT …
 * OFFSET …` で返す。ブロック取得の**合間に先頭へイベントが追加される**と
 * `OFFSET` がずれ、境界で行が重複し、末尾の行が一覧から漏れる。そこで
 * **同じ世代のブロックは同じデータ集合から取る**: 世代の最初の応答が返した
 * 境界（`collect_events.id` の最大値）を固定し、後続ブロックにすべて渡す。
 * `collect_events.id` は `INTEGER PRIMARY KEY AUTOINCREMENT` なので単調増加
 * かつ再利用されず、`id <= asOfId` で**集合のメンバーが確定する**。
 *
 * **「件数が増えたら全体を取り直す」方式を採らなかった理由**: イベントが
 * 流れ続けている間（接続が瞬断を繰り返しているなど、まさにこの一覧を見たい
 * とき）は毎回取り直しになり、**一覧が永久に読み終わらない**。これも
 * 「回復導線が、必要なときだけ死ぬ」型になる。新しいイベントは利用者が
 * 「再読み込み」した（= 新しい世代を始めた）ときに入る。
 *
 * **将来の制約**: 保持期間による削除（R0 §3.4、**まだ未実装**）が入ると、
 * 古い行（小さい `id`）が消えて末尾側の `OFFSET` がずれる。削除を実装する
 * ときは、この取得方法（サーバー側の `read_events` も含めて）を見直すこと。
 *
 * ### 自動では再試行しない
 *
 * **今の世代で失敗したブロックは、スクロールでは取り直さない**（失敗の直後に
 * 同じ要求が飛び続けるのを避ける）。取り直すのは利用者が「再読み込み」を
 * 押したとき（= 新しい世代）だけで、**失敗の表示と再試行手段は、その
 * ブロックの取得が成功するまで残る** - 別ブロックの成功では消さない。
 */
import type { CollectEventList, CollectEventRow, ReadoutState } from '$lib/banto/collectAdmin';

/** 1 ブロックの件数（`/audit-log` の `AuditLogWindow` と同じ 200）。 */
export const BLOCK_SIZE = 200;

/**
 * ブロック 1 つの取得が**行を入れられなかった**ときの持ち方。
 *
 * - `readout`: サーバーは答えたが「読めなかった」/「走っていない」
 *   （`Readout` の 3 状態を空に潰さない）、
 * - `error`: 往復そのものが失敗した / こちらが待つのをやめた。
 */
export type BlockFailure =
	{ kind: 'readout'; readout: Exclude<ReadoutState, 'ready'> } | { kind: 'error'; message: string };

/** ブロック 1 つの取得の結末（`Readout` と往復の失敗を 1 つにしたもの）。 */
export type BlockOutcome = { kind: 'ready'; list: CollectEventList } | BlockFailure;

/** [`blockRequest`] が返す 1 ブロック分の要求。 */
export interface BlockRequest {
	block: number;
	offset: number;
	limit: number;
	/** 世代のスナップショット境界。`null` = この世代の最初の取得（境界を決めさせる）。 */
	asOfId: number | null;
}

/** ブロックキャッシュの状態（**不変**。純関数が新しい値を返す）。 */
export interface BlockCache {
	/** 「再読み込み」で進む。世代違いの応答は採らない。 */
	generation: number;
	/** この世代の境界と総件数。`null` = この世代はまだ 1 度も読めていない。 */
	snapshot: { asOfId: number; totalCount: number } | null;
	loaded: ReadonlySet<number>;
	inFlight: ReadonlySet<number>;
	/** ブロックごとの失敗と、それを記録した世代。 */
	failed: ReadonlyMap<number, { failure: BlockFailure; generation: number }>;
	/** 一度でも読めたか（「まだ読み込んでいない」と「0 件」の言い分け）。 */
	everRead: boolean;
	/** 画面に出している総件数（世代をまたいで保つ - 読めなかったときに 0 に潰さない）。 */
	totalCount: number;
}

/** 画面に出す値（[`viewState`] が [`BlockCache`] から導く）。 */
export interface EventsViewState {
	loading: boolean;
	/** `null` = まだ一度も読めていない（「0 件」と言い切らない）。 */
	readout: ReadoutState | null;
	errorText: string | null;
	/** 取得に失敗したままのブロック数（0 なら「再読み込み」を出さない）。 */
	failedBlockCount: number;
	totalCount: number;
}

/** [`applyOutcome`] の結果。行の書き込みは画面（`$state`）に任せる。 */
export interface ApplyResult {
	cache: BlockCache;
	/** 非 `null` なら、行の配列をこの長さで**作り直す**（世代の最初の応答）。 */
	resetRows: number | null;
	/** 非 `null` なら、この位置から行を書き込む。 */
	write: { offset: number; rows: CollectEventRow[] } | null;
}

/**
 * 要求した境界と違う境界の応答が返ったときの文言。**採らない**（採ると
 * 重複・欠落が戻る）。サーバーが `asOfId` を無視している場合にここへ来るので、
 * 黙って取り直し続けない（無限の取り直しになる）。
 */
export const BOUNDARY_MISMATCH_MESSAGE =
	'イベントの取得範囲がサーバー側で切り替わりました。「再読み込み」でもう一度読み込んでください。';

export function initialCache(): BlockCache {
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

/**
 * **何を取るかを決める唯一の述語**（スクロールでも「再読み込み」でも同じ）。
 *
 * 取りたいのは:
 * - 表示範囲のブロック、
 * - **総件数をまだ取れていない世代では先頭ブロック**（表示範囲が空でも）。
 *   初期状態は総件数 0 なので `BantoGrid` が通知する表示範囲は `{0, 0}` に
 *   なる - これを取りこぼすと、初回取得が失敗した後「再読み込み」を押しても
 *   **リクエストが 1 本も出ない**（サーバーが復旧しても回復できない）、
 * - **前の世代で失敗したブロック**（「再読み込み」が取り直す対象）。
 *
 * ただし、すでに取れている・飛行中・**今の世代で失敗した**ブロックは除く
 * （最後のひとつがモジュール doc の「自動では再試行しない」）。
 *
 * **この世代でまだ境界が決まっていない間は 1 ブロックしか投げない**: 並列に
 * 投げると、それぞれが別の境界（別のデータ集合）を返してしまい、直したはずの
 * 重複・欠落が戻る。最初の応答で境界が決まった後、残りは並列に取る。
 */
export function blocksToFetch(cache: BlockCache, start: number, end: number): number[] {
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
export function blockRequest(cache: BlockCache, block: number): BlockRequest {
	return {
		block,
		offset: block * BLOCK_SIZE,
		limit: BLOCK_SIZE,
		asOfId: cache.snapshot?.asOfId ?? null
	};
}

export function markInFlight(cache: BlockCache, blocks: readonly number[]): BlockCache {
	const inFlight = new Set(cache.inFlight);
	for (const block of blocks) inFlight.add(block);
	return { ...cache, inFlight };
}

/**
 * 「再読み込み」= **新しい世代**。境界を外し（新しいイベントはここで入る）、
 * 取得済み・飛行中を捨てる。
 *
 * **失敗は捨てない**: 失敗の表示は、そのブロックの取得が成功するまで残す。
 * 世代が変わったことで [`blocksToFetch`] の対象に戻る（= 取り直す）。
 *
 * **飛行中を捨てるので `loading` はここで降りる**（#409 の C-3b 自己監査で
 * 直した「唯一の回復導線が、回復したいときだけ押せなくなる」の再発防止）。
 */
export function newGeneration(cache: BlockCache): BlockCache {
	return {
		...cache,
		generation: cache.generation + 1,
		snapshot: null,
		loaded: new Set(),
		inFlight: new Set()
	};
}

/**
 * 1 ブロックの結末を取り込む。
 *
 * - **世代違いの応答は採らない**（飛行中の古い応答が新しい状態を巻き戻さない）、
 * - 失敗は**そのブロックに**記録する（別ブロックの成功で消えない）、
 * - 世代の最初の `ready` で境界と総件数を固定し、**行の配列を作り直す**
 *   （前の世代の行は境界がずれているので残さない）。
 */
export function applyOutcome(
	cache: BlockCache,
	block: number,
	generation: number,
	outcome: BlockOutcome
): ApplyResult {
	if (generation !== cache.generation) return { cache, resetRows: null, write: null };

	const inFlight = new Set(cache.inFlight);
	inFlight.delete(block);
	const failed = new Map(cache.failed);

	if (outcome.kind !== 'ready') {
		failed.set(block, { failure: outcome, generation });
		return { cache: { ...cache, inFlight, failed }, resetRows: null, write: null };
	}

	if (cache.snapshot !== null && outcome.list.asOfId !== cache.snapshot.asOfId) {
		failed.set(block, {
			failure: { kind: 'error', message: BOUNDARY_MISMATCH_MESSAGE },
			generation
		});
		return { cache: { ...cache, inFlight, failed }, resetRows: null, write: null };
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

/**
 * 画面に出す値。**失敗しているブロックが 1 つでもあれば、失敗を出し続ける**
 * （別ブロックが成功していても `readout: 'ready'` で塗り潰さない）。
 */
export function viewState(cache: BlockCache): EventsViewState {
	const failures = [...cache.failed.entries()].sort(([a], [b]) => a - b);
	const readoutFailure = failures.find(([, record]) => record.failure.kind === 'readout');
	const errorFailure = failures.find(([, record]) => record.failure.kind === 'error');
	return {
		loading: cache.inFlight.size > 0,
		readout:
			readoutFailure && readoutFailure[1].failure.kind === 'readout'
				? readoutFailure[1].failure.readout
				: cache.everRead
					? 'ready'
					: null,
		errorText:
			errorFailure && errorFailure[1].failure.kind === 'error'
				? errorFailure[1].failure.message
				: null,
		failedBlockCount: cache.failed.size,
		totalCount: cache.totalCount
	};
}

/** 取得を 1 本走らせる関数（画面は `runWithLimit` + `listCollectEvents`、テストは偽物）。 */
export type BlockFetcher = (request: BlockRequest) => Promise<BlockOutcome>;

/** 画面（`$state`）への書き戻し口。 */
export interface EventBlockSink {
	/** 行の配列をこの長さで作り直す。 */
	resetRows(length: number): void;
	writeRows(offset: number, rows: CollectEventRow[]): void;
	update(view: EventsViewState): void;
}

/**
 * 上の純関数を繋ぐだけの駆動役（Svelte に依存しないので vitest から素で
 * 回せる）。画面はこれに表示範囲と「再読み込み」を伝え、`sink` で受け取る。
 */
export class EventBlockLoader {
	#cache = initialCache();
	#start = 0;
	#end = 0;
	readonly #fetcher: BlockFetcher;
	readonly #sink: EventBlockSink;

	constructor(fetcher: BlockFetcher, sink: EventBlockSink) {
		this.#fetcher = fetcher;
		this.#sink = sink;
	}

	/** テストと画面の点検用（状態の読み取りだけ）。 */
	get cache(): BlockCache {
		return this.#cache;
	}

	/** 表示範囲が変わった（初回も含む）。 */
	setRange(start: number, end: number): void {
		this.#start = start;
		this.#end = end;
		this.#pump();
	}

	/** 手で押す「再読み込み」= 新しい世代（新しいイベントもここで入る）。 */
	reload(): void {
		this.#cache = newGeneration(this.#cache);
		this.#sink.update(viewState(this.#cache));
		this.#pump();
	}

	#pump(): void {
		const blocks = blocksToFetch(this.#cache, this.#start, this.#end);
		if (blocks.length === 0) {
			this.#sink.update(viewState(this.#cache));
			return;
		}
		const generation = this.#cache.generation;
		const requests = blocks.map((block) => blockRequest(this.#cache, block));
		this.#cache = markInFlight(this.#cache, blocks);
		this.#sink.update(viewState(this.#cache));
		for (const request of requests) {
			void this.#fetcher(request)
				// `fetcher` は結末を返す約束（画面側が `runWithLimit` で包む）。
				// それでも reject したら、黙って `loading` を抱えたままにしない。
				.catch((err: unknown): BlockOutcome => ({ kind: 'error', message: String(err) }))
				.then((outcome) => this.#settle(request.block, generation, outcome));
		}
	}

	#settle(block: number, generation: number, outcome: BlockOutcome): void {
		const result = applyOutcome(this.#cache, block, generation, outcome);
		this.#cache = result.cache;
		if (result.resetRows !== null) this.#sink.resetRows(result.resetRows);
		if (result.write) this.#sink.writeRows(result.write.offset, result.write.rows);
		this.#sink.update(viewState(this.#cache));
		// 境界が決まった直後に残りのブロックを投げる（世代の最初の 1 本だけを
		// 待つ設計。[`blocksToFetch`]）。
		this.#pump();
	}
}
