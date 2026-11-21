<script lang="ts">
	/**
	 * 下のログのパネル（#1253。docs/hub/banto-hub-ui-improvement-proposal.md §下のログのパネル）。
	 * `/tags` のドックの枠（`ThreePaneDock` の追加パネル `log`）に置く部品。
	 *
	 * - 取得と保持はページが持つ `LogFeedState`（`#lib/banto/logFeedState.svelte.ts`）。この
	 *   部品は表示と操作（種類の絞り込み・一時停止・消去）だけで、隠すと作り直されてもよい
	 *   （絞り込みの選択だけは作り直すと既定に戻る）。
	 * - 種類のトグルは**届いた種類**に合わせて出す。管理者でない利用者には接続の行しか
	 *   来ない（画面で隠すのではなくサーバーが絞っている）。
	 * - 表は軽い `<table>`（保持の上限が 1,000 行なので仮想化は要らない。タグモニタの表と
	 *   同じ流儀）。新しい行が上。書き込みの失敗・不確定・拒否、操作の拒否・失敗、接続の
	 *   切断は `alert` として目立たせる。
	 */
	import { LOG_SOURCE_LABELS, logRowView, type LogSource } from '#lib/banto/logFeed.js';
	import type { LogFeedState } from '#lib/banto/logFeedState.svelte.js';

	interface Props {
		feed: LogFeedState;
	}

	let { feed }: Props = $props();

	/** 絞り込みで外した種類（既定は全部出す）。 */
	let hiddenSources = $state<LogSource[]>([]);

	const views = $derived(
		feed.rows.filter((row) => !hiddenSources.includes(row.source)).map((row) => logRowView(row))
	);

	function toggleSource(source: LogSource): void {
		hiddenSources = hiddenSources.includes(source)
			? hiddenSources.filter((s) => s !== source)
			: [...hiddenSources, source];
	}

	const statusText = $derived.by(() => {
		if (feed.status === 'retrying')
			return `読み込めません（${feed.error ?? '不明'}）。間隔を空けて再試行します`;
		if (feed.status === 'unauthorized') return 'セッションが切れたため、読み込みを止めました';
		return '';
	});
</script>

<div class="log-panel" data-testid="log-panel">
	<div class="log-toolbar">
		<div class="log-filters" role="group" aria-label="種類で絞り込む">
			{#each feed.sources as source (source)}
				<button
					type="button"
					class="tool-button"
					aria-pressed={!hiddenSources.includes(source)}
					data-testid="log-filter-{source}"
					onclick={() => toggleSource(source)}>{LOG_SOURCE_LABELS[source]}</button
				>
			{/each}
		</div>
		<span class="log-spacer"></span>
		{#if statusText}
			<span class="log-status" role="status" data-testid="log-status">{statusText}</span>
		{/if}
		{#if feed.paused}
			<span class="log-pending" data-testid="log-pending">新しい行 {feed.pendingCount} 件</span>
			<button
				type="button"
				class="tool-button"
				data-testid="log-resume"
				onclick={() => feed.resume()}>再開</button
			>
		{:else}
			<button type="button" class="tool-button" data-testid="log-pause" onclick={() => feed.pause()}
				>一時停止</button
			>
		{/if}
		<button
			type="button"
			class="tool-button"
			data-testid="log-clear"
			title="表示中の行を消します（記録は消しません）"
			onclick={() => feed.clear()}>消去</button
		>
		<span class="log-count">{views.length} 件</span>
	</div>
	<div class="log-table-wrap">
		{#if views.length === 0}
			<p class="log-empty">表示する記録はありません。</p>
		{:else}
			<table class="log-table">
				<thead>
					<tr>
						<th>時刻</th>
						<th>種類</th>
						<th>対象</th>
						<th>結果</th>
						<th>誰</th>
						<th>説明</th>
					</tr>
				</thead>
				<tbody>
					{#each views as row (row.key)}
						<tr
							class:alert={row.tone === 'alert'}
							data-testid="log-row"
							data-source={row.source}
							data-key={row.key}
						>
							<td class="nowrap">{row.time}</td>
							<td class="nowrap">{row.sourceLabel}</td>
							<td class="target">{row.target}</td>
							<td class="nowrap result">{row.result}</td>
							<td class="nowrap">{row.who}</td>
							<td class="description" title={row.description}>{row.description}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</div>
</div>

<style>
	.log-panel {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		gap: 0.4rem;
		padding: 0.5rem 0.75rem;
		font-size: 0.8rem;
	}
	.log-toolbar {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}
	.log-filters {
		display: flex;
		gap: 0.3rem;
	}
	.log-spacer {
		flex: 1;
	}
	.tool-button {
		padding: 0.2rem 0.55rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		color: var(--banto-text);
		font: inherit;
		cursor: pointer;
	}
	.tool-button[aria-pressed='true'] {
		background: var(--banto-bg);
		border-color: var(--banto-primary);
	}
	.tool-button[aria-pressed='false'] {
		color: var(--banto-text-muted);
	}
	.log-status {
		color: var(--banto-danger);
	}
	.log-pending,
	.log-count {
		color: var(--banto-text-muted);
	}
	.log-table-wrap {
		flex: 1;
		min-height: 0;
		overflow: auto;
	}
	.log-empty {
		margin: 0.25rem 0;
		color: var(--banto-text-muted);
	}
	.log-table {
		width: 100%;
		border-collapse: collapse;
	}
	.log-table th {
		position: sticky;
		top: 0;
		background: var(--banto-surface);
		text-align: left;
		font-weight: 600;
		color: var(--banto-text-muted);
		border-bottom: 1px solid var(--banto-border);
		padding: 0.2rem 0.4rem;
	}
	.log-table td {
		padding: 0.15rem 0.4rem;
		border-bottom: 1px solid var(--banto-border);
		vertical-align: top;
	}
	.nowrap {
		white-space: nowrap;
	}
	.target {
		max-width: 16rem;
		overflow-wrap: anywhere;
	}
	.description {
		max-width: 40rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	tr.alert td {
		color: var(--banto-danger);
	}
	tr.alert .result {
		font-weight: 600;
	}
</style>
