<script lang="ts">
	/**
	 * 自由トレンドの画面（#1207・#1231 PR 8b1、docs/chronogazer/trend-design.md §10 の 8b1）。
	 * 保存された自由トレンド（名前付きのペンの組。全員で共有）を選んで、監視と同じ**ハイブリッド**
	 * （現在値は同じポーラーの結果をリングバッファに書き足し、過去は履歴）で表示する。閲覧公開・
	 * ログイン不要モードでも開ける（`navigation.ts` の `publicViewer: true`）。
	 *
	 * - 選択は `<select>` と URL の `?id=<id>`（優先順位は `?id=` → この端末で最後に見たもの →
	 *   先頭。判断は `freeTrendLogic.ts`）。一覧に無い `?id=` は断る。
	 * - 状態は別々に出す: 読み込み中 / 読めなかった（やり直し）/ 0 件 / デモモード / 表示中。
	 * - 時間窓の初期値は保存された `timeWindowSec`。選び直しはこの画面の中だけ（保存しない）。
	 * - 削除は `canEdit`（作った人か editor 以上）が真のときだけ。確認のうえ消し、一覧を読み直す。
	 * - ライブの段取りは監視と共通の `LiveTrend`（`liveTrend.svelte.ts`）。
	 *
	 * 新規作成・ペンの編集・保存は PR 8b2。
	 */
	import { onDestroy, onMount, untrack } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { resolveAppPath } from '#lib/navigation.js';
	import { collectTimeLabel, getCollectValues } from '#lib/banto/collectAdmin.js';
	import { DEMO_MODE_MESSAGE, isTagRegistryAvailable } from '#lib/banto/tagRegistryAdmin.js';
	import { formatSqliteUtc } from '#lib/banto/sqliteTime.js';
	import { deleteFreeTrend, listFreeTrends, type FreeTrend } from '#lib/banto/freeTrendsAdmin.js';
	import { toastStore } from '#lib/toast.svelte.js';
	import { ValuesPoller } from '#lib/monitor/valuesPoller.svelte.js';
	import {
		INITIAL_TAG_META,
		applyTagMetaLoad,
		groupPenViews,
		pollPeriodMs,
		tagMetaNotices,
		tagsForDisplay,
		valuesStaleNote
	} from '#lib/monitor/monitorLogic.js';
	import {
		afterFreeTrendDelete,
		loadLastFreeTrend,
		parseFreeTrendParam,
		saveLastFreeTrend,
		selectFreeTrend,
		windowChoiceAfterSelection,
		type FreeTrendWindowChoice
	} from '#lib/monitor/freeTrendLogic.js';
	import { loadTagMetaResults } from '#lib/monitor/tagMetaLoad.js';
	import { LiveTrend } from '#lib/monitor/liveTrend.svelte.js';
	import TrendPanel from '#lib/monitor/TrendPanel.svelte';
	import {
		TREND_WINDOWS_SEC,
		collectPeriodMsOf,
		trendNotices,
		trendPenInfos
	} from '#lib/monitor/trendLogic.js';
	import { errorText } from '../settings/modeSwitch';

	const available = isTagRegistryAvailable();

	/** localStorage はアクセスそのものが投げることがある（プライベートモード等）。 */
	function deviceStorage(): Storage | undefined {
		try {
			return typeof localStorage === 'undefined' ? undefined : localStorage;
		} catch {
			return undefined;
		}
	}

	// --- 一覧と選択 -------------------------------------------------------------

	/** 読めていない（`null`）と 0 件（`[]`）を分ける。読み直しに失敗しても前の一覧は消さない。 */
	let list = $state<FreeTrend[] | null>(null);
	let listError = $state<string | null>(null);
	let loading = $state(false);
	/** 一覧の読み込みの世代。重なったら最後に始めたものだけ採る（古い応答が新しい状態を巻き戻さない）。 */
	let loadSeq = 0;

	async function reloadList(): Promise<void> {
		const seq = ++loadSeq;
		loading = true;
		try {
			const rows = await listFreeTrends();
			if (seq !== loadSeq) return;
			list = rows;
			listError = null;
		} catch (err) {
			if (seq !== loadSeq) return;
			listError = errorText(err);
		} finally {
			if (seq === loadSeq) loading = false;
		}
	}

	const requested = $derived(parseFreeTrendParam(page.url.searchParams.get('id')));
	const remembered = loadLastFreeTrend(deviceStorage());
	const selection = $derived(selectFreeTrend(list ?? [], requested, remembered));
	const selected = $derived(list?.find((item) => item.id === selection.id) ?? null);

	$effect(() => {
		const id = selection.id;
		if (id !== null) saveLastFreeTrend(deviceStorage(), id);
	});

	function freeTrendHref(id: number | null): string {
		const base = resolveAppPath('/free-trends');
		return id === null ? base : `${base}?id=${id}`;
	}

	function onSelect(event: Event): void {
		const id = parseFreeTrendParam((event.currentTarget as HTMLSelectElement).value);
		if (id !== null) void goto(freeTrendHref(id));
	}

	// --- タグ情報（単位・小数桁・しきい値・収集周期） ---------------------------

	let tagMeta = $state(INITIAL_TAG_META);
	const tags = $derived(tagsForDisplay(tagMeta));
	const collectionGroups = $derived(tagMeta.collectionGroups);
	const tagMetaNoticeLines = $derived(tagMetaNotices(tagMeta));

	async function loadTagMeta(): Promise<void> {
		const { meta, thresholds } = await loadTagMetaResults();
		tagMeta = applyTagMetaLoad(tagMeta, meta, thresholds);
	}

	async function reload(): Promise<void> {
		await Promise.all([reloadList(), loadTagMeta()]);
	}

	// --- 現在値のポーリング -----------------------------------------------------

	const poller = new ValuesPoller({ fetch: (signal) => getCollectValues(signal) });
	let pageVisible = $state(true);

	const periodMs = $derived(selected ? pollPeriodMs(selected, tags, collectionGroups) : null);
	const pollTarget = $derived(selected && selected.pens.length > 0 ? selected : null);

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

	/** 値はポーラーの対象が今の自由トレンドのときだけ使う（切り替えをまたいで持ち越さない）。 */
	const values = $derived(pollTarget && poller.key === pollTarget.id ? poller.state : null);
	const penViews = $derived(
		pollTarget && values ? groupPenViews(pollTarget, values.values, tags) : []
	);

	// --- トレンド ---------------------------------------------------------------

	const liveTrend = new LiveTrend();
	const trendFeed = liveTrend.feed;
	/** トレンドの描画域の幅（px）。履歴の区間の数を決めるのに使う。 */
	let trendWidth = $state(0);
	/**
	 * この画面で選び直した時間窓（どの自由トレンドのものか持つ）。保存も端末への記憶もしない
	 * （下書きは PR 8b2）。自由トレンドを切り替えたら保存された値に戻る。
	 */
	let windowChoice = $state<FreeTrendWindowChoice | null>(null);
	// 表示中の自由トレンドが変わったら（選択・戻る進む・削除）一時値を捨てる。A → B → A でも戻らない。
	$effect(() => {
		const id = selection.id;
		untrack(() => {
			windowChoice = windowChoiceAfterSelection(windowChoice, id);
		});
	});
	const trendWindowSec = $derived(
		selected
			? windowChoice !== null && windowChoice.id === selected.id
				? windowChoice.sec
				: selected.timeWindowSec
			: 600
	);
	const trendPens = $derived(pollTarget ? trendPenInfos(pollTarget, tags) : []);
	const trendFeedCurrent = $derived(
		pollTarget !== null && trendFeed.groupId === pollTarget.id && trendFeed.view !== null
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

	$effect(() => {
		const target = pollTarget;
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
		if (selected) windowChoice = { id: selected.id, sec };
	}

	// --- 削除 -------------------------------------------------------------------

	let deleting = $state(false);
	/** 画面が破棄された印。削除の応答を待つ間に離れても、後から遷移や読み直しをしない。 */
	let destroyed = false;

	async function remove(): Promise<void> {
		const target = selected;
		if (deleting || !target || !target.canEdit) return;
		if (!window.confirm(`${target.name} を削除しますか？`)) return;
		deleting = true;
		try {
			await deleteFreeTrend(target.id);
			toastStore.push('success', `${target.name} を削除しました`);
			// 読み直しが返るまで消えた物を選び続けないよう、手元の一覧から外す（飛行中の古い応答は捨てる）。
			loadSeq++;
			if (list) list = list.filter((item) => item.id !== target.id);
			const next = afterFreeTrendDelete({
				destroyed,
				currentRequestedId: parseFreeTrendParam(page.url.searchParams.get('id')),
				deletedId: target.id
			});
			if (next.clearUrl) void goto(freeTrendHref(null), { replaceState: true });
			if (next.reload) await reloadList();
		} catch (err) {
			toastStore.push('error', errorText(err));
			// 他の人が先に消した・権限が変わったときは、一覧を読み直して実際の状態に合わせる。
			if (!destroyed) await reloadList();
		} finally {
			deleting = false;
		}
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
		destroyed = true;
		poller.stop();
		liveTrend.reset();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	});
</script>

<div class="page">
	<div class="page-header">
		<h2>自由トレンド</h2>
		{#if available && list !== null && list.length > 0}
			<label class="picker">
				<span>表示する自由トレンド</span>
				<select
					data-testid="free-trend-select"
					value={selection.id === null ? '' : String(selection.id)}
					onchange={onSelect}
				>
					{#each list as item (item.id)}
						<option value={String(item.id)}>{item.name}</option>
					{/each}
				</select>
			</label>
			{#if selected?.canEdit}
				<button type="button" class="danger" onclick={() => void remove()} disabled={deleting}>
					削除
				</button>
			{/if}
		{/if}
	</div>

	{#if !available}
		<div class="empty-state">
			<p class="empty-message">{DEMO_MODE_MESSAGE}</p>
		</div>
	{:else if list === null && listError !== null}
		<div class="empty-state" role="alert">
			<p class="empty-message">自由トレンドを読み込めませんでした</p>
			<p class="empty-hint">{listError}</p>
			<button type="button" onclick={() => void reload()} disabled={loading}>やり直す</button>
		</div>
	{:else if list === null}
		<div class="empty-state">
			<p class="empty-hint">自由トレンドを読み込んでいます。</p>
		</div>
	{:else if list.length === 0}
		<div class="empty-state">
			<p class="empty-message">保存された自由トレンドはありません</p>
		</div>
	{:else}
		{#if listError !== null}
			<p class="note warn" role="status">
				自由トレンドの一覧を読み直せませんでした（{listError}）。前に読めた一覧を表示しています。
			</p>
		{/if}
		{#if selection.missingRequested !== null}
			<p class="note warn" role="status">
				指定した自由トレンドは見つかりません（ID {selection.missingRequested}）。削除された可能性があります。
			</p>
		{/if}

		{#if selected}
			<p class="note" data-testid="free-trend-meta">
				作成 {selected.createdBy}・最終更新 {selected.updatedBy}
				{formatSqliteUtc(selected.updatedAt)}
			</p>
			<div class="panel">
				{#if selected.pens.length === 0}
					<div class="empty-state">
						<p class="empty-message">この自由トレンドにはペン（タグ）がありません</p>
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
							<p class="empty-hint">現在値は収集中にだけ表示されます。</p>
						</div>
					{:else}
						<TrendPanel
							group={{ name: selected.name }}
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
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
	}

	.page-header h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.picker {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.picker select {
		min-width: 12rem;
		max-width: 24rem;
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
</style>
