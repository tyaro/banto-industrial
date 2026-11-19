<script lang="ts">
	/**
	 * ヒストリカル画面（#1207 の R2、PR2。recorder-requirements.md §3.3）。表示グループを
	 * 選び、期間を指定して、記録した値を最小・最大の包絡で描く。
	 *
	 * - **描くタグは表示グループで決める**（2026-11-19 オーナー決定）。ペンの割当て・色・
	 *   しきい値の帯は監視画面と同じ（`trendPenInfos`・`HistoryPanel`）。タグをその場で
	 *   足し引きしない。
	 * - **期間は 31 日まで**。候補（1・8・24 時間・7・31 日）・任意の開始/終了・前へ/次へ。
	 *   外れたら読みに行かず、日本語で案内する（`checkRange`）。サーバーの 422 も日本語。
	 * - **URL が状態の持ち主**（`?group=<id>&from=<ms>&to=<ms>`）。戻る・進む・再読み込みで
	 *   同じ表示に戻る。値がおかしければ既定（先頭のグループ・直近 1 時間）。履歴の積み方:
	 *   グループ・候補・期間の適用・前へ/次へは**積む**（どれも「別の表示」で、戻るで前の
	 *   表示に戻れる）。リサイズによる読み直しは URL に触れない。
	 * - 読み直しは 300ms 束ね、要求の世代が古い応答は捨てる（`#generation` と同じ作法）。
	 * - ズーム・パンは作っていない: `@banto/charts` の `LineChart` の `zoomable` は読み込み済みの
	 *   行の添字の中で拡大するだけで、範囲の変化を通知する口が無く、拡大した範囲を読み直せない。
	 */
	import { onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import {
		getCollectHistory,
		isCollectAvailable,
		type CollectHistory
	} from '#lib/banto/collectAdmin.js';
	import {
		DEMO_MODE_MESSAGE,
		isTagRegistryAvailable,
		listCollectionGroups,
		listTags,
		type CollectionGroup,
		type Tag
	} from '#lib/banto/tagRegistryAdmin.js';
	import { listTagThresholds } from '#lib/banto/tagThresholdsAdmin.js';
	import { displayGroupCatalog } from '#lib/monitor/displayGroupCatalog.svelte.js';
	import {
		INITIAL_TAG_META,
		applyTagMetaLoad,
		parseGroupParam,
		selectGroup,
		tagMetaNotices,
		tagsForDisplay,
		type LoadResult
	} from '#lib/monitor/monitorLogic.js';
	import {
		collectPeriodMsOf,
		penLegendLabel,
		resolveBandPen,
		trendPenInfos
	} from '#lib/monitor/trendLogic.js';
	import HistoryPanel from '#lib/historical/HistoryPanel.svelte';
	import {
		HISTORY_DEBOUNCE_MS,
		HISTORY_EMPTY_MESSAGE,
		HISTORY_MAX_PENS,
		HISTORY_PRESETS,
		HISTORY_UNAVAILABLE_MESSAGE,
		RANGE_INVALID_MESSAGE,
		activePresetWidth,
		buildHistoryGrid,
		canShiftNext,
		checkRange,
		fromLocalInputValue,
		historyDescription,
		historyErrorMessage,
		historyQuery,
		historyRequestFor,
		isEmptyGrid,
		presetRange,
		resolveRange,
		shiftRange,
		toLocalInputValue,
		unknownTagsNote,
		type HistoryRange
	} from '#lib/historical/historyLogic.js';

	const available = isTagRegistryAvailable() && isCollectAvailable();
	const canEdit = $derived(canWriteResources(sessionStore.role) && !sessionStore.publicViewer);

	// --- URL（グループ・期間） -------------------------------------------------

	/** URL に期間が無いときの既定の期間の基準（画面を開いた時刻で固定する）。 */
	const mountedAt = Date.now();
	let now = $state(mountedAt);

	const groups = $derived(displayGroupCatalog.groups);
	const requested = $derived(parseGroupParam(page.url.searchParams.get('group')));
	const selection = $derived(selectGroup(groups ?? [], requested, null));
	const selectedGroup = $derived(groups?.find((group) => group.id === selection.id) ?? null);
	const resolved = $derived(
		resolveRange(page.url.searchParams.get('from'), page.url.searchParams.get('to'), mountedAt)
	);
	const range = $derived(resolved.range);

	/** 履歴を積んで移る（グループ・期間のどちらも URL が持つ）。 */
	function go(groupId: number, next: HistoryRange): void {
		now = Date.now();
		void goto(`${resolveAppPath('/historical')}${historyQuery(groupId, next)}`, { reset: false });
	}

	function showGroup(id: number): void {
		go(id, range);
	}

	function onTabKeydown(event: KeyboardEvent, index: number): void {
		if (!groups || groups.length === 0) return;
		let next: number | null = null;
		if (event.key === 'ArrowRight') next = (index + 1) % groups.length;
		else if (event.key === 'ArrowLeft') next = (index - 1 + groups.length) % groups.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = groups.length - 1;
		if (next === null) return;
		event.preventDefault();
		showGroup(groups[next].id);
		document.getElementById(`historical-tab-${groups[next].id}`)?.focus();
	}

	// --- 期間の指定 -------------------------------------------------------------

	let fromDraft = $state('');
	let toDraft = $state('');
	let rangeError = $state<string | null>(null);

	// URL の期間が変わったら、入力欄を合わせる（戻る・進む・候補・前へ/次へ）。
	$effect(() => {
		fromDraft = toLocalInputValue(range.fromMs);
		toDraft = toLocalInputValue(range.toMs);
		rangeError = null;
	});

	function applyPreset(widthMs: number): void {
		if (!selectedGroup) return;
		go(selectedGroup.id, presetRange(widthMs, Date.now()));
	}

	function applyCustom(): void {
		if (!selectedGroup) return;
		const fromMs = fromLocalInputValue(fromDraft);
		const toMs = fromLocalInputValue(toDraft);
		if (fromMs === null || toMs === null) {
			rangeError = RANGE_INVALID_MESSAGE;
			return;
		}
		const next = { fromMs, toMs };
		const check = checkRange(next);
		if (!check.ok) {
			rangeError = check.message;
			return;
		}
		rangeError = null;
		go(selectedGroup.id, next);
	}

	function shift(direction: 'prev' | 'next'): void {
		if (!selectedGroup) return;
		go(selectedGroup.id, shiftRange(range, direction, Date.now()));
	}

	const nextEnabled = $derived(canShiftNext(range, now));
	const presetWidth = $derived(activePresetWidth(range));

	// --- タグ情報（単位・小数桁・しきい値。監視画面と同じ読み方） -----------------

	let tagMeta = $state(INITIAL_TAG_META);
	const tags = $derived(tagsForDisplay(tagMeta));
	const tagMetaNoticeLines = $derived(tagMetaNotices(tagMeta));

	function errorText(err: unknown): string {
		return err instanceof Error ? err.message : String(err);
	}

	async function loadTagMeta(): Promise<void> {
		const metaLoad: Promise<LoadResult<{ tags: Tag[]; collectionGroups: CollectionGroup[] }>> =
			Promise.all([listTags(), listCollectionGroups()]).then(
				([tagRows, groupRows]) => ({
					ok: true as const,
					value: { tags: tagRows, collectionGroups: groupRows }
				}),
				(err: unknown) => ({ ok: false as const, error: errorText(err) })
			);
		const thresholdsLoad = listTagThresholds().then(
			(rows) => ({ ok: true as const, value: rows }),
			(err: unknown) => ({ ok: false as const, error: errorText(err) })
		);
		const [meta, thresholds] = await Promise.all([metaLoad, thresholdsLoad]);
		tagMeta = applyTagMetaLoad(tagMeta, meta, thresholds);
	}

	async function reload(): Promise<void> {
		await Promise.all([displayGroupCatalog.refresh(), loadTagMeta()]);
	}

	// --- 描くペン ---------------------------------------------------------------

	/** ペンは先頭から 8 本まで（履歴の口のタグの上限。グループの上限と同じ 8）。 */
	const penGroup = $derived(
		selectedGroup && selectedGroup.pens.length > 0
			? { pens: selectedGroup.pens.slice(0, HISTORY_MAX_PENS) }
			: null
	);
	const pens = $derived(penGroup ? trendPenInfos(penGroup, tags) : []);
	const penTagIds = $derived(pens.map((pen) => pen.tagId));

	let bandChoice = $state<{ groupId: number; tagId: number } | null>(null);
	const bandTagId = $derived(
		selectedGroup
			? resolveBandPen(pens, bandChoice?.groupId === selectedGroup.id ? bandChoice.tagId : null)
			: null
	);
	function onSelectBandPen(tagId: number): void {
		if (selectedGroup) bandChoice = { groupId: selectedGroup.id, tagId };
	}

	// --- 履歴の読み出し ---------------------------------------------------------

	let width = $state(0);

	type Outcome =
		| {
				state: 'ready';
				/** 格子の材料。格子はタグ・収集グループの読み込み後に作り直せるよう、ここでは作らない。 */
				range: HistoryRange;
				bins: number;
				ids: number[];
				data: CollectHistory;
				unknownTagIds: number[];
		  }
		| { state: 'unavailable' }
		| { state: 'error'; message: string };
	interface Loaded {
		/** グループ・期間・ペンが同じか（リサイズで `bins` だけ違うときは同じ）。 */
		dataKey: string;
		bins: number;
		outcome: Outcome;
	}

	let loaded = $state.raw<Loaded | null>(null);
	let retryToken = $state(0);

	const request = $derived(historyRequestFor(range, penTagIds, width));
	const requestKey = $derived(request ? JSON.stringify(request) : null);
	const dataKey = $derived(
		selectedGroup && request
			? `${selectedGroup.id}:${request.fromMs}:${request.toMs}:${request.tagIds.join(',')}`
			: null
	);
	/** 今の要求に対する結果（別の表示のものは使わない）。 */
	const current = $derived(loaded !== null && loaded.dataKey === dataKey ? loaded : null);
	const loading = $derived(request !== null && (current === null || current.bins !== request.bins));

	let generation = 0;

	$effect(() => {
		const key = requestKey;
		void retryToken;
		if (key === null || !available) return;
		const req = untrack(() => request);
		const dKey = untrack(() => dataKey);
		const rangeNow = untrack(() => range);
		const ids = untrack(() => penTagIds);
		if (req === null || dKey === null) return;
		const mine = ++generation;
		const controller = new AbortController();
		const first = untrack(() => loaded === null);
		const timer = setTimeout(
			async () => {
				let outcome: Outcome;
				try {
					const readout = await getCollectHistory(req, controller.signal);
					if (mine !== generation) return;
					if (readout.state !== 'ready') {
						outcome = { state: 'unavailable' };
					} else {
						outcome = {
							state: 'ready',
							range: rangeNow,
							bins: req.bins,
							ids,
							data: readout.data,
							unknownTagIds: readout.data.unknownTagIds
						};
					}
				} catch (err) {
					if (mine !== generation || controller.signal.aborted) return;
					outcome = { state: 'error', message: historyErrorMessage(err) };
				}
				if (mine !== generation) return;
				loaded = { dataKey: dKey, bins: req.bins, outcome };
			},
			first ? 0 : HISTORY_DEBOUNCE_MS
		);
		return () => {
			// 条件が変わった・画面を離れた: 古い応答は世代で捨て、通信も止める。
			generation += 1;
			clearTimeout(timer);
			controller.abort();
		};
	});

	const outcome = $derived(current?.outcome ?? null);
	// タグ・収集グループが履歴の応答より後に届いても、周期を引いて格子を作り直す。
	const grid = $derived(
		outcome?.state === 'ready'
			? buildHistoryGrid(outcome.range, outcome.bins, outcome.ids, outcome.data, (tagId) =>
					collectPeriodMsOf(tags, tagMeta.collectionGroups, tagId)
				)
			: null
	);
	const gridRows = $derived(grid ? grid.rows : []);
	const gridEmpty = $derived(grid !== null && isEmptyGrid(grid));
	const unknownNote = $derived(
		outcome?.state === 'ready'
			? unknownTagsNote(outcome.unknownTagIds, (id) => {
					const pen = pens.find((p) => p.tagId === id);
					return pen ? penLegendLabel(pen) : `タグ ID ${id}`;
				})
			: null
	);
	const description = $derived(
		historyDescription({
			range,
			penLabels: pens.map(penLegendLabel),
			rows: gridRows,
			bandPenName: pens.find((p) => p.tagId === bandTagId)?.name ?? null
		})
	);
	const emptyMessage = $derived(
		loading
			? '読み込んでいます。'
			: outcome?.state === 'ready' && gridEmpty
				? HISTORY_EMPTY_MESSAGE
				: '表示できる値はありません'
	);

	onMount(() => {
		if (!available) return;
		void reload();
		const timer = setInterval(() => (now = Date.now()), 30_000);
		return () => clearInterval(timer);
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>ヒストリカル</h2>
	</div>

	{#if !available}
		<div class="empty-state">
			<p class="empty-message">{DEMO_MODE_MESSAGE}</p>
		</div>
	{:else if groups === null && displayGroupCatalog.error !== null}
		<div class="empty-state" role="alert">
			<p class="empty-message">表示グループを読み込めませんでした</p>
			<p class="empty-hint">{displayGroupCatalog.error}</p>
			<button type="button" onclick={() => void reload()} disabled={displayGroupCatalog.loading}>
				再読み込み
			</button>
		</div>
	{:else if groups === null}
		<div class="empty-state">
			<p class="empty-hint">表示グループを読み込んでいます。</p>
		</div>
	{:else if groups.length === 0}
		<div class="empty-state">
			<p class="empty-message">表示グループが未設定です</p>
			{#if canEdit}
				<p class="empty-hint">
					<a href={resolveAppPath('/groups')}>表示グループ</a>
					でグループを作成すると、ここで過去の値を期間を指定して表示できます。
				</p>
			{:else}
				<p class="empty-hint">
					表示グループは編集者以上のアカウントが「表示グループ」ページで作成します。作成されると、ここで過去の値を期間を指定して表示できます。
				</p>
			{/if}
		</div>
	{:else}
		{#if displayGroupCatalog.error !== null}
			<p class="note warn" role="status">
				表示グループの一覧を読み直せませんでした（{displayGroupCatalog.error}）。前に読めた一覧を表示しています。
			</p>
		{/if}
		<div class="tabs" role="tablist" aria-label="表示グループ">
			{#each groups as group, index (group.id)}
				<button
					type="button"
					role="tab"
					id={`historical-tab-${group.id}`}
					aria-selected={group.id === selection.id}
					aria-controls="historical-panel"
					tabindex={group.id === selection.id ? 0 : -1}
					class:active={group.id === selection.id}
					onclick={() => showGroup(group.id)}
					onkeydown={(event) => onTabKeydown(event, index)}
				>
					{group.name}
				</button>
			{/each}
		</div>

		{#if selection.missingRequested !== null}
			<p class="note warn" role="status">
				指定された表示グループ（ID {selection.missingRequested}）は見つかりませんでした。削除された可能性があります。
			</p>
		{/if}
		{#if resolved.fellBack}
			<p class="note warn" role="status">
				URL の期間を読み取れなかったため、直近 1 時間を表示しています。
			</p>
		{/if}

		{#if selectedGroup}
			<div
				class="panel"
				id="historical-panel"
				role="tabpanel"
				aria-labelledby={`historical-tab-${selectedGroup.id}`}
			>
				<div class="range-bar">
					<div class="presets" role="group" aria-label="期間の候補">
						{#each HISTORY_PRESETS as preset (preset.widthMs)}
							<button
								type="button"
								class="preset"
								aria-pressed={presetWidth === preset.widthMs}
								onclick={() => applyPreset(preset.widthMs)}
							>
								{preset.label}
							</button>
						{/each}
					</div>
					<form
						class="custom"
						onsubmit={(event) => {
							event.preventDefault();
							applyCustom();
						}}
					>
						<label>
							開始
							<input type="datetime-local" bind:value={fromDraft} required />
						</label>
						<label>
							終了
							<input type="datetime-local" bind:value={toDraft} required />
						</label>
						<button type="submit">表示</button>
					</form>
					<div class="shift" role="group" aria-label="期間をずらす">
						<button type="button" onclick={() => shift('prev')}>前へ</button>
						<button type="button" onclick={() => shift('next')} disabled={!nextEnabled}>
							次へ
						</button>
					</div>
				</div>
				{#if rangeError !== null}
					<p class="note warn" role="alert">{rangeError}</p>
				{/if}

				{#if selectedGroup.pens.length === 0}
					<div class="empty-state">
						<p class="empty-message">このグループにはペン（タグ）が割り当てられていません</p>
						{#if canEdit}
							<p class="empty-hint">
								<a href={resolveAppPath('/groups')}>表示グループ</a>でタグを割り当ててください。
							</p>
						{/if}
					</div>
				{:else}
					{#each tagMetaNoticeLines as notice (notice)}
						<p class="note warn" role="status">{notice}</p>
					{/each}
					{#if selectedGroup.pens.length > HISTORY_MAX_PENS}
						<p class="note warn" role="status">
							ペンは先頭の {HISTORY_MAX_PENS} 本だけを表示しています。
						</p>
					{/if}
					{#if unknownNote !== null}
						<p class="note warn" role="status">{unknownNote}</p>
					{/if}
					{#if outcome?.state === 'unavailable'}
						<p class="note warn" role="alert">
							{HISTORY_UNAVAILABLE_MESSAGE}
							<button type="button" onclick={() => (retryToken += 1)}>再試行</button>
						</p>
					{:else if outcome?.state === 'error'}
						<p class="note warn" role="alert">
							{outcome.message}
							<button type="button" onclick={() => (retryToken += 1)}>再試行</button>
						</p>
					{/if}
					{#if loading}
						<p class="note" role="status">履歴を読み込んでいます。</p>
					{/if}
					<HistoryPanel
						groupName={selectedGroup.name}
						{pens}
						rows={gridRows}
						spanMs={range.toMs - range.fromMs}
						{bandTagId}
						{onSelectBandPen}
						{description}
						bind:width
						{emptyMessage}
					/>
				{/if}
			</div>
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

	.tabs {
		flex: 0 0 auto;
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.tabs button {
		padding: 0.35rem 0.9rem;
		border: 1px solid transparent;
		border-bottom: none;
		border-radius: var(--banto-radius) var(--banto-radius) 0 0;
		background: transparent;
		color: var(--banto-text-muted);
		cursor: pointer;
		font: inherit;
	}

	.tabs button.active {
		border-color: var(--banto-border);
		background: var(--banto-surface);
		color: var(--banto-text);
		font-weight: 600;
	}

	.panel {
		flex: 1;
		min-height: 0;
		overflow: auto;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.range-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 1rem;
		font-size: 0.85rem;
	}

	.presets,
	.shift,
	.custom {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
	}

	.custom label {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		color: var(--banto-text-muted);
	}

	.custom input {
		font: inherit;
	}

	.preset[aria-pressed='true'] {
		border-color: var(--banto-text);
		font-weight: 600;
	}

	.note {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.note.warn {
		color: var(--banto-warning-tint-text);
		background: var(--banto-warning-tint);
		padding: 0.35rem 0.6rem;
		border-radius: var(--banto-radius);
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		text-align: center;
		color: var(--banto-text-muted);
	}

	.empty-message {
		margin: 0;
		font-size: 1rem;
		font-weight: 600;
		color: var(--banto-text);
	}

	.empty-hint {
		margin: 0;
		font-size: 0.85rem;
		max-width: 32rem;
	}

	.empty-hint a {
		color: var(--banto-primary);
	}
</style>
