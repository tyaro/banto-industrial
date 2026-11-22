<script lang="ts">
	/**
	 * ヒストリカル画面（#1207 の R2・#1231 の PR 2b。recorder-requirements.md §3.3、
	 * trend-design.md §10）。表示グループを選び、期間を指定して、記録した値を最小・最大の
	 * 包絡で描く。描画は共通の部品 `TrendChart`（`@banto-industrial/trend`）。
	 *
	 * - **描くタグは表示グループで決める**（2026-11-19 オーナー決定）。ペンの割当て・色・
	 *   管理線（ペンの属性）は監視画面と同じ（`trendPenInfos`）。
	 *   タグをその場で足し引きしない。
	 * - **期間は 31 日まで**。候補（1・8・24 時間・7・31 日）・任意の開始/終了・前へ/次へ。
	 *   外れたら読みに行かず、日本語で案内する（`checkRange`）。サーバーの 422 も日本語。
	 * - **拡大・縮小・パンで読み直す**: 図の操作（Ctrl+ホイール・Shift+ホイール・ドラッグ・
	 *   キー）は画面の表示範囲（`view`）をすぐ変え、`createRangeLoader` が 300ms 束ねて読み直す
	 *   （古い要求は abort し、遅れた結果は捨てる）。
	 * - **URL が状態の持ち主**（`?group=<id>&from=<ms>&to=<ms>`）。戻る・進む・再読み込みで
	 *   同じ表示に戻る。値がおかしければ既定（先頭のグループ・直近 1 時間）。履歴の積み方:
	 *   グループ・候補・期間の適用・前へ/次へは**積む**（どれも「別の表示」で、戻るで前の
	 *   表示に戻れる）。拡大・縮小・パンは止まってから（300ms）`goto` の `replace`
	 *   （フォーカス・スクロールは `reset: false` で保つ）で URL を合わせ、**積まない**。自分で書いた URL では表示範囲を巻き戻さない（`selfWritten`）。
	 *   リサイズによる読み直しは URL に触れない。
	 */
	import { onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import {
		TrendChart,
		createRangeLoader,
		type TimeRange,
		type TrendData,
		type TrendPen
	} from '@banto-industrial/trend';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import { getCollectHistory, isCollectAvailable } from '#lib/banto/collectAdmin.js';
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
	import { penLegendLabel, trendPenInfos } from '#lib/monitor/trendLogic.js';
	import ExportDialog from '#lib/historical/ExportDialog.svelte';
	import {
		HISTORY_DEBOUNCE_MS,
		HISTORY_EMPTY_MESSAGE,
		HISTORY_MAX_PENS,
		HISTORY_PRESETS,
		HISTORY_UNAVAILABLE_MESSAGE,
		HISTORY_URL_SYNC_MS,
		HistoryUnavailableError,
		RANGE_INVALID_MESSAGE,
		activePresetWidth,
		canShiftNext,
		checkRange,
		createHistoryFetch,
		fromLocalInputValue,
		historyDescription,
		historyErrorMessage,
		historyExtent,
		historyQuery,
		historyRequestFor,
		presetRange,
		resolveRange,
		sameHistoryRange,
		shiftRange,
		toHistoryRange,
		toLocalInputValue,
		toTimeRange,
		unknownTagsNote,
		type HistoryExtras,
		type HistoryRange
	} from '#lib/historical/historyLogic.js';
	import { toTrendPens } from '#lib/monitor/trendPens.js';

	const available = isTagRegistryAvailable() && isCollectAvailable();
	const canEdit = $derived(canWriteResources(sessionStore.role) && !sessionStore.publicViewer);

	// --- URL（グループ・期間）と表示範囲 ------------------------------------------

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
	/** URL の期間。 */
	const urlRange = $derived(resolved.range);

	/**
	 * 表示範囲（部品の半開区間）。URL の期間から作り、拡大・縮小・パンではこちらを先に
	 * 変える（URL は止まってから合わせる）。
	 */
	let view = $state<TimeRange>(untrack(() => toTimeRange(urlRange)));
	/** 表示範囲を両端を含む形にしたもの（入力欄・候補・前へ/次へ・CSV 出力・説明文）。 */
	const range = $derived(toHistoryRange(view));

	const rangeKey = (r: HistoryRange) => `${r.fromMs}:${r.toMs}`;
	/** 自分が replaceState で書いた期間（URL の変化が届いたら 1 つずつ消す）。 */
	const selfWritten = new Set<string>();
	let urlSyncTimer: ReturnType<typeof setTimeout> | null = null;

	function cancelUrlSync(): void {
		if (urlSyncTimer !== null) {
			clearTimeout(urlSyncTimer);
			urlSyncTimer = null;
		}
	}

	// URL の期間が変わったら、表示範囲を合わせる（戻る・進む・候補・前へ/次へ・グループ）。
	// 自分が拡大・縮小・パンで書いた URL のときは合わせない（その間に表示範囲が先へ動いて
	// いても巻き戻さない）。
	$effect(() => {
		const r = urlRange;
		untrack(() => {
			const key = rangeKey(r);
			if (selfWritten.delete(key)) return;
			// よそからの移動が勝つ: 書きかけの URL の合わせは捨てる。
			cancelUrlSync();
			selfWritten.clear();
			if (!sameHistoryRange(toHistoryRange(view), r)) view = toTimeRange(r);
		});
	});

	/** 履歴を積んで移る（グループ・期間のどちらも URL が持つ）。 */
	function go(groupId: number, next: HistoryRange): void {
		cancelUrlSync();
		selfWritten.clear();
		now = Date.now();
		void goto(`${resolveAppPath('/historical')}${historyQuery(groupId, next)}`, { reset: false });
	}

	/** 拡大・縮小・パン: 表示範囲をすぐ変え、止まってから URL を履歴を積まずに合わせる。 */
	function onRangeChange(next: TimeRange): void {
		view = next;
		cancelUrlSync();
		urlSyncTimer = setTimeout(() => {
			urlSyncTimer = null;
			const group = untrack(() => selectedGroup);
			if (!group) return;
			const target = toHistoryRange(view);
			if (sameHistoryRange(target, urlRange) && requested === group.id) return;
			selfWritten.add(rangeKey(target));
			void goto(`${resolveAppPath('/historical')}${historyQuery(group.id, target)}`, {
				replace: true,
				reset: false
			});
		}, HISTORY_URL_SYNC_MS);
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

	// 表示範囲が変わったら、入力欄を合わせる（戻る・進む・候補・前へ/次へ・拡大・縮小・パン）。
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

	/** ペンは先頭から 16 本まで（履歴の口のタグの上限。グループの上限と同じ 16）。 */
	const penGroup = $derived(
		selectedGroup && selectedGroup.pens.length > 0
			? { pens: selectedGroup.pens.slice(0, HISTORY_MAX_PENS) }
			: null
	);
	const pens = $derived(penGroup ? trendPenInfos(penGroup, tags) : []);
	const penTagIds = $derived(pens.map((pen) => pen.tagId));
	const trendPens: TrendPen[] = $derived(toTrendPens(pens));
	const penIds = $derived(trendPens.map((pen) => pen.id));

	// --- 履歴の読み出し ---------------------------------------------------------

	/** 描画域の幅（px。`TrendChart` から返る）。 */
	let width = $state(0);

	type Status = { state: 'ok' } | { state: 'unavailable' } | { state: 'error'; message: string };
	interface Loaded {
		/** グループとペンの並び。別のグループ・ペンの結果は使わない。 */
		key: string;
		data: TrendData;
		unknownTagIds: number[];
	}

	let loaded = $state.raw<Loaded | null>(null);
	let status = $state.raw<Status | null>(null);
	let loading = $state(false);
	let retryToken = $state(0);

	const extras = new WeakMap<object, HistoryExtras>();
	/** 要求のときのグループとペンの並び（結果を受けたときに `Loaded.key` にする）。 */
	let pendingKey: string | null = null;
	const loader = createRangeLoader({
		fetch: createHistoryFetch((params, signal) => getCollectHistory(params, signal), extras),
		debounceMs: HISTORY_DEBOUNCE_MS,
		onResult: (data) => {
			loaded = {
				key: pendingKey ?? '',
				data,
				unknownTagIds: extras.get(data.series)?.unknownTagIds ?? []
			};
			status = { state: 'ok' };
			loading = false;
		},
		onError: (err) => {
			status =
				err instanceof HistoryUnavailableError
					? { state: 'unavailable' }
					: { state: 'error', message: historyErrorMessage(err) };
			loading = false;
		}
	});

	const request = $derived(historyRequestFor(range, penTagIds, width));
	const dataKey = $derived(selectedGroup ? `${selectedGroup.id}:${penIds.join(',')}` : null);
	const requestKey = $derived(
		request && dataKey ? `${dataKey}|${view.startMs}:${view.endMs}|${request.bins}` : null
	);

	$effect(() => {
		const key = requestKey;
		void retryToken;
		if (key === null || !available) {
			untrack(() => (loading = false));
			return;
		}
		untrack(() => {
			const req = request;
			if (req === null) return;
			if (loaded !== null && loaded.key !== dataKey) {
				// グループ・ペンが替わった: 前の表示の結果と失敗は出さない。
				loaded = null;
				status = null;
			}
			pendingKey = dataKey;
			loading = true;
			loader.request({ range: view, penIds, bins: req.bins });
		});
	});

	/** 今のグループ・ペンの結果（読み直しの間も、取得範囲の中だけ描かれる）。 */
	const current = $derived(loaded !== null && loaded.key === dataKey ? loaded : null);
	const data = $derived(current?.data ?? null);

	// --- CSV 出力（表示中のグループ・期間） ------------------------------------------

	let exportOpen = $state(false);
	const canExport = $derived(
		penTagIds.length > 0 && checkRange(range).ok && request !== null && !loading
	);
	function exportTagInfo(tagId: number): { name: string; unit: string | null } {
		const pen = pens.find((p) => p.tagId === tagId);
		return pen ? { name: pen.name, unit: pen.unit } : { name: `タグ ID ${tagId}`, unit: null };
	}

	const unknownNote = $derived(
		current
			? unknownTagsNote(current.unknownTagIds, (id) => {
					const pen = pens.find((p) => p.tagId === id);
					return pen ? penLegendLabel(pen) : `タグ ID ${id}`;
				})
			: null
	);
	const description = $derived(
		historyDescription({
			range,
			penLabels: pens.map(penLegendLabel),
			extent: historyExtent(data, view)
		})
	);
	const emptyMessage = $derived(
		status?.state === 'ok' ? HISTORY_EMPTY_MESSAGE : '表示できる値はありません'
	);

	onMount(() => {
		if (!available) return;
		void reload();
		const timer = setInterval(() => (now = Date.now()), 30_000);
		return () => {
			clearInterval(timer);
			cancelUrlSync();
			loader.dispose();
		};
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
			<button
				type="button"
				class="secondary"
				onclick={() => void reload()}
				disabled={displayGroupCatalog.loading}
			>
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
						<button type="submit" class="primary">表示</button>
					</form>
					<div class="shift" role="group" aria-label="期間をずらす">
						<button type="button" onclick={() => shift('prev')}>前へ</button>
						<button type="button" onclick={() => shift('next')} disabled={!nextEnabled}>
							次へ
						</button>
					</div>
					<button type="button" onclick={() => (exportOpen = true)} disabled={!canExport}>
						CSV 出力
					</button>
				</div>
				{#if rangeError !== null}
					<p class="note warn" role="alert">{rangeError}</p>
				{/if}
				<ExportDialog
					open={exportOpen}
					groupName={selectedGroup.name}
					{range}
					tagIds={penTagIds}
					tagInfo={exportTagInfo}
					onclose={() => (exportOpen = false)}
				/>

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
					{#if status?.state === 'unavailable'}
						<p class="note warn" role="alert">
							{HISTORY_UNAVAILABLE_MESSAGE}
							<button type="button" class="secondary" onclick={() => (retryToken += 1)}>
								再試行
							</button>
						</p>
					{:else if status?.state === 'error'}
						<p class="note warn" role="alert">
							{status.message}
							<button type="button" class="secondary" onclick={() => (retryToken += 1)}>
								再試行
							</button>
						</p>
					{/if}
					{#if loading}
						<p class="note" role="status">履歴を読み込んでいます。</p>
					{/if}
					<section class="history-panel" aria-label={`${selectedGroup.name} のヒストリカル表示`}>
						<p class="sr-only">{description}</p>
						<TrendChart
							label={`${selectedGroup.name} のヒストリカル`}
							range={view}
							pens={trendPens}
							{data}
							{loading}
							{emptyMessage}
							bind:width
							{onRangeChange}
							legendLabel="ペン"
						/>
					</section>
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
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.5rem;
		background: var(--banto-surface);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
	}

	.range-bar button,
	.note button,
	.empty-state button {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		font: inherit;
		cursor: pointer;
	}

	.range-bar button:disabled,
	.note button:disabled,
	.empty-state button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.range-bar button:hover:not(:disabled),
	.note button:hover:not(:disabled),
	.empty-state button:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.range-bar button.primary {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}

	.range-bar button.primary:hover:not(:disabled) {
		background: var(--banto-primary-solid);
	}

	.range-bar .preset[aria-pressed='true'] {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}

	.range-bar .preset[aria-pressed='true']:hover:not(:disabled) {
		background: var(--banto-primary-solid);
	}

	.note button {
		margin-left: 0.5rem;
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

	.history-panel {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		min-width: 0;
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
