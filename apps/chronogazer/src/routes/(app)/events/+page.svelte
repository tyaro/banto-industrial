<script lang="ts">
	/**
	 * 収集イベント一覧（#383 段階2b / R1-C の C-3b。R1-A のプレースホルダを
	 * 置き換えたもの）。**`viewer` 以上**が閲覧できる
	 * （`chronogazer_core::collect::COLLECT_READ_ROLE`）。
	 *
	 * **監査ログ画面（`/audit-log`）の流儀にそのまま倣う**。新しい作法は
	 * 発明していない:
	 * - 一覧は `BantoGrid` の「サーバーモード」で、ブロック単位（200 件）に
	 *   スクロールへ応じて遅延取得する。ブロック読み込みは `@banto/admin-core`
	 *   の `createSnapshotListResource`（banto #248。監査ログ画面と同じもの）に
	 *   任せる（`collectAdmin.ts` の `createCollectEventsResource`）。
	 * - 件数は総件数を一覧の上に出す。
	 * - 並びは**新しい順**でサーバー側に固定。
	 * - デモモード（プレーンな `vite dev`/`preview`、backend 無し）では案内文だけ。
	 *
	 * **監査ログ画面と違うところは 4 つ**で、いずれもこの口の性質から来る:
	 *
	 * 1. **並べ替え・絞り込みが無い**（`GET /api/collect/events?offset=&limit=`
	 *    には列名を渡す口が無い）。列に `sortable: false` を明示するのは、
	 *    押せるのに効かない見出しを出さないため - 押して何も変わらない
	 *    ソートは「画面が本当のことを言わない」型そのもの。
	 * 2. **`Readout` の 3 状態をそのまま見せる**。`unavailable`（読めなかった）を
	 *    **空一覧に潰さない** - 潰すと「0 件です」としか言えず、利用者は本当に
	 *    記録が無いのか読めなかったのかを区別できない
	 *    （docs/implementation-checklist.md §5）。読めなかったブロックは取得関数が
	 *    `EventsReadoutError` を投げて**ブロック単位の失敗**にし、**行も件数も
	 *    消さず**、注記と「再読み込み」を出す（`collectAdmin.ts` の `eventsView`）。
	 * 3. **失敗のトーストを出さない**（2026-10-06 オーナー決定、`notify: false`）。
	 *    失敗は注記と赤字で出す。読めなかったブロックの注記と、別のブロックの
	 *    往復の失敗の赤字は**両方出ることがある**。
	 * 4. **`detail` 列が無い**。C-3a が型でも SQL でも落としている（自由文で、
	 *    切断理由や書き込み先のファイルパスを含みうる）ので、**無い列を作らない** -
	 *    行をクリックしても出す詳細が無いため、監査ログ画面のような詳細ペインも
	 *    置いていない。切断理由が画面に要ると判断したら、発生源
	 *    （`banto-collect`）で「見せてよい理由」を分類するのが筋で、ここでは
	 *    やらない。
	 *
	 * 以前はブロックキャッシュの判断を自前で持っていた（`./eventBlocks.ts` と
	 * `#lib/blockCache.ts`、#409/#410）。挙動の約束（世代の最初の応答で境界を
	 * 固定する・失敗はブロック単位でそのブロックが読めるまで残る・自動では
	 * 再試行しない・初回失敗の後も「再読み込み」で先頭ブロックを取り直す）は
	 * banto の `SnapshotListResource` が同じものを持つ。
	 */
	import { untrack } from 'svelte';
	import { BantoGrid, GridState, type GridColumn } from '@banto/grid-svelte';
	import {
		DEMO_MODE_MESSAGE,
		collectTimeLabel,
		createCollectEventsResource,
		eventKindLabel,
		eventsView,
		isCollectAvailable,
		type CollectEventRow
	} from '#lib/banto/collectAdmin.js';

	const available = isCollectAvailable();

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
			format: (value) => eventKindLabel(String(value))
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
	 * イベント一覧のブロック読み込み（banto #248 の `SnapshotListResource`）。
	 * 読み取り 1 回の上限は `COLLECT_READ_TIMEOUT_MS`（4 秒。backend は 2 秒で
	 * 必ず `unavailable` を返すので、それを超えて何も返らないのは「届いて
	 * いない」）をリソースの `requestTimeoutMs` で掛ける - **reject ではなく
	 * 無応答**の相手でも `loading` が降り、失敗として残る。上限・失敗の
	 * 持ち方・世代違いの応答の破棄・新しい世代での中断はリソース側が行う。
	 */
	const events = createCollectEventsResource();

	// 注記・赤字・失効の案内（`failures` はブロック順。いちばん前の失敗を出す）。
	const view = $derived(
		eventsView({
			failures: events.failures,
			totalCount: events.totalCount,
			expired: events.expired
		})
	);

	// `untrack`: 初回の読み込みはマウント時に 1 回。`ensureRange()` が公開する
	// 状態（`loading` など）にこの effect を依存させない（`/audit-log` の同じ
	// effect と同じ理由・同じ書き方）。後始末で飛行中の要求を中断する。
	$effect(() => {
		if (!available) return;
		untrack(() => events.ensureRange(0, 100));
		return () => events.dispose();
	});

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		events.ensureRange(range.start, range.end);
	}

	/**
	 * 手で押す「再読み込み」= 新しい世代。**自動では再試行しない**ので
	 * （「自動で再試行します」と書いたら本当にやる、の裏返し）、読めなかった
	 * ときに利用者が抜け出せる手段をここに 1 つだけ置く。失敗したブロック
	 * （総件数が未取得なら先頭ブロック）も取り直す。**処理中でも押せる**
	 * （2026-10-06 オーナー決定。監査ログ画面・banto と同じ）: 処理中の要求は
	 * 中断され、新しい世代の要求に置き換わる。
	 */
	function reload(): void {
		events.refresh();
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
			まだ一度も読めていないうちは件数を言わない（0 件と言い切らない）。
		-->
		<p class="note">{view.note}</p>

		{#if view.errorText}
			<p class="error">{view.errorText}</p>
		{/if}
		{#if view.expiredText}
			<p class="error">{view.expiredText}</p>
		{/if}

		<!--
			**「再読み込み」は常に出す**（#409 レビュー P2-4）。失敗からの再試行
			だけでなく、**新しいイベントを取り込む唯一の導線**でもあるため:
			一覧は世代の最初の応答で `asOfId`（スナップショット境界）を固定し、
			同じ世代の後続ブロックにもその境界を渡す（P2-2 の修正）ので、
			**新しい世代を始めない限り、後から記録されたイベントは決して入らない**。
			失敗しているときだけ出していると、0 件で正常に読めた画面に留まる限り
			「まだ1件も記録されていません」のままになる。

			**失敗はブロック単位**なので、別のブロックが読めても消えない
			（#409 レビュー P2-3）- 失敗の表示（上の `error` と注記）は、その
			ブロックが読めるまで残る。

			**自動ポーリングは足さない**（オーナー指定）: 明示的に世代を始め
			られれば十分で、勝手に世代を切ると読んでいる途中で行が入れ替わる。

			**処理中でも押せる**（2026-10-06 オーナー決定。監査ログ画面と同じ）。
		-->
		<div class="actions">
			<button type="button" onclick={reload}>再読み込み</button>
		</div>

		<section class="grid-wrap">
			<BantoGrid
				mode="server"
				state={gridState}
				rows={events.rows}
				totalRows={events.totalCount ?? 0}
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

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}
</style>
