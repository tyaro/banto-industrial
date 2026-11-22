<script lang="ts">
	/**
	 * 監視画面（R1-D の D-1・D-2・D-3b、docs/archive/r1-plan.md）。ログイン後の既定ページ
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
	 *   D-3b で足した（`TrendPanel.svelte`。共通の部品 `TrendChart` で描く。帯・追従の
	 *   判断は `trendLogic.ts`、データの持ち主は `trendFeed.svelte.ts`）。設計書 trend-design.md
	 *   §10 の 4b で**ハイブリッド**にした: 現在値は同じポーラーの結果をリングバッファに書き足し、
	 *   過去は表示範囲に要る履歴（D-3a）を読む。拡大・縮小・左へのパンで過去へつながり、
	 *   「最新へ」（右端を今へ戻す）で追従に戻る。グループ・ペンの並びが変わったら全部捨て、
	 *   前の構成の応答は捨てる（グループをまたいで線を持ち越さない）。ポーリングの失敗・収集の
	 *   停止・タブが隠れた間は、リングバッファに途切れを知らせる。時間窓は端末ごと（Q4、
	 *   localStorage）で、グループの定義には書かない。
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
	import { toastStore } from '#lib/toast.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import { monitorGroupHref, resolveAppPath } from '#lib/navigation.js';
	import { collectTimeLabel, getCollectValues } from '#lib/banto/collectAdmin.js';
	import { DEMO_MODE_MESSAGE, isTagRegistryAvailable } from '#lib/banto/tagRegistryAdmin.js';
	import { displayGroupCatalog } from '#lib/monitor/displayGroupCatalog.svelte.js';
	import { ValuesPoller } from '#lib/monitor/valuesPoller.svelte.js';
	import {
		INITIAL_TAG_META,
		applyTagMetaLoad,
		tagMetaNotices,
		tagsForDisplay,
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
	import {
		bundleKeyOf,
		bundleTabs,
		groupsOfBundle,
		tabKeyTarget
	} from '#lib/monitor/bundleLogic.js';
	import { meterViews } from '#lib/monitor/meterLogic.js';
	import DigitalPanel from '#lib/monitor/DigitalPanel.svelte';
	import BarPanel from '#lib/monitor/BarPanel.svelte';
	import GaugePanel from '#lib/monitor/GaugePanel.svelte';
	import TrendPanel from '#lib/monitor/TrendPanel.svelte';
	import { loadTagMetaResults } from '#lib/monitor/tagMetaLoad.js';
	import { LiveTrend } from '#lib/monitor/liveTrend.svelte.js';
	import { draftSessionGeneration } from '#lib/monitor/freeTrendDraftSessionWatch.js';
	import { offerGroupHandoff } from '#lib/monitor/freeTrendGroupHandoff.js';
	import {
		canCreateFreeTrend,
		draftFromDisplayGroup,
		draftOwner,
		loadDraft,
		saveDraft,
		sessionDraftStorage
	} from '#lib/monitor/freeTrendDraft.js';
	import {
		TREND_WINDOWS_SEC,
		collectPeriodMsOf,
		groupDefaultWindowSec,
		loadTrendWindowOverride,
		resolveTrendWindowSec,
		saveTrendWindowOverride,
		trendNotices,
		trendPenInfos
	} from '#lib/monitor/trendLogic.js';

	const available = isTagRegistryAvailable();
	const canEdit = $derived(canWriteResources(sessionStore.role) && !sessionStore.publicViewer);
	/** 閲覧公開のセッションは記録を開けない（ガードが監視へ戻す）ので、リンクを出さない。 */
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

	// 束（#1207・#1231 PR 5b）: 束が 1 つも無ければ `bundles` は null で、画面は平らなタブ 1 列。
	// 選んだ束は選択中のグループから求める（`?group=` と前回のグループの記憶がそのまま効く）。
	const bundles = $derived(bundleTabs(groups ?? []));
	const selectedBundleKey = $derived(selectedGroup === null ? null : bundleKeyOf(selectedGroup));
	const visibleGroups = $derived(
		bundles === null ? (groups ?? []) : groupsOfBundle(groups ?? [], selectedBundleKey)
	);

	/** 束のタブを押すと、その束の最初のグループを見せる。 */
	function showBundle(key: string | null): void {
		const first = groupsOfBundle(groups ?? [], key)[0];
		if (first) showGroup(first.id);
	}

	/** グループのタブの左右キー（WAI-ARIA のタブの作法）。 */
	function onTabKeydown(event: KeyboardEvent, index: number): void {
		const next = tabKeyTarget(event.key, index, visibleGroups.length);
		if (next === null) return;
		event.preventDefault();
		showGroup(visibleGroups[next].id);
		document.getElementById(`monitor-tab-${visibleGroups[next].id}`)?.focus();
	}

	/** 束のタブの左右キー。 */
	function onBundleKeydown(event: KeyboardEvent, index: number): void {
		const tabs = bundles ?? [];
		const next = tabKeyTarget(event.key, index, tabs.length);
		if (next === null) return;
		event.preventDefault();
		showBundle(tabs[next].key);
		document.getElementById(`monitor-bundle-tab-${next}`)?.focus();
	}

	// --- タグ情報（単位・小数桁・しきい値・収集周期） ---------------------------

	// #532: しきい値はタグではなく記録計の側の設定から読み、タグに添える。タグ情報と
	// しきい値は**別の失敗の軸**（`applyTagMetaLoad`）: しきい値だけ読めなくてもタグの
	// 名前・単位・小数桁は使い、しきい値は前の値を残さず「判定なし」にする。
	let tagMeta = $state(INITIAL_TAG_META);
	const tags = $derived(tagsForDisplay(tagMeta));
	const collectionGroups = $derived(tagMeta.collectionGroups);
	const tagMetaNoticeLines = $derived(tagMetaNotices(tagMeta));

	async function loadTagMeta(): Promise<void> {
		const { meta, thresholds } = await loadTagMetaResults();
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

	const liveTrend = new LiveTrend();
	const trendFeed = liveTrend.feed;
	/**
	 * トレンドの描画域の幅（px、`TrendChart` が返す余白を除いた幅）。履歴の区間の数を決めるのに使う。
	 * 余白は縦軸の有無だけで決まり、データを替えても幅は変わらない。
	 */
	let trendWidth = $state(0);
	/**
	 * この画面で選んだ時間窓（グループ ID → 秒。`null` = グループの既定）。無いグループは
	 * localStorage の覚えを読む（Q4）。
	 */
	let windowChoice = $state<Record<number, number | null>>({});

	const trendTarget = $derived(pollTarget && pollTarget.kind === 'trend' ? pollTarget : null);
	const trendWindowSec = $derived.by(() => {
		if (!trendTarget) return groupDefaultWindowSec({});
		const chosen = windowChoice[trendTarget.id];
		const override =
			chosen !== undefined ? chosen : loadTrendWindowOverride(deviceStorage(), trendTarget.id);
		return resolveTrendWindowSec(trendTarget.attributes, override);
	});
	const trendPens = $derived(trendTarget ? trendPenInfos(trendTarget, tags) : []);
	/** フィードが今のグループのものか（違えば前のグループの線を出さない）。 */
	const trendFeedCurrent = $derived(
		trendTarget !== null && trendFeed.groupId === trendTarget.id && trendFeed.view !== null
	);
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
		return collectPeriodMsOf(tags, collectionGroups, tagId);
	}

	/** 現在値のポーラーの結果をトレンドへ流す段取り（サーバーの時計・途切れ・対象の切り替え）。 */
	$effect(() => {
		const target = trendTarget;
		const current = values;
		const width = trendWidth;
		const windowSec = trendWindowSec;
		const period = periodMs;
		const views = penViews;
		const visible = pageVisible;
		untrack(() => {
			liveTrend.update({
				target,
				current,
				width,
				windowSec,
				periodMs: period,
				views,
				visible,
				collectPeriodOf
			});
		});
	});

	function onTrendWindowChange(sec: number): void {
		if (!trendTarget) return;
		const groupDefault = groupDefaultWindowSec(trendTarget.attributes);
		saveTrendWindowOverride(deviceStorage(), trendTarget.id, sec, groupDefault);
		windowChoice = { ...windowChoice, [trendTarget.id]: sec === groupDefault ? null : sec };
	}

	// --- 表示グループの組から自由トレンドを作る（8c） -------------------------

	const canCreateFree = $derived(
		canCreateFreeTrend({
			authDisabled: sessionStore.authDisabled,
			publicViewer: sessionStore.publicViewer,
			localDesktop: sessionStore.localDesktop,
			signedIn: sessionStore.identity !== null
		})
	);

	/** 今のグループのペンから新規の下書きを作り、自由トレンドの新規の編集欄へ移る（保存はしない）。 */
	function saveAsFreeTrend(): void {
		if (!trendTarget || !canCreateFree) return;
		const owner = draftOwner({
			authDisabled: sessionStore.authDisabled,
			publicViewer: sessionStore.publicViewer,
			localDesktop: sessionStore.localDesktop,
			accountId: sessionStore.identity?.id ?? null
		});
		if (owner === null) return;
		const storage = sessionDraftStorage();
		if (
			loadDraft(storage, owner, 'new') !== null &&
			!window.confirm('作りかけの新しい自由トレンドの下書きを置き換えますか？')
		) {
			return;
		}
		const { draft, skipped } = draftFromDisplayGroup(trendTarget, trendWindowSec);
		if (!saveDraft(storage, owner, 'new', draft)) {
			toastStore.push(
				'error',
				'下書きをこのタブに保存できなかったため、自由トレンドの画面へ移れませんでした（ブラウザの保存領域が使えないか、いっぱいです）。'
			);
			return;
		}
		offerGroupHandoff({ owner, generation: draftSessionGeneration(), skipped });
		void goto(`${resolveAppPath('/free-trends')}?new=1`);
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
					<a href={resolveAppPath('/groups')}>表示グループ</a>
					で表示グループを作成すると、ここにトレンド・デジタル・バー・計器表示が並びます。
				</p>
			{:else}
				<p class="empty-hint">
					表示グループは編集者以上のアカウントが「表示グループ」ページで作成します。作成されると、ここにトレンド・デジタル・バー・計器表示が並びます。
				</p>
			{/if}
		</div>
	{:else}
		{#if displayGroupCatalog.error !== null}
			<p class="note warn" role="status">
				表示グループの一覧を読み直せませんでした（{displayGroupCatalog.error}）。前に読めた一覧を表示しています。
			</p>
		{/if}
		{#if bundles !== null}
			<div class="tabs bundle-tabs" role="tablist" aria-label="束">
				{#each bundles as bundle, index (bundle.key ?? '')}
					<button
						type="button"
						role="tab"
						id={`monitor-bundle-tab-${index}`}
						aria-selected={bundle.key === selectedBundleKey}
						tabindex={bundle.key === selectedBundleKey ? 0 : -1}
						class:active={bundle.key === selectedBundleKey}
						onclick={() => showBundle(bundle.key)}
						onkeydown={(event) => onBundleKeydown(event, index)}
					>
						{bundle.label}
					</button>
				{/each}
			</div>
		{/if}
		<div class="tabs" role="tablist" aria-label="表示グループ">
			{#each visibleGroups as group, index (group.id)}
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
								<a href={resolveAppPath('/groups')}>表示グループ</a>でタグを割り当ててください。
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
									<a href={resolveAppPath('/settings/collect')}>
										収集の設定
									</a>で収集を開始してください。
								{/if}
							</p>
						</div>
					{:else if selectedGroup.kind === 'trend'}
						{#if canCreateFree}
							<p class="note">
								<button
									type="button"
									class="secondary"
									data-testid="save-as-free-trend"
									onclick={saveAsFreeTrend}
								>
									この組を自由トレンドとして保存
								</button>
							</p>
						{/if}
						<TrendPanel
							group={selectedGroup}
							pens={trendPens}
							data={trendFeedCurrent ? trendFeed.data : null}
							view={trendFeedCurrent ? trendFeed.view : null}
							following={!trendFeedCurrent || trendFeed.following}
							windowSec={trendWindowSec}
							windowOptions={TREND_WINDOWS_SEC}
							onWindowChange={onTrendWindowChange}
							onRangeChange={(range) => trendFeed.rangeChange(range)}
							onFollowLatest={() => trendFeed.followLatest()}
							notices={trendNoticeLines}
							historyLoading={trendFeedCurrent && trendFeed.historyState === 'loading'}
							nowMs={trendFeed.nowMs}
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
