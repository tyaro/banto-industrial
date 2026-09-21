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
		type CollectEventRow,
		type ReadoutState
	} from '$lib/banto/collectAdmin';

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

	const BLOCK_SIZE = 200;

	/**
	 * `/audit-log` の `AuditLogWindow` の縮小コピー（ブロック単位フェッチ・
	 * 世代カウンタによる競合防止は同じ）。違いは 2 つ:
	 *
	 * - 取得が `Readout` なので、**`unavailable` を空に潰さず `readout` に
	 *   残す**（行も総件数もそのまま残し、注記だけを足す）。
	 * - 並べ替え・絞り込みが無いので `params`/`setParams` を持たない。
	 *
	 * **読み取り 1 回に上限を掛ける**（`COLLECT_READ_TIMEOUT_MS`）。backend は
	 * 2 秒で必ず `unavailable` を返すが、**reject ではなく無応答**の相手
	 * （TCP は繋がるが応答が返らない）だと `catch` に入らないまま `loading` が
	 * 降りず、一覧が永久に「読み込み中」で固まる - `HubSection.svelte` の
	 * ポーリングで踏んだのと同じ型。
	 */
	class CollectEventsWindow {
		rows: (CollectEventRow | undefined)[] = $state([]);
		totalCount = $state(0);
		loading = $state(false);
		/** 最後に**読めた**結末。`null` = まだ一度も読めていない。 */
		readout = $state<ReadoutState | null>(null);
		/** 往復そのものが失敗した（アプリに届かなかった／打ち切った）ときの文言。 */
		error = $state<string | null>(null);

		#loadedBlocks = new Set<number>();
		#inFlightBlocks = new Map<number, Promise<void>>();
		#generation = 0;
		#hasTotalCountForGeneration = false;

		#blocksFor(start: number, end: number): number[] {
			if (end <= start) return [];
			const firstBlock = Math.floor(start / BLOCK_SIZE);
			const lastBlock = Math.floor((end - 1) / BLOCK_SIZE);
			const blocks: number[] = [];
			for (let b = firstBlock; b <= lastBlock; b++) blocks.push(b);
			return blocks;
		}

		async ensureRange(start: number, end: number): Promise<void> {
			const generation = this.#generation;
			const blocks = this.#blocksFor(start, end).filter(
				(block) => !this.#loadedBlocks.has(block) && !this.#inFlightBlocks.has(block)
			);
			if (blocks.length === 0) return;

			this.loading = true;
			const fetches = blocks.map((block) => this.#fetchBlock(block, generation));
			blocks.forEach((block, i) => this.#inFlightBlocks.set(block, fetches[i]));
			try {
				await Promise.all(fetches);
			} finally {
				if (generation === this.#generation) {
					blocks.forEach((block) => this.#inFlightBlocks.delete(block));
					this.loading = this.#inFlightBlocks.size > 0;
				}
			}
		}

		async #fetchBlock(block: number, generation: number): Promise<void> {
			const offset = block * BLOCK_SIZE;
			const outcome = await runWithLimit(
				(signal) => listCollectEvents(offset, BLOCK_SIZE, signal),
				COLLECT_READ_TIMEOUT_MS
			);
			if (generation !== this.#generation) return;

			if (outcome.kind === 'failed') {
				this.error = errorMessage(outcome.error);
				return;
			}
			if (outcome.kind === 'timedOut') {
				// 打ち切りは**失敗ではない**（アプリ側の読み取りは続いている）。
				// それでも画面にとっては「今は読めていない」なので、黙らない。
				this.error = `イベントの読み取りが${Math.round(COLLECT_READ_TIMEOUT_MS / 1000)}秒以内に返りませんでした。待つのをやめただけなので、「再読み込み」でもう一度試せます。`;
				return;
			}

			this.error = null;
			this.readout = outcome.value.state;
			// 読めなかった（`unavailable`）ときは**行も件数も触らない** -
			// 0 件に潰すと「記録がありません」という別の嘘になる。
			if (outcome.value.state !== 'ready') return;

			const result = outcome.value.data;
			if (!this.#hasTotalCountForGeneration) {
				this.#hasTotalCountForGeneration = true;
				this.totalCount = result.totalCount;
				this.rows.length = result.totalCount;
			}
			if (this.rows.length < offset + result.rows.length) {
				this.rows.length = offset + result.rows.length;
			}
			for (let i = 0; i < result.rows.length; i++) {
				this.rows[offset + i] = result.rows[i];
			}
			this.#loadedBlocks.add(block);
		}

		/**
		 * 手で押す「再読み込み」。**自動では再試行しない**ので（「自動で再試行
		 * します」と書いたら本当にやる、の裏返し）、読めなかったときに利用者が
		 * 抜け出せる手段をここに 1 つだけ置く。
		 *
		 * **`loading` をここで降ろす**のが要点。世代を進めた時点で飛行中の取得は
		 * もう自分のものではなく、その `ensureRange` の `finally` は世代違いで
		 * `loading` を触らずに抜ける - 降ろさないと `loading` が true のまま
		 * 残り、**この「再読み込み」ボタンが二度と押せなくなる**（唯一の回復
		 * 導線が、回復したいときだけ死ぬ）。飛行中の分は `#inFlightBlocks` ごと
		 * 捨てているので、この世代に飛んでいるものは 0 件で正しい。
		 */
		reload(start: number, end: number): void {
			this.#generation++;
			this.#loadedBlocks.clear();
			this.#inFlightBlocks.clear();
			this.#hasTotalCountForGeneration = false;
			this.loading = false;
			void this.ensureRange(start, end);
		}
	}

	const windowed = new CollectEventsWindow();

	// `untrack`: `ensureRange()` は effect の追跡スコープ内で `rows`/`loading`
	// を読み書きするので、これが無いと自分の書き込みで再実行し続ける
	// （`/audit-log` の同じ effect と同じ理由・同じ書き方）。
	$effect(() => {
		if (!available) return;
		untrack(() => void windowed.ensureRange(0, 100));
	});

	let visibleRange = { start: 0, end: 100 };

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		visibleRange = range;
		void windowed.ensureRange(range.start, range.end);
	}

	function reload(): void {
		windowed.reload(visibleRange.start, visibleRange.end);
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
			{#if windowed.readout === null}
				イベントを読み込んでいます。
			{:else}
				{collectEventsNote(windowed.readout, windowed.totalCount)}
			{/if}
		</p>

		{#if windowed.error}
			<p class="error">{windowed.error}</p>
		{/if}

		{#if windowed.error || windowed.readout === 'unavailable' || windowed.readout === 'notRunning'}
			<div class="actions">
				<button type="button" onclick={reload} disabled={windowed.loading}>再読み込み</button>
			</div>
		{/if}

		<section class="grid-wrap">
			<BantoGrid
				mode="server"
				state={gridState}
				rows={windowed.rows}
				totalRows={windowed.totalCount}
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
