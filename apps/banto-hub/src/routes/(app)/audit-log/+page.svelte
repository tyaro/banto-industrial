<script lang="ts">
	/**
	 * 監査ログ閲覧画面。`admin` のみ到達（+page.ts が非adminをリダイレクト）。
	 * banto の admin-template の同名画面（`apps/admin-template/src/routes/(app)/
	 * audit-log/+page.svelte`、v4.0.0）の形に合わせ、以下だけを banto-hub 向けに
	 * 変えている:
	 * - 文言は日本語の直書き（banto-hub は paraglide を持たない）。件数の表示は
	 *   0 件でも「0件の記録があります。」（E2E が固定している）。
	 * - デモモード分岐（`isAuditLogAvailable()`）は無い - banto-hub にはデモ
	 *   モードが存在しない。
	 * - 保持ポリシーの表示（`getAuditConfig()`）は無い - 表示・変更の UI が
	 *   banto-hub にはまだ無い（`auditLogAdmin.ts` の doc comment 参照）。
	 * - アクションの表示名に `revoke`（API キーの失効）がある。経路は REST だけ。
	 *
	 * 一覧は BantoGrid の「サーバーモード」: ソート/フィルタ/ページングは
	 * すべて `listAuditLog()`（banto の `audit_log_router` -> SQL）が行い、
	 * ブロック単位でスクロールに応じて遅延取得する。
	 *
	 * ブロック読み込みは `@banto/admin-core` の `createSnapshotListResource`
	 * （banto #248）に任せる。監査ログは通知（`invalidate`）なしに行が増え、
	 * 保持期間の削除で減るので、世代の最初の応答で境界（`asOfId`）を固定し、
	 * 後続のブロックに渡す（ブロックの合間の追加で重複・欠落しない）。境界の
	 * 中で件数か `deletionEpoch` が変わったら失効として続きを読まず、
	 * 「再読み込み」で新しい世代にする。以前は同じ判断を自前で持っていた
	 * （`auditBlocks.ts` と chronogazer の複製の `#lib/blockCache.ts`、#428）。
	 */
	import { untrack } from 'svelte';
	import {
		BantoGrid,
		GridState,
		type FilterState,
		type GridColumn,
		type SortState
	} from '@banto/grid-svelte';
	import { createSnapshotListResource } from '@banto/admin-core';
	import {
		AUDIT_SNAPSHOT_EXPIRED_MESSAGE,
		auditErrorText,
		createAuditLogFetcher,
		type AuditLogEntry
	} from '#lib/banto/auditLogAdmin.js';

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
	 * 監査ログのブロック読み込み（banto #248）。取得 1 本は
	 * `createAuditLogFetcher()`（`listAuditLog(params, asOfId, signal)` に
	 * 15 秒の上限を掛けたもの）: 世代の最初は `asOfId: null`（サーバーが境界を
	 * 決めて返す）、後続は固定した境界。境界付きの取得ではサーバーは保持期間の
	 * 削除を走らせない。失敗の持ち方・世代違いの応答の破棄・新しい世代での
	 * 中断はリソース側が行う。
	 */
	const auditLog = createSnapshotListResource<AuditLogEntry>(createAuditLogFetcher(), {
		params: { sort: gridState.sort, filters: [] }
	});

	// `untrack`: 初回の読み込みはマウント時に 1 度だけ。リソースは引数の
	// 非リアクティブな写しだけを読むが、`ensureRange()` が公開する状態
	// （`loading` など）に effect が依存しないようにする。cleanup でページを
	// 離れたときに飛行中の要求を中断する。
	$effect(() => {
		untrack(() => auditLog.ensureRange(0, 100));
		return () => auditLog.dispose();
	});

	// 並べ替え・絞り込みの変更 = 新しい問い合わせ（前の行・件数・失敗は
	// 持ち越さない）。表示範囲が `{0, 0}`（0 件の後）でも先頭ブロックを取る。
	function handleParamsChange(params: { sort: SortState[]; filters: FilterState[] }): void {
		auditLog.setParams(params);
	}

	function handleVisibleRangeChange(range: { start: number; end: number }): void {
		auditLog.ensureRange(range.start, range.end);
	}

	// 「再読み込み」= 新しい世代（同じ問い合わせのまま新しい記録も入る）。
	// 失敗したブロックも取り直す。処理中でも押せる: 処理中の要求は中断され、
	// 新しい世代の要求に置き換わる（応答しない要求のせいで回復の手段が
	// 使えなくならない）。
	function reload(): void {
		auditLog.refresh();
	}

	// result='denied'/'failed' の行を控えめな左ボーダーで区別し、選択中の行を
	// 強調する（下の詳細パネルとの対応が分かるように。admin-template と同じ）。
	function auditRowClass(row: AuditLogEntry): string | undefined {
		const classes: string[] = [];
		if (row.result === 'denied' || row.result === 'failed') classes.push('audit-row-alert');
		if (selected?.id === row.id) classes.push('audit-row-selected');
		return classes.length > 0 ? classes.join(' ') : undefined;
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
		「まだ読めていない」「読めなかった」「0 件」を別々に出す（#428、banto #248）。
		件数が `null` のうちは件数を言わない（0 件と言い切らない）。
	-->
	<p class="note">
		{#if auditLog.totalCount === null}
			{auditLog.failedBlocks.length > 0
				? '監査ログを読み込めていません。'
				: '監査ログを読み込んでいます。'}
		{:else}
			{auditLog.totalCount.toLocaleString()}件の記録があります。行をクリックすると下に詳細が表示されます。
		{/if}
	</p>

	{#if auditLog.error}
		<p class="error" role="alert">{auditErrorText(auditLog.error)}</p>
	{/if}
	{#if auditLog.expired}
		<p class="error" role="alert">{AUDIT_SNAPSHOT_EXPIRED_MESSAGE}</p>
	{/if}

	<!--
		**「再読み込み」は常に出す**（#428）: 失敗からの再試行だけでなく、
		**新しい記録を取り込む唯一の導線**でもある - 一覧は世代の最初の応答で
		`asOfId`（スナップショット境界）を固定するので、新しい世代を始めない限り
		後から記録された行は入らない。処理中でも押せる（banto #248）。自動
		ポーリングは足さない。
	-->
	<div class="actions">
		<button type="button" onclick={reload}>再読み込み</button>
	</div>

	<section class="grid-wrap">
		<BantoGrid
			mode="server"
			state={gridState}
			rows={auditLog.rows}
			totalRows={auditLog.totalCount ?? 0}
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

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}

	:global(.row.audit-row-alert) {
		border-left: 3px solid var(--banto-danger);
	}

	:global(.row.audit-row-selected) {
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
		border-left: 3px solid var(--banto-primary);
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
