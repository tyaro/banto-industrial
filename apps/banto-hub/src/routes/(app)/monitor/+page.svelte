<script lang="ts">
	/**
	 * ライブタグモニタ画面（T10、docs/ux-plan.md §2、2026-08-06 オーナー
	 * 承認）。既存の WS 購読（`/api/v1/stream`）を消費して、登録済み全タグの
	 * 現在値・品質・時刻を一覧表示する読み取り専用画面。
	 *
	 * relay-wright の `(app)/monitor/+page.svelte`（接続→収集グループの
	 * ツリー + インライン書き込み）とは構造が異なる: あちらのツリーは
	 * 「エンジンの PLC セッションを共有する（実機は SLMP 同時接続を1本しか
	 * 受けない）」という制約から来ていたが、この画面は WS 経由で
	 * `CollectorManager` の現在値スナップショットを読むだけで、独自の PLC
	 * セッションを持たないためその制約が存在しない。よってツリーではなく
	 * フラットな一覧 + 接続/グループのプルダウンフィルタにした。書き込み
	 * UI も一切持たない（ux-plan.md §2「書き込み操作は付けない」）。
	 *
	 * 権限: relay-wright のモニタは書き込みセルを editor 以上に限定するが、
	 * この画面には書き込み要素が無いため viewer を含む全ロールが無条件で
	 * 閲覧できる（ゲートすべき対象が無い）。
	 */
	import { toastStore } from '$lib/toast.svelte';
	import {
		getCatalog,
		connectTagStream,
		type CatalogTagEntry,
		type StreamValue
	} from '$lib/banto/tagMonitorAdmin';
	import { isProviderError } from '@banto/admin-core';

	/** catalog の1タグ + WS から届く最新の現在値。 */
	interface Row extends CatalogTagEntry {
		v: number | null;
		q: string;
		t: number;
	}

	const FLASH_MS = 700;

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let rows = $state<Row[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let wsConnected = $state(false);

	/** 直近で値/品質が変化した外部名（一時的なハイライト表示用）。 */
	let flashing = $state<Record<string, boolean>>({});

	function triggerFlash(externalName: string): void {
		flashing[externalName] = true;
		setTimeout(() => {
			delete flashing[externalName];
		}, FLASH_MS);
	}

	function toRow(entry: CatalogTagEntry, previous?: Row): Row {
		return {
			...entry,
			v: previous?.v ?? null,
			q: previous?.q ?? 'stale',
			t: previous?.t ?? 0
		};
	}

	/** catalog を（再）取得し、既存の生値（v/q/t）を維持しつつ行を作り直す。
	 * 新規タグは追加され、消えたタグは自然に脱落する（`catalog.tags` を
	 * そのまま元に組み立てるため）。 */
	async function reloadCatalog(): Promise<void> {
		try {
			const catalog = await getCatalog();
			const previousByName = new Map(rows.map((r) => [r.external_name, r]));
			rows = catalog.tags.map((entry) => toRow(entry, previousByName.get(entry.external_name)));
			loadError = null;
		} catch (err) {
			loadError = errorMessage(err);
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	function applyStreamData(values: StreamValue[]): void {
		const byTag = new Map(values.map((v) => [v.tag, v]));
		if (byTag.size === 0) return;
		rows = rows.map((row) => {
			const update = byTag.get(row.external_name);
			if (!update) return row;
			triggerFlash(row.external_name);
			return { ...row, v: update.v, q: update.q, t: update.t };
		});
	}

	$effect(() => {
		void reloadCatalog();

		const disconnect = connectTagStream({
			onData: applyStreamData,
			onConfigChanged: () => void reloadCatalog(),
			onStatusChange: (connected) => {
				wsConnected = connected;
			}
		});

		return () => {
			disconnect();
		};
	});

	// --- フィルタ（接続・グループ、クライアント側のみ・再取得なし） ---------
	let connectionFilter = $state('');
	let groupFilter = $state('');

	const connectionOptions = $derived(
		Array.from(new Set(rows.map((r) => r.connection))).sort((a, b) => a.localeCompare(b))
	);
	const groupOptions = $derived(
		Array.from(new Set(rows.map((r) => r.group))).sort((a, b) => a.localeCompare(b))
	);

	const filteredRows = $derived(
		rows.filter(
			(r) =>
				(connectionFilter === '' || r.connection === connectionFilter) &&
				(groupFilter === '' || r.group === groupFilter)
		)
	);

	const qualityLabels: Record<string, string> = {
		good: '良好',
		bad: '不良',
		stale: '陳腐化'
	};

	function qualityLabel(q: string): string {
		return qualityLabels[q] ?? q;
	}

	/** status/+page.svelte と同じ規約: good=通常, bad=danger, stale=muted。 */
	function qualityClass(q: string): string {
		if (q === 'bad') return 'bad';
		if (q === 'stale') return 'stale';
		return 'good';
	}

	function formatValue(row: Row): string {
		if (row.q !== 'good' || row.v === null) return '--';
		return row.unit ? `${row.v} ${row.unit}` : String(row.v);
	}

	function formatTime(epochMs: number): string {
		if (!epochMs) return '--';
		return new Date(epochMs).toLocaleString('ja-JP');
	}
</script>

<div class="page">
	<section>
		<h2>タグモニタ</h2>
		<p class="note">
			登録済みタグの現在値をリアルタイム表示します（WebSocket購読、書き込み機能はありません）。
		</p>
		<p class="note status-line">
			<span class="ws-dot" class:on={wsConnected} class:off={!wsConnected}></span>
			{wsConnected
				? '接続中（リアルタイム更新中）'
				: '再接続中…（値は最後の受信内容のまま停止しています）'}
		</p>

		{#if loadError}
			<p class="error-text">{loadError}</p>
		{/if}

		<div class="filters">
			<label class="field">
				接続
				<select bind:value={connectionFilter}>
					<option value="">すべて</option>
					{#each connectionOptions as name (name)}
						<option value={name}>{name}</option>
					{/each}
				</select>
			</label>
			<label class="field">
				グループ
				<select bind:value={groupFilter}>
					<option value="">すべて</option>
					{#each groupOptions as name (name)}
						<option value={name}>{name}</option>
					{/each}
				</select>
			</label>
			<span class="muted small count">{filteredRows.length} / {rows.length} 件</span>
		</div>

		{#if loading && rows.length === 0}
			<p class="note">読み込み中…</p>
		{:else if rows.length === 0}
			<!--
				T18-2d（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「空状態を…
				不足する前工程と移動ボタンを示す」）: タグが1件も無い（フィルタの
				問題ではなく真の空）場合は、前工程（タグ登録）へ案内する。
			-->
			<p class="note">
				登録されているタグがありません。先に タグの登録画面 からタグを作成してください。
			</p>
			<a class="onboarding-cta" href="/tags">タグの登録画面へ移動</a>
		{:else if filteredRows.length === 0}
			<p class="note">条件に一致するタグがありません。</p>
		{:else}
			<div class="table-wrap">
				<table class="values-table">
					<thead>
						<tr>
							<th>外部名</th>
							<th>接続</th>
							<th>グループ</th>
							<th>値</th>
							<th>品質</th>
							<th>時刻</th>
						</tr>
					</thead>
					<tbody>
						{#each filteredRows as row (row.external_name)}
							<tr class:flash={flashing[row.external_name]}>
								<td class="tag-name">{row.external_name}</td>
								<td>
									{row.connection}
									{#if row.simulation}
										<span class="sim-badge" title="シミュレーション接続（実機ではありません）"
											>⚠ SIM</span
										>
									{/if}
								</td>
								<td>{row.group}</td>
								<td class="value quality-{qualityClass(row.q)}">{formatValue(row)}</td>
								<td class="quality quality-{qualityClass(row.q)}">{qualityLabel(row.q)}</td>
								<td>{formatTime(row.t)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
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

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.muted {
		color: var(--banto-text-muted);
	}

	.small {
		font-size: 0.75rem;
	}

	.error-text {
		color: var(--banto-danger);
		font-size: 0.8rem;
		margin: 0 0 0.5rem;
	}

	/* T18-2d（TAG-UX-A）: 前工程（タグ登録）への移動リンク。 */
	.onboarding-cta {
		display: inline-block;
		padding: 0.3rem 0.75rem;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		font-size: 0.8rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.onboarding-cta:hover {
		background: var(--banto-primary-hover);
	}

	.status-line {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.ws-dot {
		display: inline-block;
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 50%;
		background: var(--banto-text-muted);
	}

	.ws-dot.on {
		background: var(--banto-primary);
	}

	.ws-dot.off {
		background: var(--banto-danger);
	}

	.filters {
		display: flex;
		align-items: flex-end;
		gap: 1rem;
		margin-bottom: 0.75rem;
		flex-wrap: wrap;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.field select {
		padding: 0.35rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		min-width: 10rem;
	}

	.count {
		margin-left: auto;
		align-self: center;
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

	tbody tr {
		transition: background 0.6s ease-out;
	}

	.table-wrap {
		max-height: 560px;
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

	/* T9 連携: シミュレーション接続配下のタグに付けるバッジ。plc-connections
	   の一覧バッジと同じ --banto-warning を使うが、こちらはフラットな表の
	   1セル内なので rowClass ではなく単純な span で足りる。 */
	.sim-badge {
		display: inline-block;
		margin-left: 0.35rem;
		padding: 0.05rem 0.35rem;
		border-radius: var(--banto-radius);
		border: 1px solid var(--banto-warning);
		color: var(--banto-warning);
		font-size: 0.65rem;
		font-weight: 700;
	}

	/* WS の data で値/品質が変化した行を一瞬ハイライトする（アニメーション
	   ライブラリは使わず、JS 側で .flash を付けて setTimeout で外すだけ -
	   tagMonitorAdmin.ts の呼び出し元 `triggerFlash` 参照）。 */
	tr.flash {
		background: color-mix(in srgb, var(--banto-primary) 16%, transparent);
		transition: background 0.6s ease-out;
	}
</style>
