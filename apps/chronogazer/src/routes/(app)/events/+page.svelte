<script lang="ts">
	/**
	 * 収集イベント一覧（#383 段階2b / R1-C の C-3b。R1-A のプレースホルダを
	 * 置き換えたもの）。**`viewer` 以上**が閲覧できる
	 * （`chronogazer_core::collect::COLLECT_READ_ROLE`）。
	 *
	 * **監査ログ画面（`/audit-log`）の流儀にそのまま倣う**。新しい作法は
	 * 発明していない:
	 * - 一覧は `BantoGrid` の「サーバーモード」で、ブロック単位
	 *   （`BLOCK_SIZE` 件）にスクロールへ応じて遅延取得する。`@banto/admin-core`
	 *   の `createWindowedListResource` を使わない理由も同じ（専用のワイヤ形状・
	 *   専用の Tauri コマンド名で、汎用レジストリの外にある）。
	 * - 件数は総件数（`ListResult.totalCount`）を一覧の上に出す。
	 * - 並びは**新しい順**でサーバー側に固定。
	 * - デモモード（プレーンな `vite dev`/`preview`、backend 無し）では案内文だけ。
	 *
	 * **監査ログ画面と違うところは 3 つ**で、いずれもこの口の性質から来る:
	 *
	 * 1. **並べ替え・絞り込みが無い**（`GET /api/collect/events?offset=&limit=`
	 *    には列名を渡す口が無い）。列に `sortable: false` を明示するのは、
	 *    押せるのに効かない見出しを出さないため - 押して何も変わらない
	 *    ソートは「画面が本当のことを言わない」型そのもの。
	 * 2. **`Readout` の 3 状態をそのまま見せる**。`unavailable`（読めなかった）を
	 *    **空一覧に潰さない** - 潰すと「0 件です」としか言えず、利用者は本当に
	 *    記録が無いのか読めなかったのかを区別できない
	 *    （docs/implementation-checklist.md §5）。読めなかったときは**行も件数も
	 *    消さず**、注記と「再読み込み」を出す。
	 * 3. **`detail` 列が無い**。C-3a が型でも SQL でも落としている（自由文で、
	 *    切断理由や書き込み先のファイルパスを含みうる）ので、**無い列を作らない** -
	 *    行をクリックしても出す詳細が無いため、監査ログ画面のような詳細ペインも
	 *    置いていない。切断理由が画面に要ると判断したら、発生源
	 *    （`banto-collect`）で「見せてよい理由」を分類するのが筋で、ここでは
	 *    やらない。
	 *
	 * **ブロックキャッシュの判断は `./eventBlocks.ts` に出してある**（#409
	 * オーナーレビュー P2 の 3 件: 初回失敗からの回復・ブロック境界の重複と
	 * 欠落・別ブロックの成功が失敗を消すこと）。どのブロックを取るか、世代の
	 * スナップショット境界（`asOfId`）、失敗の持ち方、「再読み込み」で何を
	 * 取り直すかは全部あちらの純関数が決め、表テストで固定してある。
	 */
	import { untrack } from 'svelte';
	import { BantoGrid, GridState, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import {
		COLLECT_READ_TIMEOUT_MS,
		DEMO_MODE_MESSAGE,
		collectEventsNote,
		collectTimeLabel,
		isCollectAvailable,
		listCollectEvents,
		runWithLimit,
		type CollectEventRow
	} from '$lib/banto/collectAdmin';
	import {
		EventBlockLoader,
		initialCache,
		viewState,
		type BlockOutcome,
		type BlockRequest,
		type EventsViewState
	} from './eventBlocks';

	/** `/audit-log` の同名関数と同じ（この画面に検証エラーは返らない）。 */
	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const available = isCollectAvailable();

	/**
	 * `banto_collect::EventKind::as_str` の綴り → 日本語。**語彙を決めている
	 * のは `banto-collect`** なので、知らない種類が来たら**綴りをそのまま出す**
	 * （落としも失敗もしない）。監査ログ画面の `actionLabel` と同じ作法。
	 */
	const kindLabels: Record<string, string> = {
		collection_started: '収集開始',
		collection_stopped: '収集停止',
		plc_connected: 'PLC接続',
		plc_disconnected: 'PLC切断',
		plc_reconnected: 'PLC再接続',
		threshold_entered: 'しきい値超過',
		threshold_exited: 'しきい値復帰',
		append_failure_entered: '書き込み失敗',
		append_failure_exited: '書き込み復帰'
	};

	function kindLabel(kind: string): string {
		return kindLabels[kind] ?? kind;
	}

	/**
	 * 列は `CollectEventRow` のフィールドそのまま（`detail` は**そもそも
	 * 返ってこない**ので列にしない）。`sortable`/`filterable` を落としてあるのは
	 * この口が並べ替え・絞り込みを受け取らないため（上の doc comment 1.）。
	 */
	const columns: GridColumn<CollectEventRow>[] = [
		{
			id: 'tsMs',
			header: '時刻',
			accessor: 'tsMs',
			width: 175,
			sortable: false,
			format: (value) => collectTimeLabel(Number(value))
		},
		{
			id: 'kind',
			header: '種類',
			accessor: 'kind',
			width: 140,
			sortable: false,
			format: (value) => kindLabel(String(value))
		},
		{
			id: 'connectionKey',
			header: '接続',
			accessor: (row) => row.connectionKey ?? '-',
			width: 120,
			sortable: false
		},
		{
			id: 'tagKey',
			header: 'タグ',
			accessor: (row) => row.tagKey ?? '-',
			width: 120,
			sortable: false
		},
		{
			id: 'level',
			header: '水準',
			accessor: (row) => row.level ?? '-',
			width: 80,
			sortable: false
		},
		{
			id: 'value',
			header: '値',
			accessor: (row) => row.value ?? '-',
			width: 110,
			align: 'right',
			sortable: false
		}
	];

	const gridState = new GridState<CollectEventRow>(columns);

	/**
	 * ブロックキャッシュの**判断**は `./eventBlocks.ts`（純関数 +
	 * `EventBlockLoader`）に出してあり、この画面はそれを呼んで `$state` に
	 * 書き戻すだけ（#409 オーナーレビュー P2 の 3 件。`tagsPageLogic.ts` と
	 * 同じ作法で、状態遷移を表テストで固定する）。ここに残っているのは:
	 *
	 * - 取得 1 本の作り方（**読み取り 1 回に上限を掛ける** -
	 *   `COLLECT_READ_TIMEOUT_MS`。backend は 2 秒で必ず `unavailable` を
	 *   返すが、**reject ではなく無応答**の相手だと `catch` に入らないまま
	 *   `loading` が降りず、一覧が永久に「読み込み中」で固まる。
	 *   `HubSection.svelte` のポーリングで踏んだのと同じ型）、
	 * - 行の配列（`$state`）への書き戻し。
	 */
	let rows = $state<(CollectEventRow | undefined)[]>([]);
	let view = $state<EventsViewState>(viewState(initialCache()));

	/** 1 ブロックの取得。**結末を返し、reject しない**（`runWithLimit` の約束）。 */
	async function fetchBlock(request: BlockRequest): Promise<BlockOutcome> {
		const outcome = await runWithLimit(
			(signal) => listCollectEvents(request.offset, request.limit, request.asOfId, signal),
			COLLECT_READ_TIMEOUT_MS
		);
		if (outcome.kind === 'failed') return { kind: 'error', message: errorMessage(outcome.error) };
		if (outcome.kind === 'timedOut') {
			// 打ち切りは**失敗ではない**（アプリ側の読み取りは続いている）。
			// それでも画面にとっては「今は読めていない」なので、黙らない。
			return {
				kind: 'error',
				message: `イベントの読み取りが${Math.round(COLLECT_READ_TIMEOUT_MS / 1000)}秒以内に返りませんでした。待つのをやめただけなので、「再読み込み」でもう一度試せます。`
			};
		}
		// 読めなかった（`unavailable`）ときは**行も件数も触らない** -
		// 0 件に潰すと「記録がありません」という別の嘘になる。
		if (outcome.value.state !== 'ready') return { kind: 'readout', readout: outcome.value.state };
		return { kind: 'ready', list: outcome.value.data };
	}

	const loader = new EventBlockLoader(fetchBlock, {
		resetRows(length) {
			// 世代の最初の応答。前の世代の行は境界がずれているので残さない。
			rows = new Array<CollectEventRow | undefined>(length);
		},
		writeRows(offset, block) {
			if (rows.length < offset + block.length) rows.length = offset + block.length;
			for (let i = 0; i < block.length; i++) rows[offset + i] = block[i];
		},
		update(next) {
			view = next;
		}
	});

	// `untrack`: 取得は effect の追跡スコープ内で `rows`/`view` を読み書きする
	// ので、これが無いと自分の書き込みで再実行し続ける（`/audit-log` の同じ
	// effect と同じ理由・同じ書き方）。
	$effect(() => {
		if (!available) return;
		untrack(() => loader.setRange(0, 100));
	});

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		loader.setRange(range.start, range.end);
	}

	/**
	 * 手で押す「再読み込み」。**自動では再試行しない**ので（「自動で再試行
	 * します」と書いたら本当にやる、の裏返し）、読めなかったときに利用者が
	 * 抜け出せる手段をここに 1 つだけ置く。取り直す対象（失敗したブロック /
	 * 総件数が未取得なら先頭ブロック）は `eventBlocks.ts` の
	 * `blocksToFetch` が決める。
	 */
	function reload(): void {
		loader.reload();
	}
</script>

<div class="page">
	<div class="page-header">
		<h2>イベント</h2>
	</div>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードには収集ランタイムがないため、この機能はデスクトップアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<!--
			「読めなかった」「0 件」「まだ一度も読めていない」を別々に出す。
			`readout` が `null` のうちは件数を言わない（0 件と言い切らない）。
		-->
		<p class="note">
			{#if view.readout === null}
				イベントを読み込んでいます。
			{:else}
				{collectEventsNote(view.readout, view.totalCount)}
			{/if}
		</p>

		{#if view.errorText}
			<p class="error">{view.errorText}</p>
		{/if}

		<!--
			**失敗はブロック単位**なので、別のブロックが読めても消えない
			（#409 レビュー P2-3）。1 つでも失敗が残っているうちは、再試行の
			導線を出し続ける。
		-->
		{#if view.failedBlockCount > 0}
			<div class="actions">
				<button type="button" onclick={reload} disabled={view.loading}>再読み込み</button>
			</div>
		{/if}

		<section class="grid-wrap">
			<BantoGrid
				mode="server"
				state={gridState}
				{rows}
				totalRows={view.totalCount}
				{columns}
				getRowId={(row) => row.id}
				onVisibleRangeChange={handleVisibleRangeChange}
			/>
		</section>
	{/if}
</div>

<style>
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
		gap: 0.5rem;
	}

	.page-header {
		flex: 0 0 auto;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.note {
		flex: 0 0 auto;
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.error {
		flex: 0 0 auto;
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.actions {
		flex: 0 0 auto;
	}

	.actions button {
		padding: 0.35rem 0.8rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		color: var(--banto-text);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.actions button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}
</style>
