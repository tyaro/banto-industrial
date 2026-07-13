<script lang="ts">
	/**
	 * M4 dashboard (spec §6): real charts built from the `items` data, in
	 * place of the M1-M3 placeholder cards. Same "load once, whole dataset"
	 * pattern as the items page ($effect load/dispose around
	 * createListResource) since the dashboard needs the full set to
	 * aggregate, not a paginated window.
	 */
	import {
		BarChart,
		ComboChart,
		Gauge,
		Heatmap,
		LineChart,
		PieChart,
		RadarChart,
		ScatterChart,
		Sparkline
	} from '@banto/charts';
	import { createListResource } from '@banto/admin-core';
	import {
		DockHost,
		clampWindowToHost,
		collectPanelIds,
		createDockState,
		type DockLayout,
		type PanelContent
	} from '@banto/dock-svelte';
	import type { Item } from '$lib/banto/sampleData';
	import {
		byCategory,
		categoryCountsTop,
		computeStatTiles,
		monthlyWithMovingAvg,
		priceBuckets,
		scatterSample,
		updatesByMonth,
		weekdayMonthHeat
	} from '$lib/banto/dashboard';
	import { PANEL_DEFS } from '$lib/banto/panels';
	import { getUiSettings, isTauri } from '$lib/banto/setup';
	import { listenPanelClosed, openPanelWindow } from '$lib/banto/popout';
	import DashboardPanel from '$lib/components/DashboardPanel.svelte';

	const STOCK_TARGET = 3_000_000;

	const list = createListResource<Item>('items', {
		initialParams: { pagination: { offset: 0, limit: 20_000 } }
	});

	$effect(() => {
		void list.load();
		return () => list.dispose();
	});

	const stats = $derived(computeStatTiles(list.rows));
	const categoryStock = $derived(byCategory(list.rows));
	const buckets = $derived(priceBuckets(list.rows));
	const monthCounts = $derived(updatesByMonth(list.rows));
	const scatterRows = $derived(scatterSample(list.rows));

	// v2 chart types (spec §6.1): combo (bar+line), radar, heatmap, gauge.
	const monthlyAvg = $derived(monthlyWithMovingAvg(list.rows));
	const weekdayHeat = $derived(weekdayMonthHeat(list.rows));
	const topCategories = $derived(categoryCountsTop(list.rows, 5));

	const yen = (n: number) => `¥${n.toLocaleString()}`;
	const countLabel = (n: number) => `${n.toLocaleString()}件`;

	/**
	 * M8 dock demo (spec §5, @banto/dock-svelte): the default layout docks the
	 * two chart panels side by side (a row split) with the メモ panel
	 * floating over them, so docked + floating coexist out of the box. Panels
	 * can be dragged to re-split, tabbed (drop on a pane center), undocked
	 * (drag a tab out to floating), and the split divider resizes live. The
	 * whole layout auto-saves; a stored M7 (v1, floating-only) value migrates
	 * cleanly via hydrate.
	 */
	const DOCK_STORAGE_KEY = 'banto.dock.dashboard';

	// Panel id/title/icon defs moved to $lib/banto/panels.ts (spec §5.3 v2):
	// shared with the standalone routes/panel/[id] route a popped-out panel
	// renders as, which has no access to this page's own locals.
	const PANEL_META: Record<string, { title: string; icon: string }> = Object.fromEntries(
		PANEL_DEFS.map((d) => [d.id, { title: d.title, icon: d.icon! }])
	);

	/** The seeded default: monthly | priceBuckets docked side by side, メモ floating. `hostW/H` clamp the floating window into view. */
	function defaultLayout(hostW: number, hostH: number): DockLayout {
		const memoDef = PANEL_DEFS.find((d) => d.id === 'memo')!;
		const memo = clampWindowToHost(
			{
				id: 'memo',
				title: memoDef.title,
				icon: memoDef.icon,
				x: hostW - (memoDef.width ?? 320) - 24,
				y: hostH - (memoDef.height ?? 220) - 24,
				width: memoDef.width ?? 320,
				height: memoDef.height ?? 220,
				open: true
			},
			hostW,
			hostH
		);
		return {
			version: 2,
			floating: [memo],
			docked: {
				type: 'split',
				id: 'root',
				direction: 'row',
				children: [
					{
						type: 'panel',
						id: 'monthly',
						title: PANEL_META.monthly.title,
						icon: PANEL_META.monthly.icon
					},
					{
						type: 'panel',
						id: 'priceBuckets',
						title: PANEL_META.priceBuckets.title,
						icon: PANEL_META.priceBuckets.icon
					}
				],
				sizes: [0.5, 0.5]
			}
		};
	}

	// M12 (spec §12.1): the UiSettingsProvider key the layout is ALSO saved
	// under (settings DB via Tauri/REST; localStorage in demo mode).
	// localStorage (DOCK_STORAGE_KEY) stays the synchronous first-paint
	// source; the provider copy is reconciled once after mount, below.
	const DOCK_SETTING_KEY = 'dock.dashboard';

	function loadDockLayout(): string | undefined {
		if (typeof localStorage === 'undefined') return undefined;
		return localStorage.getItem(DOCK_STORAGE_KEY) ?? undefined;
	}

	const dock = createDockState(loadDockLayout());

	let dockHostW = $state(0);
	let dockHostH = $state(0);
	let seeded = $state(false);

	// Seed the docked default once the host has a real measured size, but only
	// if nothing was restored from storage (an empty layout = fresh visit).
	// After seeding, a panel the user fully removed stays removed.
	$effect(() => {
		if (dockHostW === 0 || dockHostH === 0 || seeded) return;
		seeded = true;
		const known = new Set([
			...collectPanelIds(dock.layout.docked),
			...dock.layout.floating.map((w) => w.id)
		]);
		if (known.size === 0) {
			dock.layout = defaultLayout(dockHostW, dockHostH);
		}
	});

	// M12: after the localStorage-seeded first paint, read the provider's
	// copy once - if it holds a layout that differs from the local cache
	// (e.g. saved from another client/session), it wins: hydrate + refresh
	// the cache. A missing key or any provider failure keeps the local state.
	let providerChecked = false;
	$effect(() => {
		if (providerChecked) return;
		providerChecked = true;
		void (async () => {
			try {
				const remote = await getUiSettings().get(DOCK_SETTING_KEY);
				if (remote !== null && remote !== localStorage.getItem(DOCK_STORAGE_KEY)) {
					dock.hydrate(remote);
					localStorage.setItem(DOCK_STORAGE_KEY, remote);
				}
			} catch {
				// Best-effort: offline/unauthenticated reads keep the local layout.
			}
		})();
	});

	// M12 provider autosave, debounced: a divider/window drag serializes on
	// every pointermove, and each of those must NOT become a settings-DB
	// write. localStorage (cheap, synchronous) still saves immediately below;
	// the provider gets one write 500ms after the layout stops changing (plus
	// a flush on unmount so the last change is never lost).
	const DOCK_SAVE_DEBOUNCE_MS = 500;
	let dockSaveTimer: ReturnType<typeof setTimeout> | undefined;
	let pendingDockJson: string | null = null;
	let dockSaveArmed = false;

	function flushDockSave(): void {
		if (pendingDockJson === null) return;
		const json = pendingDockJson;
		pendingDockJson = null;
		void getUiSettings()
			.set(DOCK_SETTING_KEY, json)
			.catch(() => {});
	}

	// Autosave the whole layout on every change (reading dock.serialize() ties
	// this effect to every move/resize/dock/undock/tab/close).
	$effect(() => {
		const json = dock.serialize();
		if (typeof localStorage !== 'undefined') {
			localStorage.setItem(DOCK_STORAGE_KEY, json);
		}
		// First run is the just-loaded layout, not a user change - arming
		// here avoids echoing it straight back into the settings DB.
		if (!dockSaveArmed) {
			dockSaveArmed = true;
			return;
		}
		pendingDockJson = json;
		clearTimeout(dockSaveTimer);
		dockSaveTimer = setTimeout(flushDockSave, DOCK_SAVE_DEBOUNCE_MS);
	});

	// Unmount: flush (not drop) a pending provider save, so navigating away
	// right after a drag still persists the final layout.
	$effect(() => {
		return () => {
			clearTimeout(dockSaveTimer);
			flushDockSave();
		};
	});

	function resetDockLayout() {
		dock.layout = defaultLayout(dockHostW, dockHostH);
	}

	// Each docked pane / floating window body measures its own height so the
	// chart fills it (charts ResizeObserver their own width). Keyed by panel id.
	let panelHeights = $state<Record<string, number>>({});
	function chartHeight(id: string): number {
		return Math.max(140, (panelHeights[id] ?? 240) - 16);
	}

	function isDocked(id: string): boolean {
		return collectPanelIds(dock.layout.docked).includes(id);
	}
	/** A panel counts as visible when it's in the docked tree or an open floating window. */
	function isPanelVisible(id: string): boolean {
		return isDocked(id) || dock.isOpen(id);
	}
	/** Toolbar toggle: docked panels are always shown (button disabled); floating ones open/close, and a fully-removed one re-adds as floating. */
	function togglePanel(id: string): void {
		if (isDocked(id)) return;
		if (dock.isOpen(id)) {
			dock.close(id);
			return;
		}
		const def = PANEL_DEFS.find((d) => d.id === id);
		if (def) {
			dock.ensureWindow(def, dockHostW, dockHostH);
			dock.open(id);
		}
	}

	/**
	 * v2 pop-out (spec §5.3, Tauri only): dragging a panel out via
	 * dock-svelte's `onPopOut` affordance (⧉ button) opens it as a REAL
	 * native window (`panel_open` Tauri command -> routes/panel/[id]) instead
	 * of a pseudo-window. The panel is hidden from THIS dock - undocked (if
	 * docked) then closed (if floating) - but its floating geometry is
	 * preserved, so `dock.open(id)` (below, on window-close) restores it
	 * exactly where it was. `undefined` in browser mode: DockHost then
	 * renders no pop-out button at all (see its doc comment), so this is
	 * never called there - `isTauri()` guards it anyway as a second line of
	 * defense.
	 */
	const onPopOut = isTauri()
		? (content: PanelContent) => {
				if (isDocked(content.id)) dock.undockPanel(content.id);
				dock.close(content.id);
				void openPanelWindow(content);
			}
		: undefined;

	// Round-trip contract (spec §5.3 v2): when a popped-out panel's native
	// window is closed, src-tauri emits `banto://panel-closed` with that
	// panel's id (see popout.ts's listenPanelClosed) - `dock.open` brings it
	// back as a floating pseudo-window at its preserved geometry. (If the
	// panel had been docked, `onPopOut` above already demoted it to floating
	// via `undockPanel` before closing it - same permanent docked->floating
	// transition `DockedTree`'s own ✕ button uses - so it does NOT return to
	// the docked tree; this mirrors "closing a docked panel" everywhere else
	// in this package.) No-op in browser mode.
	$effect(() => {
		if (!isTauri()) return;
		return listenPanelClosed((id) => dock.open(id));
	});
</script>

<div class="page">
	<p class="note">
		商品データ（{list.totalCount.toLocaleString()}件）から集計したダッシュボードです（M4）。折れ線・棒・円・散布図に加え、複合（棒+折れ線）・レーダー・ヒートマップ・ゲージも
		@banto/charts
		のSVGフルスクラッチ実装です（v2）。下部のドッキングレイアウトは@banto/dock-svelteによる分割・タブ化・ドラッグ再配置のデモです（M8）。ツールバーの「SPC」「トレンド」パネルはM13の追加機能（ヒストグラム・パレート図・箱ひげ図、SVGエクスポート、ズーム/パン・しきい値バンド・第2Y軸・ストリーミング更新）のデモです。
	</p>

	<div class="stat-row">
		<section class="stat-tile">
			<span class="stat-label">商品数</span>
			<span class="stat-value">{stats.count.toLocaleString()}</span>
		</section>
		<section class="stat-tile">
			<span class="stat-label">在庫合計</span>
			<div class="stat-value-row">
				<span class="stat-value">{stats.stockTotal.toLocaleString()}</span>
				<Sparkline values={monthCounts.map((m) => m.count)} width={72} height={24} />
			</div>
		</section>
		<section class="stat-tile">
			<span class="stat-label">平均価格</span>
			<span class="stat-value">{yen(stats.avgPrice)}</span>
		</section>
		<section class="stat-tile">
			<span class="stat-label">在庫僅少（50未満）</span>
			<span class="stat-value">{stats.lowStockCount.toLocaleString()}</span>
		</section>
	</div>

	{#if list.loading && list.rows.length === 0}
		<p class="loading">読み込み中…</p>
	{:else}
		<div class="chart-grid">
			<section class="card">
				<h2>カテゴリ別在庫</h2>
				<BarChart
					data={categoryStock}
					category={(row) => row.category}
					series={[{ id: 'stock', label: '在庫', value: (row) => row.stock }]}
					horizontal
					label="カテゴリ別在庫の横棒グラフ"
					height={280}
					formatValue={(n) => n.toLocaleString()}
				/>
			</section>

			<section class="card">
				<h2>価格帯分布</h2>
				<PieChart
					data={buckets}
					category={(row) => row.bucket}
					value={(row) => row.count}
					donut
					label="価格帯分布のドーナツグラフ"
					height={280}
					formatValue={countLabel}
				/>
			</section>

			<section class="card">
				<h2>月別更新件数</h2>
				<LineChart
					data={monthCounts}
					x={(row) => row.month}
					series={[{ id: 'count', label: '更新件数', y: (row) => row.count }]}
					area
					label="月別更新件数の面グラフ"
					height={280}
					formatY={(n) => n.toLocaleString()}
				/>
			</section>

			<section class="card">
				<h2>価格×在庫</h2>
				<ScatterChart
					data={scatterRows}
					x={(row) => row.price}
					y={(row) => row.stock}
					pointLabel={(row) => row.name}
					label="価格と在庫の散布図"
					height={280}
					formatX={(v) => yen(Number(v))}
					formatY={(v) => countLabel(Number(v))}
				/>
			</section>
		</div>

		<h2 class="section-heading">チャート拡張（v2）</h2>
		<div class="chart-grid">
			<section class="card">
				<h2>月別更新件数と3ヶ月移動平均</h2>
				<ComboChart
					data={monthlyAvg}
					x={(row) => row.month}
					bars={[{ id: 'count', label: '更新件数', value: (row) => row.count }]}
					lines={[{ id: 'avg3', label: '3ヶ月移動平均', y: (row) => row.avg3 }]}
					label="月別更新件数と3ヶ月移動平均の複合グラフ"
					height={280}
					formatY={(n) => n.toLocaleString()}
				/>
			</section>

			<section class="card">
				<h2>曜日×月の更新件数</h2>
				<Heatmap
					data={weekdayHeat}
					x={(row) => row.month}
					y={(row) => row.weekday}
					value={(row) => row.count}
					label="曜日と月別の更新件数ヒートマップ"
					height={300}
					formatValue={(n) => n.toLocaleString()}
				/>
			</section>

			<section class="card">
				<h2>在庫充足率</h2>
				<Gauge
					value={stats.stockTotal}
					max={STOCK_TARGET}
					label="在庫充足率のゲージ"
					height={220}
					formatValue={(n) => `${Math.round((n / STOCK_TARGET) * 100)}%`}
				/>
			</section>

			<section class="card">
				<h2>上位カテゴリの商品数</h2>
				<RadarChart
					data={topCategories}
					axis={(row) => row.category}
					series={[{ id: 'count', label: '商品数', value: (row) => row.count }]}
					label="上位カテゴリ別商品数のレーダーチャート"
					height={280}
					formatValue={(n) => n.toLocaleString()}
				/>
			</section>

			<section class="card wide">
				<h2>ドッキングレイアウト</h2>
				<p>
					ドッキングレイアウトのデモです（M8、@banto/dock-svelte）。タイトルバーやタブをドラッグしてパネルを分割・タブ化・再配置でき、ペイン中央にドロップするとタブ、端にドロップすると分割になります。タブを外側にドラッグするとフローティング化します。仕切りのドラッグでサイズ変更、レイアウトは自動保存されます。
				</p>
				<div class="dock-toolbar" role="toolbar" aria-label="ドックウィンドウ操作">
					{#each PANEL_DEFS as def (def.id)}
						<button
							type="button"
							class="dock-toggle"
							class:active={isPanelVisible(def.id)}
							aria-pressed={isPanelVisible(def.id)}
							disabled={isDocked(def.id)}
							title={isDocked(def.id) ? 'ドック中のパネルは常に表示されます' : undefined}
							onclick={() => togglePanel(def.id)}
						>
							{def.icon}
							{def.title}
						</button>
					{/each}
					<button type="button" class="dock-reset" onclick={resetDockLayout}>リセット</button>
				</div>
				<div class="dock-wrapper" bind:clientWidth={dockHostW} bind:clientHeight={dockHostH}>
					<DockHost {dock} panel={dockPanel} {onPopOut} />
				</div>
			</section>
		</div>
	{/if}
</div>

{#snippet dockPanel(content: PanelContent)}
	<!--
		Body extracted to DashboardPanel.svelte (spec §5.3 v2 pop-out): the
		SAME component renders this panel's content standalone at
		routes/panel/[id] once popped out into a real Tauri window, so the
		markup can't depend on anything from this page beyond the measured
		height below.
	-->
	<div class="dock-panel" bind:clientHeight={panelHeights[content.id]}>
		<DashboardPanel id={content.id} height={chartHeight(content.id)} />
	</div>
{/snippet}

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.stat-row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
		gap: 1rem;
	}

	.stat-tile {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.stat-label {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.stat-value {
		font-size: 1.6rem;
		font-weight: 600;
		color: var(--banto-text);
		font-variant-numeric: tabular-nums;
	}

	.stat-value-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.section-heading {
		margin: 0.25rem 0 0;
		font-size: 1rem;
		color: var(--banto-text-muted);
	}

	.chart-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
		gap: 1rem;
	}

	.card {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
		min-width: 0;
	}

	.card.wide {
		grid-column: 1 / -1;
	}

	h2 {
		margin: 0 0 0.5rem;
		font-size: 1rem;
	}

	.card p {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.875rem;
	}

	.dock-toolbar {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin: 0.75rem 0;
	}

	.dock-toggle {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		border: 1px solid var(--banto-border);
		border-radius: 999px;
		background: var(--banto-surface);
		color: var(--banto-text-muted);
		padding: 0.35rem 0.9rem;
		font-size: 0.8rem;
		cursor: pointer;
	}

	.dock-toggle.active {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.dock-toggle:focus-visible,
	.dock-reset:focus-visible {
		outline: none;
		box-shadow: var(--banto-focus-ring);
	}

	.dock-reset {
		border: 1px solid var(--banto-border);
		border-radius: 999px;
		background: var(--banto-surface);
		color: var(--banto-text);
		padding: 0.35rem 0.9rem;
		font-size: 0.8rem;
		cursor: pointer;
		margin-left: auto;
	}

	.dock-reset:hover {
		border-color: var(--banto-primary);
	}

	.dock-wrapper {
		height: 520px;
	}

	.dock-panel {
		height: 100%;
		box-sizing: border-box;
		padding: 0.75rem;
	}
</style>
