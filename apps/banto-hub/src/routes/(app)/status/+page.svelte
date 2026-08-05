<script lang="ts">
	/**
	 * 接続状態モニタ画面（実装指示のスコープ主軸機能、新規作成）。
	 * `GET /api/v1/status`（connections/revision/last_config_error）と
	 * `GET /api/v1/values`（全タグ現在値）を3秒ポーリングで表示する。
	 *
	 * ポーリングでよい理由（設計 §5.1）: 読み取りは
	 * `CollectorManager::current_values` が保持するオンメモリの現在値
	 * スナップショットを読むだけで、PLC への追加ポーリング要求は一切
	 * 発生しない（設計 §4: 「/api/v1/values* は current_values を読むだけで
	 * 完結し、PLC への追加要求を発生させない」）。つまりこの画面が
	 * リロードする頻度を上げても実機の負荷は増えないので、WebSocket/SSE
	 * 差分配信を新設するより単純な定期ポーリングで十分（WebSocket は
	 * 実装指示でも明示的にスコープ外）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import {
		getHubStatus,
		getHubValues,
		type ConnectionStatusEntry,
		type StatusResponse,
		type ValueEntry,
		type ValuesResponse
	} from '$lib/banto/hubStatus';

	const POLL_INTERVAL_MS = 3000;

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const statusLabels: Record<string, string> = {
		connected: '接続中',
		reconnecting: '再接続中',
		stopped: '停止中'
	};

	function statusLabel(status: string): string {
		return statusLabels[status] ?? status;
	}

	function statusClass(status: string): string {
		if (status === 'connected') return 'ok';
		if (status === 'reconnecting') return 'warn';
		return 'bad';
	}

	const qualityLabels: Record<string, string> = {
		good: '良好',
		bad: '不良',
		stale: '陳腐化'
	};

	function qualityLabel(q: string): string {
		return qualityLabels[q] ?? q;
	}

	/** 品質での色分け（実装指示: good=通常, bad=danger, stale=muted）。 */
	function qualityClass(q: string): string {
		if (q === 'bad') return 'bad';
		if (q === 'stale') return 'stale';
		return 'good';
	}

	function formatTime(epochMs: number): string {
		return new Date(epochMs).toLocaleString('ja-JP');
	}

	function formatValue(entry: ValueEntry): string {
		return entry.v === null ? '-' : String(entry.v);
	}

	let status: StatusResponse | null = $state(null);
	let values: ValuesResponse | null = $state(null);
	let loading = $state(true);
	let lastErrorShownAt = 0;

	// 連続失敗（サーバー停止中など）でトーストが3秒毎に積み上がらないよう、
	// 直近のエラー表示から一定時間は再表示を抑制する。
	const ERROR_TOAST_THROTTLE_MS = 15000;

	async function poll(): Promise<void> {
		try {
			const [nextStatus, nextValues] = await Promise.all([getHubStatus(), getHubValues()]);
			status = nextStatus;
			values = nextValues;
		} catch (err) {
			const now = Date.now();
			if (now - lastErrorShownAt > ERROR_TOAST_THROTTLE_MS) {
				lastErrorShownAt = now;
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void poll();
		const timer = setInterval(() => void poll(), POLL_INTERVAL_MS);
		return () => clearInterval(timer);
	});

	function connectionRowClass(conn: ConnectionStatusEntry): string {
		return `status-${statusClass(conn.status)}`;
	}
</script>

<div class="page">
	<section>
		<h2>サーバー状態</h2>
		{#if loading && !status}
			<p class="note">読み込み中…</p>
		{:else if status}
			<dl class="summary">
				<dt>バージョン</dt>
				<dd>{status.version}</dd>
				<dt>リビジョン</dt>
				<dd>{status.revision}</dd>
			</dl>
			{#if status.last_config_error}
				<p class="config-error">設定エラー: {status.last_config_error}</p>
			{/if}

			<h3>接続一覧</h3>
			{#if status.connections.length === 0}
				<p class="note">登録されているPLC接続がありません。</p>
			{:else}
				<table class="conn-table">
					<thead>
						<tr>
							<th>名前</th>
							<th>状態</th>
							<th>再試行回数</th>
						</tr>
					</thead>
					<tbody>
						{#each status.connections as conn (conn.id)}
							<tr class={connectionRowClass(conn)}>
								<td>{conn.name}</td>
								<td>{statusLabel(conn.status)}</td>
								<td>{conn.attempt ?? '-'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		{/if}
	</section>

	<section>
		<h2>タグ現在値</h2>
		{#if loading && !values}
			<p class="note">読み込み中…</p>
		{:else if values}
			<p class="note">
				{values.values.length}件のタグ ・ 更新時刻: {formatTime(values.t)}
			</p>
			{#if values.values.length === 0}
				<p class="note">登録されているタグがありません。</p>
			{:else}
				<div class="table-wrap">
					<table class="values-table">
						<thead>
							<tr>
								<th>外部名</th>
								<th>値</th>
								<th>品質</th>
								<th>時刻</th>
							</tr>
						</thead>
						<tbody>
							{#each values.values as entry (entry.tag)}
								<tr>
									<td class="tag-name">{entry.tag}</td>
									<td class="value quality-{qualityClass(entry.q)}">{formatValue(entry)}</td>
									<td class="quality quality-{qualityClass(entry.q)}">{qualityLabel(entry.q)}</td>
									<td>{formatTime(entry.t)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		{/if}
	</section>
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	h3 {
		margin: 1rem 0 0.5rem;
		font-size: 0.95rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.summary {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0;
		font-size: 0.875rem;
	}

	.summary dt {
		color: var(--banto-text-muted);
	}

	.summary dd {
		margin: 0;
	}

	.config-error {
		margin: 0.75rem 0 0;
		padding: 0.5rem 0.7rem;
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		color: var(--banto-text-muted);
		font-weight: 600;
		border-bottom: 1px solid var(--banto-border);
	}

	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
	}

	tr.status-ok td {
		color: var(--banto-text);
	}

	tr.status-warn td {
		color: var(--banto-danger);
	}

	tr.status-bad td {
		color: var(--banto-text-muted);
	}

	.table-wrap {
		max-height: 480px;
		overflow-y: auto;
	}

	.tag-name {
		font-family: var(--banto-font-mono, monospace);
	}

	.quality-good {
		color: var(--banto-text);
	}

	.quality-bad {
		color: var(--banto-danger);
		font-weight: 600;
	}

	.quality-stale {
		color: var(--banto-text-muted);
	}
</style>
