<script lang="ts">
	/**
	 * 書き込み監査ログ閲覧画面（plan `luminous-discovering-goblet.md` W4）。
	 * エンジンが記録した `write_audit_log`（ルール発火・アーム/ディスアーム・
	 * ドライラン切替・レート制限トリップ）を一覧表示する読み取り専用画面。
	 * viewer+ で到達可（+page.ts ガード無し）。
	 *
	 * 一覧は BantoGrid のクライアントモード（write-rules/write-targets と同じ）:
	 * `writeAuditLogAdmin.list()` が全行を取得し、フィルタ/ソートはグリッドが
	 * クライアント側で行う。理由は REST の `GET /api/write-audit-log` が
	 * ListParams を無視して全行を新しい順で返す設計（rest.rs の当該ルートの
	 * doc comment 参照 = 両経路対称の範囲でW2レジストリと同じGET-all方式）で
	 * あり、Tauri コマンド側の server-side ページングとは一つの画面で統一する
	 * ため、共通して全行取得＋クライアントグリッドに倒している。
	 *
	 * デモモード（バックエンド無し）では監査DBが無いため案内文のみ表示する。
	 *
	 * result 列は色分けする（安全設計の可視化）: 実際に書き込んだ ok と、抑止
	 * された suppressed_*（disarmed/dry_run/rate_limited）や failed /
	 * rate_limit_tripped を運用者が一目で区別できるようにする。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import {
		list,
		isWriteAuditLogAvailable,
		DEMO_MODE_MESSAGE,
		type WriteAuditLogRow
	} from '$lib/banto/writeAuditLogAdmin';

	const available = isWriteAuditLogAvailable();

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const actionLabels: Record<string, string> = {
		rule_fire: 'ルール発火',
		arm: 'アーム',
		disarm: 'ディスアーム',
		dry_run_toggle: 'ドライラン切替',
		rate_limit_tripped: 'レート制限トリップ',
		// feature/tag-monitor: モニタ画面からのワンショット手動書き込み。
		manual_write: '手動書き込み'
	};

	const resultLabels: Record<string, string> = {
		ok: '書き込み成功',
		failed: '失敗',
		suppressed_disarmed: '抑止（非アーム）',
		suppressed_rate_limited: '抑止（レート制限）',
		suppressed_dry_run: '抑止（ドライラン）',
		rate_limit_tripped: 'レート制限トリップ'
	};

	function actionLabel(action: string): string {
		return actionLabels[action] ?? action;
	}

	function resultLabel(result: string): string {
		return resultLabels[result] ?? result;
	}

	/**
	 * 結果を3系統に分類する（色分けの基準）:
	 * - 'ok'        : 実際に書き込んだ（--banto-success）
	 * - 'alert'     : 失敗・レート制限トリップ（--banto-danger）= 注意
	 * - 'suppressed': 意図的に抑止された suppressed_*（控えめ・muted）
	 */
	function resultKind(result: string): 'ok' | 'alert' | 'suppressed' {
		if (result === 'ok') return 'ok';
		if (result === 'failed' || result === 'rate_limit_tripped') return 'alert';
		return 'suppressed';
	}

	function numOrDash(value: number | null): string {
		return value === null ? '-' : String(value);
	}

	/** ソース列: タグID と 発火時のスナップショット値をまとめて表示。 */
	function sourceCell(row: WriteAuditLogRow): string {
		if (row.sourceTagId === null && row.sourceValueSnapshot === null) return '-';
		const id = row.sourceTagId === null ? '-' : `#${row.sourceTagId}`;
		return `${id} = ${numOrDash(row.sourceValueSnapshot)}`;
	}

	/** ターゲット列: 書き込み先ID と 実際に書き込んだ値。 */
	function targetCell(row: WriteAuditLogRow): string {
		if (row.writeTargetId === null && row.targetValueWritten === null) return '-';
		const id = row.writeTargetId === null ? '-' : `#${row.writeTargetId}`;
		return `${id} = ${numOrDash(row.targetValueWritten)}`;
	}

	let rows: WriteAuditLogRow[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			const result = await list({ sort: [], filters: [] });
			rows = result.rows;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	const columns: GridColumn<WriteAuditLogRow>[] = [
		{ id: 'ts', header: '時刻', accessor: 'ts', width: 170 },
		{
			id: 'action',
			header: 'アクション',
			accessor: 'action',
			width: 140,
			filterable: true,
			filterType: 'text',
			format: (v) => actionLabel(String(v))
		},
		{
			id: 'result',
			header: '結果',
			accessor: 'result',
			width: 160,
			filterable: true,
			filterType: 'text',
			format: (v) => resultLabel(String(v))
		},
		{
			id: 'ruleNameSnapshot',
			header: 'ルール',
			accessor: 'ruleNameSnapshot',
			width: 150,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'source',
			header: 'ソース（タグ=値）',
			accessor: (row) => sourceCell(row),
			width: 150
		},
		{
			id: 'target',
			header: '書き込み先（=値）',
			accessor: (row) => targetCell(row),
			width: 150
		},
		{
			id: 'actorUsername',
			header: '操作者',
			accessor: (row) => row.actorUsername ?? '-',
			width: 120,
			filterable: true,
			filterType: 'text'
		}
	];

	// result の系統でうっすら左ボーダーを色分けする（生色禁止・テーマ変数のみ、
	// 監査ログ画面の audit-row-alert と同じ :global() 方式）。
	function rowClass(row: WriteAuditLogRow): string {
		return `wal-${resultKind(row.result)}`;
	}

	let selected: WriteAuditLogRow | null = $state(null);

	function selectRow(row: WriteAuditLogRow): void {
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
		<h2>書き込み監査ログ</h2>
	</div>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードには監査ログDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<p class="note">
			{rows.length.toLocaleString()}件の記録があります。行をクリックすると下に詳細が表示されます。実際に書き込んだ行（緑）と抑止された行（灰）・失敗/レート制限（赤）を左端の色で見分けられます。
		</p>

		{#if loading && rows.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<section class="grid-wrap">
				<BantoGrid {rows} {columns} getRowId={(row) => row.id} {rowClass} onRowClick={selectRow} />
			</section>
		{/if}

		{#if selected}
			<section class="detail">
				<h3>詳細（ID: {selected.id}）</h3>
				<dl>
					<dt>時刻</dt>
					<dd>{selected.ts}</dd>
					<dt>アクション</dt>
					<dd>{actionLabel(selected.action)}</dd>
					<dt>結果</dt>
					<dd class={`result-${resultKind(selected.result)}`}>{resultLabel(selected.result)}</dd>
					<dt>ルール</dt>
					<dd>
						{selected.ruleNameSnapshot}{selected.writeRuleId !== null
							? `（#${selected.writeRuleId}）`
							: ''}
					</dd>
					<dt>ソース</dt>
					<dd>{sourceCell(selected)}</dd>
					<dt>書き込み先</dt>
					<dd>{targetCell(selected)}</dd>
					<dt>操作者</dt>
					<dd>{selected.actorUsername ?? '-'}</dd>
				</dl>
				{#if selectedDetail}
					<h4>詳細情報（JSON）</h4>
					<pre>{selectedDetail}</pre>
				{/if}
			</section>
		{/if}
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

	.loading {
		color: var(--banto-text-muted);
	}

	.grid-wrap {
		flex: 1;
		min-height: 0;
	}

	/* BantoGrid の rowClass が付与するクラスに対する :global() 色分け
	   （audit-log の audit-row-alert と同じ手法）。生色は使わずテーマ変数のみ。 */
	:global(.row.wal-ok) {
		border-left: 3px solid var(--banto-success);
	}

	:global(.row.wal-alert) {
		border-left: 3px solid var(--banto-danger);
	}

	:global(.row.wal-suppressed) {
		border-left: 3px solid var(--banto-text-muted);
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

	dd.result-ok {
		color: var(--banto-success);
		font-weight: 600;
	}

	dd.result-alert {
		color: var(--banto-danger);
		font-weight: 600;
	}

	dd.result-suppressed {
		color: var(--banto-text-muted);
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
