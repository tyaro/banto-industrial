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
	 * セッションを持たないためその制約が存在しない。書き込み UI も一切
	 * 持たない（ux-plan.md §2「書き込み操作は付けない」）。
	 *
	 * T18-4a（docs/banto-hub-t18-design.md「T18-4a モニタの Tree/検索統合」、
	 * T13-2 移管）: 当初のフラット一覧 + 接続/グループのプルダウン
	 * フィルタを、タグ登録ページ（`(app)/tags/+page.svelte`）と同じ
	 * SplitPane + ConnectionTree + 検索ボックスの Tree/検索 UI へ置換した -
	 * 登録と同じ操作感でモニタ対象を絞り込めるようにする、という目的の
	 * スライス。ツリー・絞り込みは表示専用（`ConnectionTree` の
	 * `oncontextmenu` は渡さない - このページに作成系 UI は無い）。ツリーへ
	 * 渡す `connections`/`groups`/`adminTags` は tagRegistryAdmin.ts の既存
	 * 一覧 API から読むだけの補助データで、値表示自体は従来どおり
	 * catalog（`rows`）+ WS が正。絞り込みロジックは依存ゼロの純関数
	 * `filterMonitorRows`（`$lib/banto/monitorFilter.ts`）に切り出してある。
	 * WS 購読のロジック（`connectTagStream` 呼び出し）自体は変更していない
	 * （購読最適化は T18-4b の別スライス）。
	 *
	 * 権限: relay-wright のモニタは書き込みセルを editor 以上に限定するが、
	 * この画面には書き込み要素が無いため viewer を含む全ロールが無条件で
	 * 閲覧できる（ゲートすべき対象が無い）。ツリー・検索も同様に読み取り
	 * 専用の絞り込みでしかないため、権限ゲートは追加しない。
	 */
	import { toastStore } from '$lib/toast.svelte';
	import {
		getCatalog,
		connectTagStream,
		type CatalogTagEntry,
		type StreamValue
	} from '$lib/banto/tagMonitorAdmin';
	import {
		listPlcConnections,
		listCollectionGroups,
		listTags,
		type PlcConnection,
		type CollectionGroup,
		type Tag
	} from '$lib/banto/tagRegistryAdmin';
	import { filterMonitorRows, type MonitorTreeFilter } from '$lib/banto/monitorFilter';
	import SplitPane from '$lib/components/SplitPane.svelte';
	import ConnectionTree from '$lib/components/ConnectionTree.svelte';
	import type { ConnectionTreeNodeData } from '$lib/components/connectionTreeTypes';
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

	// --- T18-4a: ツリー表示専用の補助データ（接続/収集グループ/タグ）------
	//
	// `ConnectionTree` に渡すためだけにロードする - 値表示自体は catalog
	// （`rows`）+ WS のまま変えない。tags ページの `reload()` と違い、この
	// 画面の一次表示は `rows` なので、失敗時も `rows` はそのまま・
	// エラートーストだけ出す（`loadError`/空状態は catalog 側の責務のまま
	// 触らない）。

	let connections = $state<PlcConnection[]>([]);
	let groups = $state<CollectionGroup[]>([]);
	let adminTags = $state<Tag[]>([]);

	async function reloadAdmin(): Promise<void> {
		try {
			const [nextConnections, nextGroups, nextTags] = await Promise.all([
				listPlcConnections(),
				listCollectionGroups(),
				listTags()
			]);
			connections = nextConnections;
			groups = nextGroups;
			adminTags = nextTags;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
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
		void reloadAdmin();

		const disconnect = connectTagStream({
			onData: applyStreamData,
			onConfigChanged: () => {
				void reloadCatalog();
				void reloadAdmin();
			},
			onStatusChange: (connected) => {
				wsConnected = connected;
			}
		});

		return () => {
			disconnect();
		};
	});

	// --- T18-4a: ツリー選択 + 検索（クライアント側のみ・再取得なし） --------
	//
	// tags ページの `TreeFilter`/`handleTreeSelect`/`treeSelectedId` と同じ
	// 形（`monitorFilter.ts` 冒頭の doc comment 参照）。

	let treeFilter: MonitorTreeFilter = $state({ type: 'all' });
	let searchQuery = $state('');

	const treeSelectedId = $derived.by((): string => {
		if (treeFilter.type === 'all') return 'all';
		if (treeFilter.type === 'connection') return `conn:${treeFilter.id}`;
		return `group:${treeFilter.id}`;
	});

	function handleTreeSelect(data: ConnectionTreeNodeData): void {
		if (data.kind === 'all') treeFilter = { type: 'all' };
		else if (data.kind === 'connection')
			treeFilter = { type: 'connection', id: data.connection.id };
		else treeFilter = { type: 'group', id: data.group.id };
	}

	const filteredRows = $derived(filterMonitorRows(rows, treeFilter, searchQuery));

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
	<div class="page-header">
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
	</div>

	{#if loading && rows.length === 0}
		<p class="note">読み込み中…</p>
	{:else if rows.length === 0}
		<!--
			T18-2d（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「空状態を…
			不足する前工程と移動ボタンを示す」）: タグが1件も無い（フィルタの
			問題ではなく真の空）場合は、前工程（タグ登録）へ案内する。ツリー/
			検索を出しても絞り込む対象が無いので、SplitPane は出さない。
		-->
		<p class="note">
			登録されているタグがありません。先に タグの登録画面 からタグを作成してください。
		</p>
		<a class="onboarding-cta" href="/tags">タグの登録画面へ移動</a>
	{:else}
		<!--
			T18-4a: タグ登録ページと同じ SplitPane + ConnectionTree + 検索
			ボックス。左ツリーは接続/グループを選択して絞り込むだけの表示専用
			（`oncontextmenu` は渡さない - このページに作成系 UI は無い）。
		-->
		<div class="content">
			<SplitPane leftWidth="280px">
				{#snippet left()}
					<ConnectionTree
						{connections}
						{groups}
						tags={adminTags}
						selectedId={treeSelectedId}
						onselect={handleTreeSelect}
					/>
				{/snippet}
				{#snippet right()}
					<div class="right-pane">
						<div class="toolbar">
							<input
								type="search"
								class="search-box"
								placeholder="外部名・名前・アドレスで検索"
								bind:value={searchQuery}
							/>
							<span class="count">{filteredRows.length} / {rows.length} 件</span>
						</div>
						{#if filteredRows.length === 0}
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
														<span
															class="sim-badge"
															title="シミュレーション接続（実機ではありません）">⚠ SIM</span
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
					</div>
				{/snippet}
			</SplitPane>
		</div>
	{/if}
</div>

<style>
	/* T18-4a: tags ページ（`(app)/tags/+page.svelte`）と同じ
	   page/page-header/content/right-pane/toolbar 構成 - SplitPane が
	   `height: 100%` を前提にしているため、画面全体を calc(100vh - ...) の
	   flex column にして `.content` に残り高さ全部を渡す。 */
	.page {
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		display: flex;
		flex-direction: column;
		min-height: 0;
		gap: 0.75rem;
	}

	.page-header {
		flex: 0 0 auto;
	}

	.page-header h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
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

	.content {
		flex: 1;
		min-height: 0;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		overflow: hidden;
	}

	.right-pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
		gap: 0.6rem;
		padding: 1rem 1.25rem;
	}

	.toolbar {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: 0.6rem;
		flex-wrap: wrap;
	}

	.search-box {
		margin-left: auto;
		min-width: 220px;
		padding: 0.4rem 0.6rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-size: 0.8rem;
	}

	.count {
		flex: 0 0 auto;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
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
		flex: 1;
		min-height: 0;
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
