<script lang="ts">
	/**
	 * 監視画面（R1-D の D-1・D-2・D-3b、docs/r1-plan.md）。ログイン後の既定ページ
	 * （`routes/+page.ts`、`routes/login/+page.svelte`）。閲覧公開のセッションにも
	 * 開いている（`navigation.ts` の `publicViewer: true`）。
	 *
	 * D-1 でやること:
	 *
	 * - **表示グループのタブ**（表示の並び）。選択は URL の `?group=<id>` に載せ、
	 *   この端末で最後に見たグループを localStorage に覚える（URL の指定が優先）。
	 *   最初に開いたとき URL は書き換えない（`/monitor` のまま、覚えたグループか
	 *   先頭を出す）。コマンドパレットの「グループ: ◯◯ を表示」も同じ URL へ移る
	 *   （`commands.ts` の `displayGroupCommands`。一覧は `displayGroupCatalog` で共有）。
	 * - **デジタル表示**を描く。**バー・計器**は D-2 で足した（`BarPanel.svelte`・
	 *   `GaugePanel.svelte`。レンジ・色の判断は `meterLogic.ts`）。**トレンド**は
	 *   D-3b で足した（`TrendPanel.svelte`。格子・帯の判断は `trendLogic.ts`、
	 *   バッファの持ち主は `trendFeed.svelte.ts`）。現在値は同じポーラーの結果を
	 *   書き足し、初期窓だけ履歴（D-3a）を読む。履歴はグループ・時間窓・刻みが
	 *   変わったときに読み直し、前の構成の応答は捨てる（グループをまたいで線を
	 *   持ち越さない）。時間窓は端末ごと（Q4、localStorage）で、グループの定義には
	 *   書かない。
	 * - **現在値のポーリング**（`valuesPoller.svelte.ts`）。周期はグループのペンの
	 *   収集周期の最短を 500ms〜5s に丸めたもの（2026-10-08 オーナー決定 Q1）。
	 *   描かない種別・ペンが無いグループ・タブが隠れている間は止める。
	 *
	 * **状態を潰さない**（チェックリスト §5）: 「グループが 0 件」「グループを読め
	 * なかった」「収集が動いていない」「現在値を取得できていない（いつの表示か）」を
	 * 別々の表示にする。
	 *
	 * タグの単位・小数桁は既存の `/api/tags` と `/api/collection-groups` から読む（Q6）。
	 * しきい値は記録計の側の設定 `/api/tag-thresholds`（#532）から**別の失敗の軸**で読む
	 * （`applyTagMetaLoad`）。どちらが読めなくても値は出し、読めなかったことを別々の
	 * 注記で添える（しきい値が読めなければ前の値を使わず、色分け・判定をしない）。
	 */
	import { onDestroy, onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import { monitorGroupHref, resolveAppPath } from '#lib/navigation.js';
	import { collectTimeLabel, getCollectValues } from '#lib/banto/collectAdmin.js';
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
	import { ValuesPoller } from '#lib/monitor/valuesPoller.svelte.js';
	import {
		INITIAL_TAG_META,
		applyTagMetaLoad,
		tagMetaNotices,
		tagsForDisplay,
		type LoadResult,
		KIND_NOT_READY_MESSAGE,
		groupPenViews,
		isKindRendered,
		loadLastGroup,
		parseGroupParam,
		pollPeriodMs,
		saveLastGroup,
		selectGroup,
		valuesStaleNote
	} from '#lib/monitor/monitorLogic.js';
	import { kindLabel } from '../groups/groupsPageLogic';
	import { meterViews } from '#lib/monitor/meterLogic.js';
	import DigitalPanel from '#lib/monitor/DigitalPanel.svelte';
	import BarPanel from '#lib/monitor/BarPanel.svelte';
	import GaugePanel from '#lib/monitor/GaugePanel.svelte';
	import TrendPanel from '#lib/monitor/TrendPanel.svelte';
	import { TrendFeed } from '#lib/monitor/trendFeed.svelte.js';
	import {
		TREND_WINDOWS_SEC,
		chooseStepMs,
		groupDefaultWindowSec,
		loadTrendWindowOverride,
		resolveBandPen,
		resolveTrendWindowSec,
		saveTrendWindowOverride,
		observeServerClock,
		serverClockNow,
		type ServerClock,
		trendNotices,
		trendPenInfos
	} from '#lib/monitor/trendLogic.js';

	const available = isTagRegistryAvailable();
	const canEdit = $derived(canWriteResources(sessionStore.role) && !sessionStore.publicViewer);
	/** 閲覧公開のセッションはタグ設定を開けない（ガードが監視へ戻す）ので、リンクを出さない。 */
	const tagsHref = $derived(sessionStore.publicViewer ? null : resolveAppPath('/tags'));

	/** localStorage はアクセスそのものが投げることがある（プライベートモード等）。 */
	function deviceStorage(): Storage | undefined {
		try {
			return typeof localStorage === 'undefined' ? undefined : localStorage;
		} catch {
			return undefined;
		}
	}

	// --- 表示グループの一覧と選択 ---------------------------------------------

	const groups = $derived(displayGroupCatalog.groups);
	const requested = $derived(parseGroupParam(page.url.searchParams.get('group')));
	const remembered = loadLastGroup(deviceStorage());
	const selection = $derived(selectGroup(groups ?? [], requested, remembered));
	const selectedGroup = $derived(groups?.find((group) => group.id === selection.id) ?? null);

	$effect(() => {
		const id = selection.id;
		if (id !== null) saveLastGroup(deviceStorage(), id);
	});

	function showGroup(id: number): void {
		void goto(monitorGroupHref(id), { reset: false });
	}

	/** タブの左右キー（WAI-ARIA のタブの作法）。 */
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
		document.getElementById(`monitor-tab-${groups[next].id}`)?.focus();
	}

	// --- タグ情報（単位・小数桁・しきい値・収集周期） ---------------------------

	// #532: しきい値はタグではなく記録計の側の設定から読み、タグに添える。タグ情報と
	// しきい値は**別の失敗の軸**（`applyTagMetaLoad`）: しきい値だけ読めなくてもタグの
	// 名前・単位・小数桁は使い、しきい値は前の値を残さず「判定なし」にする。
	let tagMeta = $state(INITIAL_TAG_META);
	const tags = $derived(tagsForDisplay(tagMeta));
	const collectionGroups = $derived(tagMeta.collectionGroups);
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

	// --- 現在値のポーリング -----------------------------------------------------

	const poller = new ValuesPoller({ fetch: (signal) => getCollectValues(signal) });
	let pageVisible = $state(true);

	const periodMs = $derived(
		selectedGroup ? pollPeriodMs(selectedGroup, tags, collectionGroups) : null
	);
	/** 描くグループ（描ける種別で、ペンがある）。それ以外ではポーリングしない。 */
	const pollTarget = $derived(
		selectedGroup && isKindRendered(selectedGroup.kind) && selectedGroup.pens.length > 0
			? selectedGroup
			: null
	);

	$effect(() => {
		const target = pollTarget;
		const period = periodMs;
		const visible = pageVisible;
		untrack(() => {
			if (!available || target === null || period === null || !visible) {
				poller.stop();
			} else {
				poller.start(target.id, period);
			}
		});
	});

	/** 値はポーラーの対象が今のグループのときだけ使う（グループをまたいで持ち越さない）。 */
	const values = $derived(pollTarget && poller.key === pollTarget.id ? poller.state : null);
	const penViews = $derived(
		pollTarget && values ? groupPenViews(pollTarget, values.values, tags) : []
	);
	/** バー・計器の表示（レンジ・色、D-2）。デジタルでは使わない。 */
	const meters = $derived(
		pollTarget && (pollTarget.kind === 'bar' || pollTarget.kind === 'gauge')
			? meterViews(penViews, tags)
			: []
	);

	// --- トレンド（D-3b） ---------------------------------------------------------

	const trendFeed = new TrendFeed();
	/** トレンドの描画域の幅（px、`TrendPanel` が測る）。刻みを決めるのに使う。 */
	let trendWidth = $state(0);
	/**
	 * この画面で選んだ時間窓（グループ ID → 秒。`null` = グループの既定）。無いグループは
	 * localStorage の覚えを読む（Q4）。
	 */
	let windowChoice = $state<Record<number, number | null>>({});
	/** しきい値の帯を出すペン（グループごと。グループを替えたら既定に戻る）。 */
	let bandChoice = $state<{ groupId: number; tagId: number } | null>(null);

	const trendTarget = $derived(pollTarget && pollTarget.kind === 'trend' ? pollTarget : null);
	const trendWindowSec = $derived.by(() => {
		if (!trendTarget) return groupDefaultWindowSec({});
		const chosen = windowChoice[trendTarget.id];
		const override =
			chosen !== undefined ? chosen : loadTrendWindowOverride(deviceStorage(), trendTarget.id);
		return resolveTrendWindowSec(trendTarget.attributes, override);
	});
	const trendPens = $derived(trendTarget ? trendPenInfos(trendTarget, tags) : []);
	const bandTagId = $derived(
		trendTarget
			? resolveBandPen(
					trendPens,
					bandChoice && bandChoice.groupId === trendTarget.id ? bandChoice.tagId : null
				)
			: null
	);
	/** フィードが今のグループのものか（違えば前のグループの線を出さない）。 */
	const trendFeedCurrent = $derived(
		trendTarget !== null && trendFeed.groupId === trendTarget.id && trendFeed.buffer !== null
	);
	const trendRows = $derived(trendFeedCurrent ? (trendFeed.buffer?.rows ?? []) : []);
	const trendNoticeLines = $derived.by(() => {
		if (!trendFeedCurrent) return [];
		const nameOf = (id: number) =>
			trendPens.find((pen) => pen.tagId === id)?.name ?? `タグ ID ${id}`;
		return trendNotices({
			historyState: trendFeed.historyState,
			unknownNames: trendFeed.unknownTagIds.map(nameOf),
			simulationNames: trendFeed.simulationTagIds.map(nameOf)
		});
	});

	function collectPeriodOf(tagId: number): number | null {
		const tag = tags.find((t) => t.id === tagId);
		const cg = tag ? collectionGroups.find((g) => g.id === tag.collectionGroupId) : undefined;
		return cg && Number.isFinite(cg.periodMs) && cg.periodMs > 0 ? cg.periodMs : null;
	}

	/**
	 * 表示の時計（`trendLogic.ts` の `observeServerClock`）。サーバーの時刻（`ptimeMs` の最大）を
	 * 基準に、端末の単調な経過時間で進める。サーバーの時刻を受け取るまでは `null` で、格子を
	 * 作らない（端末の時計で作らない）。時計はサーバー全体のものなので、グループを替えても残す。
	 */
	let serverClock: ServerClock = null;
	/** 時計に反映済みの現在値のスナップショット（成功のたびに新しいオブジェクト）。 */
	let observedValues: object | null = null;

	$effect(() => {
		const target = trendTarget;
		const current = values;
		const width = trendWidth;
		const windowSec = trendWindowSec;
		const period = periodMs;
		const views = penViews;
		untrack(() => {
			if (target === null) {
				trendFeed.reset();
				return;
			}
			// グループが替わったら、新しいグループの値が来る前でも前の線を捨てる。
			if (trendFeed.groupId !== null && trendFeed.groupId !== target.id) trendFeed.reset();
			if (current === null || current.phase !== 'ready' || current.values === null) return;
			if (current.values !== observedValues) {
				observedValues = current.values;
				serverClock = observeServerClock(serverClock, current.values, performance.now());
			}
			const now = serverClockNow(serverClock, performance.now());
			if (now === null || width <= 0) return;
			const penTagIds = target.pens.map((pen) => pen.tagId);
			trendFeed.sync(
				{
					groupId: target.id,
					windowMs: windowSec * 1000,
					stepMs: chooseStepMs({
						windowMs: windowSec * 1000,
						widthPx: width,
						pollPeriodMs: period ?? 1000,
						tagCount: new Set(penTagIds).size
					}),
					penTagIds,
					periodMsOf: collectPeriodOf
				},
				now,
				current.values,
				views.map((view) => view.value)
			);
		});
	});

	function onTrendWindowChange(sec: number): void {
		if (!trendTarget) return;
		const groupDefault = groupDefaultWindowSec(trendTarget.attributes);
		saveTrendWindowOverride(deviceStorage(), trendTarget.id, sec, groupDefault);
		windowChoice = { ...windowChoice, [trendTarget.id]: sec === groupDefault ? null : sec };
	}

	function onSelectBandPen(tagId: number): void {
		if (trendTarget) bandChoice = { groupId: trendTarget.id, tagId };
	}

	function onVisibilityChange(): void {
		pageVisible = document.visibilityState === 'visible';
	}

	onMount(() => {
		if (!available) return;
		pageVisible = document.visibilityState === 'visible';
		document.addEventListener('visibilitychange', onVisibilityChange);
		void reload();
	});

	onDestroy(() => {
		poller.stop();
		trendFeed.reset();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>監視</h2>
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
					<a href={resolveAppPath('/groups')}>グループ設定</a>
					で表示グループを作成すると、ここにトレンド・デジタル・バー・計器表示が並びます。
				</p>
			{:else}
				<p class="empty-hint">
					表示グループは編集者以上のアカウントがグループ設定で作成します。作成されると、ここにトレンド・デジタル・バー・計器表示が並びます。
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
					id={`monitor-tab-${group.id}`}
					aria-selected={group.id === selection.id}
					aria-controls="monitor-panel"
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

		{#if selectedGroup}
			<div
				class="panel"
				id="monitor-panel"
				role="tabpanel"
				aria-labelledby={`monitor-tab-${selectedGroup.id}`}
			>
				{#if !isKindRendered(selectedGroup.kind)}
					<div class="empty-state">
						<p class="empty-message">{KIND_NOT_READY_MESSAGE}</p>
						<p class="empty-hint">
							「{kindLabel(
								selectedGroup.kind
							)}」の表示は準備中です。トレンド・デジタル・バー・計器のグループは表示できます。
						</p>
					</div>
				{:else if selectedGroup.pens.length === 0}
					<div class="empty-state">
						<p class="empty-message">このグループにはペン（タグ）が割り当てられていません</p>
						{#if canEdit}
							<p class="empty-hint">
								<a href={resolveAppPath('/groups')}>グループ設定</a>でタグを割り当ててください。
							</p>
						{/if}
					</div>
				{:else}
					{#each tagMetaNoticeLines as notice (notice)}
						<p class="note warn" role="status">{notice}</p>
					{/each}
					{#if poller.stale}
						<p class="note warn" role="status">
							{valuesStaleNote(values?.lastOkAt ?? null, collectTimeLabel)}
						</p>
					{/if}
					{#if values === null || values.phase === 'loading'}
						<p class="note">現在値を読み込んでいます。</p>
					{:else if values.phase === 'notRunning'}
						<div class="empty-state">
							<p class="empty-message">収集が動いていません</p>
							<p class="empty-hint">
								現在値は収集中にだけ表示されます。
								{#if canEdit}
									<a href={resolveAppPath('/settings/collect')}>収集の設定</a
									>で収集を開始してください。
								{/if}
							</p>
						</div>
					{:else if selectedGroup.kind === 'trend'}
						<TrendPanel
							group={selectedGroup}
							pens={trendPens}
							rows={trendRows}
							windowSec={trendWindowSec}
							windowOptions={TREND_WINDOWS_SEC}
							onWindowChange={onTrendWindowChange}
							{bandTagId}
							{onSelectBandPen}
							notices={trendNoticeLines}
							historyLoading={trendFeedCurrent && trendFeed.historyState === 'loading'}
							bind:width={trendWidth}
						/>
					{:else if selectedGroup.kind === 'bar'}
						<BarPanel group={selectedGroup} pens={meters} timeLabel={collectTimeLabel} {tagsHref} />
					{:else if selectedGroup.kind === 'gauge'}
						<GaugePanel
							group={selectedGroup}
							pens={meters}
							timeLabel={collectTimeLabel}
							{tagsHref}
						/>
					{:else}
						<DigitalPanel
							group={selectedGroup}
							pens={penViews}
							timeLabel={collectTimeLabel}
							{tagsHref}
						/>
					{/if}
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
