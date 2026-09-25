<script lang="ts">
	/**
	 * 監査ログ閲覧画面。`admin` のみ到達（+page.ts が非adminをリダイレクト）。
	 * chronogazer の同名ファイルから複製し、以下を削除した:
	 * - デモモード分岐（`isAuditLogAvailable()`/`DEMO_MODE_MESSAGE`） -
	 *   banto-hub にはデモモードが存在しない。
	 * - 保持ポリシー表示（`getAuditConfig()`） - 複製した当時は banto-hub
	 *   バックエンドに `/api/audit-log/config` が無かった。今はある（P3-a、
	 *   admin 限定の `GET`/`PUT`）が、この画面の表示は戻していない
	 *   （`auditLogAdmin.ts` の doc comment 参照）。
	 *
	 * 一覧は BantoGrid の「サーバーモード」: ソート/フィルタ/ページングは
	 * すべて `listAuditLog()`（Rust側 `ListParams` -> SQL）が行い、ブロック
	 * 単位（`BLOCK_SIZE`件）でスクロールに応じて遅延取得する
	 * （`@banto/admin-core` の `createWindowedListResource` は汎用リソース
	 * レジストリ経由を前提にしており、監査ログはその外にある）。
	 *
	 * **ブロックキャッシュの判断は `./auditBlocks.ts`（と chronogazer の複製の
	 * `$lib/blockCache`）に出してある**（#428。以前はこのページ内に
	 * `AuditLogWindow` として複製していて、chronogazer の #410 と同じ欠陥を
	 * 持っていた）。どのブロックを取るか、世代のスナップショット境界
	 * （`asOfId`）、保持期間の削除による失効、失敗の持ち方、並べ替え・絞り込みの
	 * 変更と「再読み込み」で何を取り直すかは全部あちらの純関数が決め、表テストで
	 * 固定してある。
	 */
	import { untrack } from 'svelte';
	import {
		BantoGrid,
		GridState,
		type FilterState,
		type GridColumn,
		type SortState
	} from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import {
		AUDIT_LIST_TIMEOUT_MS,
		listAuditLog,
		type AuditLogEntry
	} from '$lib/banto/auditLogAdmin';
	import { runWithLimit } from '$lib/banto/runWithLimit';
	import { initialCache, type BlockRequest } from '$lib/blockCache';
	import {
		AUDIT_SNAPSHOT_EXPIRED_MESSAGE,
		AuditBlockLoader,
		auditViewState,
		type AuditBlockFailure,
		type AuditBlockOutcome,
		type AuditViewState
	} from './auditBlocks';

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const actionLabels: Record<string, string> = {
		create: '作成',
		update: '更新',
		delete: '削除',
		revoke: '失効',
		login: 'ログイン',
		login_failed: 'ログイン失敗',
		logout: 'ログアウト',
		setup: '初期セットアップ',
		password_reset: 'パスワードリセット',
		settings_change: '設定変更',
		denied: '権限拒否'
	};

	const resultLabels: Record<string, string> = {
		ok: '成功',
		denied: '拒否',
		failed: '失敗'
	};

	const originLabels: Record<string, string> = {
		rest: 'REST'
	};

	function actionLabel(action: string): string {
		return actionLabels[action] ?? action;
	}

	function resultLabel(result: string): string {
		return resultLabels[result] ?? result;
	}

	function originLabel(origin: string): string {
		return originLabels[origin] ?? origin;
	}

	const columns: GridColumn<AuditLogEntry>[] = [
		{ id: 'ts', header: '時刻', accessor: 'ts', width: 175 },
		{
			id: 'actorUsername',
			header: 'ユーザー',
			accessor: (row) => row.actorUsername ?? '-',
			width: 140,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'actorRole',
			header: 'ロール',
			accessor: (row) => row.actorRole ?? '-',
			width: 90
		},
		{
			id: 'action',
			header: 'アクション',
			accessor: 'action',
			width: 130,
			filterable: true,
			filterType: 'text',
			format: (value) => actionLabel(String(value))
		},
		{
			id: 'resource',
			header: 'リソース',
			accessor: 'resource',
			width: 110,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'entityId',
			header: '対象ID',
			accessor: (row) => row.entityId ?? '-',
			width: 90,
			align: 'right'
		},
		{
			id: 'origin',
			header: '経路',
			accessor: 'origin',
			width: 90,
			format: (value) => originLabel(String(value))
		},
		{
			id: 'result',
			header: '結果',
			accessor: 'result',
			width: 90,
			format: (value) => resultLabel(String(value))
		}
	];

	const gridState = new GridState<AuditLogEntry>(columns);
	// 既定ソート: 新しい記録が先頭に来るよう ts 降順。
	gridState.sort = [{ field: 'ts', direction: 'desc' }];

	/**
	 * 今の問い合わせ（並べ替え・絞り込み）。取得 1 本はこれを**投げる時点で**
	 * 読む - `AuditBlockLoader` は問い合わせが変わると世代を進めるので、前の
	 * 問い合わせで投げた応答は世代違いとして採られない。
	 */
	let query: { sort: SortState[]; filters: FilterState[] } = {
		sort: gridState.sort,
		filters: []
	};

	let rows = $state<(AuditLogEntry | undefined)[]>([]);
	let view = $state<AuditViewState>(auditViewState(initialCache<AuditBlockFailure>()));

	/**
	 * 1 ブロックの取得。**結末を返し、reject しない**。読み取り 1 回に上限を
	 * 掛ける（`AUDIT_LIST_TIMEOUT_MS`）- **reject ではなく無応答**の相手だと
	 * 飛行中のブロックが残り続け、`loading` が降りず「再読み込み」が押せなく
	 * なるため。
	 */
	async function fetchBlock(request: BlockRequest): Promise<AuditBlockOutcome> {
		const params = {
			pagination: { offset: request.offset, limit: request.limit },
			sort: query.sort,
			filters: query.filters
		};
		const outcome = await runWithLimit(
			(signal) => listAuditLog(params, request.asOfId, signal),
			AUDIT_LIST_TIMEOUT_MS
		);
		if (outcome.kind === 'failed') return { kind: 'error', message: errorMessage(outcome.error) };
		if (outcome.kind === 'timedOut') {
			return {
				kind: 'error',
				message: `監査ログの読み取りが${Math.round(AUDIT_LIST_TIMEOUT_MS / 1000)}秒以内に返りませんでした。待つのをやめただけなので、「再読み込み」でもう一度試せます。`
			};
		}
		return { kind: 'ready', list: outcome.value };
	}

	const loader = new AuditBlockLoader(fetchBlock, {
		resetRows(length) {
			// 世代の最初の応答 / 問い合わせの変更。前の行は残さない。
			rows = new Array<AuditLogEntry | undefined>(length);
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
	// ので、これが無いと自分の書き込みで再実行し続ける（初回に 1 度だけ読む。
	// `onMount` と同じ意図だが、effect なのでクライアントでだけ走る）。
	$effect(() => {
		untrack(() => loader.setRange(0, 100));
	});

	/**
	 * 並べ替え・絞り込みの変更 = **新しい問い合わせ**。前の問い合わせの行・
	 * 件数・失敗は持ち越さない（`restart`）。表示範囲が `{0, 0}`（0 件の後）でも
	 * 先頭ブロックを取る（`blocksToFetch` - 以前はここで要求が 1 本も出ず、
	 * 絞り込みを外しても 0 件のままだった）。
	 */
	function handleParamsChange(params: { sort: SortState[]; filters: FilterState[] }): void {
		query = { sort: params.sort, filters: params.filters };
		loader.restart();
	}

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		loader.setRange(range.start, range.end);
	}

	/**
	 * 手で押す「再読み込み」= 新しい世代（同じ問い合わせのまま、新しい記録も
	 * ここで入る）。取り直す対象（失敗したブロック / 総件数が未取得なら先頭
	 * ブロック）は `blocksToFetch` が決める。
	 */
	function reload(): void {
		loader.reload();
	}

	function auditRowClass(row: AuditLogEntry): string | undefined {
		return row.result === 'denied' || row.result === 'failed' ? 'audit-row-alert' : undefined;
	}

	let selected: AuditLogEntry | null = $state(null);

	function selectRow(row: AuditLogEntry): void {
		selected = row;
	}

	const selectedDetail = $derived.by((): string | null => {
		if (!selected?.detail) return null;
		try {
			return JSON.stringify(JSON.parse(selected.detail), null, 2);
		} catch {
			return selected.detail;
		}
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>監査ログ</h2>
	</div>

	<!--
		「まだ読めていない」「読めなかった」「0 件」を別々に出す（#428）。
		件数が `null` のうちは件数を言わない（0 件と言い切らない）。
	-->
	<p class="note">
		{#if view.totalCount === null}
			{view.failedBlockCount > 0
				? '監査ログを読み込めていません。'
				: '監査ログを読み込んでいます。'}
		{:else}
			{view.totalCount.toLocaleString()}件の記録があります。行をクリックすると下に詳細が表示されます。
		{/if}
	</p>

	{#if view.errorText}
		<p class="error">{view.errorText}</p>
	{/if}
	{#if view.expired}
		<p class="error">{AUDIT_SNAPSHOT_EXPIRED_MESSAGE}</p>
	{/if}

	<!--
		**「再読み込み」は常に出す**（#428。chronogazer の #410 と同じ理由）:
		失敗からの再試行だけでなく、**新しい記録を取り込む唯一の導線**でもある -
		一覧は世代の最初の応答で `asOfId`（スナップショット境界）を固定するので、
		新しい世代を始めない限り後から記録された行は入らない。失敗の表示は
		ブロック単位で、そのブロックが読めるまで残る。自動ポーリングは足さない。
	-->
	<div class="actions">
		<button type="button" onclick={reload} disabled={view.loading}>再読み込み</button>
	</div>

	<section class="grid-wrap">
		<BantoGrid
			mode="server"
			state={gridState}
			{rows}
			totalRows={view.totalCount ?? 0}
			{columns}
			getRowId={(row) => row.id}
			rowClass={auditRowClass}
			onRowClick={selectRow}
			onParamsChange={handleParamsChange}
			onVisibleRangeChange={handleVisibleRangeChange}
		/>
	</section>

	{#if selected}
		<section class="detail">
			<h3>詳細（ID: {selected.id}）</h3>
			<dl>
				<dt>時刻</dt>
				<dd>{selected.ts}</dd>
				<dt>ユーザー</dt>
				<dd>{selected.actorUsername ?? '-'}</dd>
				<dt>ロール</dt>
				<dd>{selected.actorRole ?? '-'}</dd>
				<dt>アクション</dt>
				<dd>{actionLabel(selected.action)}</dd>
				<dt>リソース</dt>
				<dd>{selected.resource}</dd>
				<dt>対象ID</dt>
				<dd>{selected.entityId ?? '-'}</dd>
				<dt>経路</dt>
				<dd>{originLabel(selected.origin)}</dd>
				<dt>結果</dt>
				<dd class:alert={selected.result === 'denied' || selected.result === 'failed'}>
					{resultLabel(selected.result)}
				</dd>
			</dl>
			{#if selectedDetail}
				<h4>詳細情報（JSON）</h4>
				<pre>{selectedDetail}</pre>
			{/if}
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

	:global(.row.audit-row-alert) {
		border-left: 3px solid var(--banto-danger);
	}

	.detail {
		flex: 0 0 auto;
		max-height: 40%;
		overflow-y: auto;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	.detail h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}

	.detail h4 {
		margin: 0.75rem 0 0.5rem;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	dl {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0;
		font-size: 0.85rem;
	}

	dt {
		color: var(--banto-text-muted);
	}

	dd {
		margin: 0;
	}

	dd.alert {
		color: var(--banto-danger);
		font-weight: 600;
	}

	pre {
		margin: 0;
		padding: 0.75rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		font-size: 0.8rem;
		white-space: pre-wrap;
		word-break: break-word;
	}
</style>
