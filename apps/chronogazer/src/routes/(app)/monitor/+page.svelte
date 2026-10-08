<script lang="ts">
	/**
	 * 監視画面（R1-D の D-1・D-2、docs/r1-plan.md）。ログイン後の既定ページ
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
	 *   `GaugePanel.svelte`。レンジ・色の判断は `meterLogic.ts`）。トレンドは
	 *   「この表示種別は準備中です」（D-3）。
	 * - **現在値のポーリング**（`valuesPoller.svelte.ts`）。周期はグループのペンの
	 *   収集周期の最短を 500ms〜5s に丸めたもの（2026-10-08 オーナー決定 Q1）。
	 *   描かない種別・ペンが無いグループ・タブが隠れている間は止める。
	 *
	 * **状態を潰さない**（チェックリスト §5）: 「グループが 0 件」「グループを読め
	 * なかった」「収集が動いていない」「現在値を取得できていない（いつの表示か）」を
	 * 別々の表示にする。
	 *
	 * タグの単位・小数桁・しきい値は既存の `/api/tags` と `/api/collection-groups`
	 * から読む（Q6。新しい API は足さない）。読めなくても値は出す（単位・しきい値
	 * なしで出し、その旨を添える）。
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
	import { displayGroupCatalog } from '#lib/monitor/displayGroupCatalog.svelte.js';
	import { ValuesPoller } from '#lib/monitor/valuesPoller.svelte.js';
	import {
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

	let tags = $state<Tag[]>([]);
	let collectionGroups = $state<CollectionGroup[]>([]);
	let tagsError = $state<string | null>(null);

	async function loadTagMeta(): Promise<void> {
		try {
			const [tagRows, groupRows] = await Promise.all([listTags(), listCollectionGroups()]);
			tags = tagRows;
			collectionGroups = groupRows;
			tagsError = null;
		} catch (err) {
			tagsError = err instanceof Error ? err.message : String(err);
		}
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
							)}」の表示は準備中です。デジタル・バー・計器のグループは表示できます。
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
					{#if tagsError !== null}
						<p class="note warn" role="status">
							タグの情報を読み込めませんでした（{tagsError}）。単位・小数桁・しきい値なしで表示しています。
						</p>
					{/if}
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
