<script module lang="ts">
	/** 窓の 1 行（ページが外部名・値の表示・強調を組み立てて渡す）。 */
	export interface WatchRowView {
		id: number;
		/** 完全な外部名（`{接続}.{グループ}.{タグ}`）。 */
		name: string;
		value: string;
		qualityClass: 'good' | 'bad' | 'stale' | 'unverified';
		qualityLabel: string;
		time: string;
		/** 値・品質が変わった直後（700ms の強調）。 */
		flashing: boolean;
	}
</script>

<script lang="ts">
	/**
	 * 値を見張る窓（#1254。docs/hub/banto-hub-ui-improvement-proposal.md §値を見張る窓）。
	 * `/tags` のドックの枠（`ThreePaneDock` の追加パネル `watch`）に置く部品。
	 *
	 * - 一覧・購読・保存はページが持つ（`watchList.ts`・`watchListStore.ts`・`createTagValueStream`）。
	 *   この部品は表示と操作（行ごとの「外す」・「すべて外す」）だけで、窓を隠して作り直されても
	 *   状態を失わない。
	 * - 表は軽い `<table>`（上限 100 タグ。`LogPanel` と同じ流儀）。名前は完全な外部名。
	 *   値・品質・時刻の表示はタグ一覧の値の列と同じ（ページが `monitorCellDisplay` を通した結果）。
	 * - 窓からの書き込みは無い（手動書き込みは段階 2）。
	 * - 一覧の保存の状態（`watchListStore.ts` の `status`、PR #1282 のレビュー・2026-11-22 の
	 *   作り直し）を出す: `saving` は「保存中」、`retrying` は「保存できていません。再試行します」と
	 *   「今すぐ再試行」、`blocked` は「保存できません（ログインし直してください）」。`loading`・
	 *   `saved` は出さない。保存が成功する前にページを離れた変更は失われるので、隠さず出す。
	 */
	import type { StreamStatusLine } from '#lib/banto/monitorStreamView.js';
	import type { WatchSaveStatus } from '#lib/watchListStore.js';

	interface Props {
		rows: WatchRowView[];
		/** 見張りの購読の状態（切断・止まっている間の表示）。 */
		status: StreamStatusLine;
		onRemove: (id: number) => void;
		onClear: () => void;
		/** close `1008` で止まった購読の再開。 */
		onResume: () => void;
		/** 一覧の保存の状態。 */
		saveStatus?: WatchSaveStatus;
		/** 「今すぐ再試行」（`retrying` のとき）。 */
		onRetrySave?: () => void;
	}

	let {
		rows,
		status,
		onRemove,
		onClear,
		onResume,
		saveStatus = 'saved',
		onRetrySave
	}: Props = $props();
</script>

<div class="watch-panel" data-testid="watch-panel">
	<div class="watch-toolbar">
		{#if rows.length > 0}
			<span
				class="watch-status"
				class:warning={status.warning}
				data-testid="watch-status"
				title={status.text}>{status.text}</span
			>
			{#if status.resumable}
				<button type="button" class="tool-button" onclick={onResume}>再接続</button>
			{/if}
		{/if}
		<span class="watch-spacer"></span>
		<span class="watch-count" data-testid="watch-count">{rows.length} 件</span>
		<button
			type="button"
			class="tool-button"
			data-testid="watch-clear"
			disabled={rows.length === 0}
			onclick={onClear}>すべて外す</button
		>
	</div>
	{#if saveStatus === 'saving'}
		<p class="watch-save" role="status" data-testid="watch-save-status">保存中…</p>
	{:else if saveStatus === 'retrying'}
		<p class="watch-save warning" role="status" data-testid="watch-save-status">
			保存できていません。再試行します。
			<button
				type="button"
				class="tool-button"
				data-testid="watch-save-retry"
				onclick={() => onRetrySave?.()}>今すぐ再試行</button
			>
		</p>
	{:else if saveStatus === 'blocked'}
		<p class="watch-save warning" role="status" data-testid="watch-save-status">
			保存できません（ログインし直してください）。
		</p>
	{/if}
	<div class="watch-table-wrap">
		{#if rows.length === 0}
			<p class="watch-empty">
				見張っているタグはありません。タグ一覧で行を選び、「見張りに足す」で足します。
			</p>
		{:else}
			<table class="watch-table">
				<thead>
					<tr>
						<th>名前</th>
						<th>現在値</th>
						<th>品質</th>
						<th>時刻</th>
						<th><span class="sr-only">操作</span></th>
					</tr>
				</thead>
				<tbody>
					{#each rows as row (row.id)}
						<tr class:flash={row.flashing} data-testid="watch-row" data-tag-id={row.id}>
							<td class="name">{row.name}</td>
							<td class="nowrap value quality-{row.qualityClass}" data-testid="watch-value"
								>{row.value}</td
							>
							<td class="nowrap quality-{row.qualityClass}" data-testid="watch-quality"
								>{row.qualityLabel}</td
							>
							<td class="nowrap">{row.time}</td>
							<td class="nowrap">
								<button
									type="button"
									class="tool-button"
									data-testid="watch-remove"
									aria-label="{row.name} を見張りから外す"
									onclick={() => onRemove(row.id)}>外す</button
								>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</div>
</div>

<style>
	.watch-panel {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		gap: 0.4rem;
		padding: 0.5rem 0.75rem;
		font-size: 0.8rem;
	}
	.watch-toolbar {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}
	.watch-spacer {
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
	.tool-button:disabled {
		color: var(--banto-text-muted);
		cursor: default;
	}
	.watch-status,
	.watch-count {
		color: var(--banto-text-muted);
	}
	.watch-status.warning {
		color: var(--banto-danger);
	}
	.watch-save {
		flex: 0 0 auto;
		margin: 0;
		color: var(--banto-text-muted);
	}
	.watch-save.warning {
		color: var(--banto-danger);
	}
	.watch-table-wrap {
		flex: 1;
		min-height: 0;
		overflow: auto;
	}
	.watch-empty {
		margin: 0.25rem 0;
		color: var(--banto-text-muted);
	}
	.watch-table {
		width: 100%;
		border-collapse: collapse;
	}
	.watch-table th {
		position: sticky;
		top: 0;
		background: var(--banto-surface);
		text-align: left;
		font-weight: 600;
		color: var(--banto-text-muted);
		border-bottom: 1px solid var(--banto-border);
		padding: 0.2rem 0.4rem;
	}
	.watch-table td {
		padding: 0.15rem 0.4rem;
		border-bottom: 1px solid var(--banto-border);
		vertical-align: middle;
	}
	.name {
		overflow-wrap: anywhere;
	}
	.nowrap {
		white-space: nowrap;
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
	.quality-unverified {
		color: var(--banto-text-muted);
		font-style: italic;
	}
	tr.flash td {
		background: color-mix(in srgb, var(--banto-primary) 22%, transparent);
	}
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0 0 0 0);
	}
</style>
