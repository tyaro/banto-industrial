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
	 * 8b2a（#1207・#1231）: 新規作成・名前・保存する時間窓・ペンの追加／削除／並べ替え／色・下書き・
	 * 保存（上書き・別の自由トレンドとして保存・版の食い違い）。
	 * - 下書きは sessionStorage に対象ごと（既存は ID、新規は `new`）。タブを閉じるまで残るので、
	 *   画面を離れるときの確認は付けない。下書きがあれば、チャートは下書きのペンで描く。
	 * - ペンの属性の編集欄は 8b2b。保存済みの属性は下書きと保存でそのまま保つ。
	 * - ペンは登録済みのタグだけ（§9 Q9）。作れるのはアカウントのログインか localDesktop。
	 */
	import { onDestroy, onMount, untrack } from 'svelte';
	import { isProviderError } from '@banto/admin-core';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { resolveAppPath } from '#lib/navigation.js';
	import { collectTimeLabel, getCollectValues } from '#lib/banto/collectAdmin.js';
	import { DEMO_MODE_MESSAGE, isTagRegistryAvailable } from '#lib/banto/tagRegistryAdmin.js';
	import { formatSqliteUtc } from '#lib/banto/sqliteTime.js';
	import {
		MAX_FREE_TREND_NAME_CHARS,
		MAX_FREE_TRENDS,
		MAX_PENS_PER_FREE_TREND,
		createFreeTrend,
		deleteFreeTrend,
		listFreeTrends,
		updateFreeTrend,
		type FreeTrend
	} from '#lib/banto/freeTrendsAdmin.js';
	import { isRevisionConflict } from '#lib/banto/revisionConflict.js';
	import { listPlcConnections, type PlcConnection } from '#lib/banto/tagRegistryAdmin.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { toastStore } from '#lib/toast.svelte.js';
	import PenTagPicker from '#lib/components/PenTagPicker.svelte';
	import type { PickerRow } from '#lib/components/penTagPicker.js';
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
	import {
		addPens,
		afterFreeTrendSave,
		afterFreeTrendSaveFailure,
		canCreateFreeTrend,
		classifySaveFailure,
		clearDraft,
		createBlocker,
		draftFromFreeTrend,
		draftToInput,
		draftToKeep,
		emptyDraft,
		emptyFreeTrendFieldErrors,
		loadDraft,
		movePenAt,
		pruneDrafts,
		remainingPens,
		removePenAt,
		saveDraft,
		sessionDraftStorage,
		shouldDiscardSavedDraft,
		setPenColor,
		usedTagKeys,
		type DraftTarget,
		type FreeTrendDraft
	} from '#lib/monitor/freeTrendDraft.js';
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
	import {
		COLOR_SLOTS,
		TIME_WINDOW_OPTIONS,
		colorSlotVar,
		effectiveColorSlot
	} from '../groups/groupsPageLogic';

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
			// 一覧に無い ID の下書きは捨てる（消された・別の端末で消えたもの）。
			pruneDrafts(
				sessionDraftStorage(),
				rows.map((item) => item.id)
			);
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

	// --- 下書き・編集の状態 -----------------------------------------------------

	const canCreate = $derived(
		canCreateFreeTrend({
			authDisabled: sessionStore.authDisabled,
			publicViewer: sessionStore.publicViewer,
			localDesktop: sessionStore.localDesktop,
			signedIn: sessionStore.identity !== null
		})
	);
	const createBlock = $derived(
		createBlocker({ canCreate, count: list?.length ?? 0, max: MAX_FREE_TRENDS })
	);

	/** 選んでいる自由トレンドの下書き（無ければ `null` = チャートは保存されたもので描く）。 */
	let storedDraft = $state<FreeTrendDraft | null>(null);
	/** 新規の下書き。 */
	let newDraft = $state<FreeTrendDraft | null>(null);
	/** 編集欄を開いている対象。 */
	let editing = $state(false);
	let creating = $state(false);
	/** 編集欄でタグを選んでいる（`PenTagPicker`）。 */
	let picking = $state(false);
	let saving = $state(false);
	let saveErrors = $state(emptyFreeTrendFieldErrors());
	/** 保存が版の食い違いで断られた（ほかの人が先に変えた）。 */
	let conflict = $state(false);
	/** `conflict` を立てた対象（「保存されたものを読み込む」が捨てる下書きの持ち主）。 */
	let conflictFor: DraftTarget | null = null;

	const canEditSelected = $derived(selected?.canEdit === true);
	/** 下書きを始められるのは、作れる人か、その自由トレンドを変えられる人。 */
	const canDraftSelected = $derived(selected !== null && (canCreate || canEditSelected));
	const paneOpen = $derived(creating || (editing && canDraftSelected));
	/** 編集欄に出す下書き。持っていなければ保存された内容（新規は空）から始める。 */
	const working = $derived.by((): FreeTrendDraft | null => {
		if (creating) return newDraft ?? emptyDraft();
		if (editing && selected) return storedDraft ?? draftFromFreeTrend(selected);
		return null;
	});
	/** 編集中の対象（下書きのキー）。 */
	const editTarget = $derived<DraftTarget | null>(
		creating ? 'new' : editing && selected ? selected.id : null
	);
	const draftIsStale = $derived(
		storedDraft !== null &&
			selected !== null &&
			storedDraft.baseRevision !== null &&
			storedDraft.baseRevision !== selected.revision
	);

	// 表示中の自由トレンドが変わったら、その下書きを読み、編集欄を閉じる（下書きはストレージに残る）。
	// `selection` は一覧を読み直すたびに別のオブジェクトになるので、ID（数）の変化だけを見る。
	const selectedId = $derived(selection.id);
	$effect(() => {
		const id = selectedId;
		untrack(() => {
			storedDraft = id === null ? null : loadDraft(sessionDraftStorage(), id);
			editing = false;
			creating = false;
			picking = false;
			saveErrors = emptyFreeTrendFieldErrors();
			conflict = false;
		});
	});

	/** 編集の結果を持つ（元と同じなら下書きを持たない）。ストレージにも書く。 */
	function applyEdit(next: FreeTrendDraft): void {
		const storage = sessionDraftStorage();
		if (creating) {
			newDraft = draftToKeep(next, emptyDraft());
			if (newDraft) saveDraft(storage, 'new', newDraft);
			else clearDraft(storage, 'new');
		} else if (selected) {
			storedDraft = draftToKeep(next, draftFromFreeTrend(selected));
			if (storedDraft) saveDraft(storage, selected.id, storedDraft);
			else clearDraft(storage, selected.id);
		}
	}

	function discardDraft(): void {
		if (creating) {
			newDraft = null;
			clearDraft(sessionDraftStorage(), 'new');
		} else if (selected) {
			storedDraft = null;
			clearDraft(sessionDraftStorage(), selected.id);
		}
		saveErrors = emptyFreeTrendFieldErrors();
		conflict = false;
	}

	function discardSelectedDraft(): void {
		if (!selected) return;
		storedDraft = null;
		clearDraft(sessionDraftStorage(), selected.id);
		saveErrors = emptyFreeTrendFieldErrors();
		conflict = false;
	}

	function startCreate(): void {
		if (!canCreate || createBlock !== null) return;
		creating = true;
		editing = false;
		picking = false;
		saveErrors = emptyFreeTrendFieldErrors();
		conflict = false;
		void ensureConnections();
	}

	function startEdit(): void {
		if (!canDraftSelected) return;
		editing = true;
		creating = false;
		picking = false;
		saveErrors = emptyFreeTrendFieldErrors();
		conflict = false;
		void ensureConnections();
	}

	function closePane(): void {
		editing = false;
		creating = false;
		picking = false;
	}

	// --- 編集欄（ペンの追加・削除・並べ替え・色・名前・時間窓） -----------------

	let connections = $state<PlcConnection[] | null>(null);
	let connectionsError = $state<string | null>(null);

	async function ensureConnections(): Promise<void> {
		if (connections !== null) return;
		try {
			connections = await listPlcConnections();
			connectionsError = null;
		} catch (err) {
			connectionsError = errorText(err);
		}
	}

	function tagLabel(tagId: number): string {
		return (
			tagMeta.tags.find((tag) => tag.id === tagId)?.name ?? `タグ ID ${tagId}（読み込めていません）`
		);
	}

	function handlePick(rows: PickerRow[]): void {
		if (!working) return;
		const tagIds = rows.flatMap((row) => (row.pick?.kind === 'tag' ? [row.pick.tagId] : []));
		applyEdit(addPens(working, tagIds));
	}

	function onNameInput(event: Event): void {
		if (working) applyEdit({ ...working, name: (event.currentTarget as HTMLInputElement).value });
	}

	function onWindowSelect(event: Event): void {
		if (!working) return;
		applyEdit({
			...working,
			timeWindowSec: Number((event.currentTarget as HTMLSelectElement).value)
		});
	}

	function onColorSelect(index: number, event: Event): void {
		if (!working) return;
		const value = (event.currentTarget as HTMLSelectElement).value;
		applyEdit(setPenColor(working, index, value === '' ? null : Number(value)));
	}

	// --- 保存 -------------------------------------------------------------------

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			return err.body.kind === 'validation'
				? err.body.field_errors.map((fe) => fe.message).join(' / ')
				: err.message;
		}
		return errorText(err);
	}

	/**
	 * 保存する。`update` = 開いている自由トレンドを上書き（`canEdit`）、`create` = 新規、または
	 * 開いているものの内容を別の自由トレンドとして保存（`canCreate`）。
	 */
	async function save(mode: 'update' | 'create'): Promise<void> {
		const draft = working;
		const startedFor = editTarget;
		if (saving || draft === null || startedFor === null) return;
		if (mode === 'update' && !(startedFor !== 'new' && canEditSelected)) return;
		if (mode === 'create' && (!canCreate || createBlock !== null)) return;
		saving = true;
		saveErrors = emptyFreeTrendFieldErrors();
		conflict = false;
		try {
			const saved =
				mode === 'update' && startedFor !== 'new'
					? await updateFreeTrend(startedFor, draftToInput(draft, true))
					: await createFreeTrend(draftToInput(draft, false));
			toastStore.push('success', `${saved.name} を${mode === 'update' ? '更新' : '作成'}しました`);
			const next = afterFreeTrendSave({
				destroyed,
				startedFor,
				currentTarget: editTarget
			});
			// 保存を始めた後に変えた下書きは捨てない（送った中身と同じときだけ捨てる）。
			const storage = sessionDraftStorage();
			if (next.clearDraft && shouldDiscardSavedDraft(loadDraft(storage, startedFor), draft)) {
				clearDraft(storage, startedFor);
			}
			if (startedFor === 'new') {
				if (newDraft && shouldDiscardSavedDraft(newDraft, draft)) newDraft = null;
			} else if (selection.id === startedFor) {
				if (storedDraft && shouldDiscardSavedDraft(storedDraft, draft)) storedDraft = null;
			}
			// 読み直しが返るまで保存したものを選べるよう、手元の一覧へ入れる（飛行中の古い応答は捨てる）。
			loadSeq++;
			if (list) {
				list = list.some((item) => item.id === saved.id)
					? list.map((item) => (item.id === saved.id ? saved : item))
					: [...list, saved];
			}
			if (next.navigate) {
				editing = false;
				creating = false;
				picking = false;
				if (parseFreeTrendParam(page.url.searchParams.get('id')) !== saved.id) {
					void goto(freeTrendHref(saved.id));
				}
			}
			if (next.reload) await reloadList();
		} catch (err) {
			const after = afterFreeTrendSaveFailure({
				destroyed,
				startedFor,
				currentTarget: editTarget
			});
			if (destroyed) return;
			const failure = classifySaveFailure(err, isRevisionConflict, errorMessage);
			if (!after.applyToEditor) {
				// 別のものを編集している間の失敗。その編集欄には何も立てず、どれの失敗かを通知する。
				const label = startedFor === 'new' ? '新しい自由トレンド' : `自由トレンド #${startedFor}`;
				const detail =
					failure.kind === 'conflict'
						? '他で更新されていたため保存できませんでした'
						: failure.kind === 'validation'
							? '入力に誤りがあり保存できませんでした'
							: failure.message;
				toastStore.push('error', `${label} の保存に失敗しました: ${detail}`);
				if (after.reload) await reloadList();
			} else if (failure.kind === 'conflict') {
				conflict = true;
				conflictFor = startedFor;
				// 今の保存内容を読み直して、食い違いの確認に使えるようにする（下書きは残す）。
				await reloadList();
			} else if (failure.kind === 'validation') {
				saveErrors = failure.errors;
				for (const message of failure.errors.other) toastStore.push('error', message);
			} else {
				toastStore.push('error', failure.message);
				// 消えた・権限が変わった（404・403）ときに、実際の状態に合わせる。
				await reloadList();
			}
		} finally {
			saving = false;
		}
	}

	/** 版の食い違いのあと: 下書きを捨てて保存されたものを読み込む。 */
	async function loadSaved(): Promise<void> {
		// 競合を立てた対象の下書きだけを捨てる（選択が変わっていれば捨てない）。
		if (conflictFor !== null && selected && conflictFor === selected.id) discardSelectedDraft();
		await reloadList();
	}

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

	/**
	 * チャートが描く自由トレンド。下書きがあれば下書きのペンと名前・時間窓で描く（ペンを選び直して
	 * 見るのが目的）。`id` は保存されたもののまま（フィードの対象のキー）。
	 */
	const viewed = $derived<FreeTrend | null>(
		selected && storedDraft
			? {
					...selected,
					name: storedDraft.name,
					timeWindowSec: storedDraft.timeWindowSec,
					pens: storedDraft.pens
				}
			: selected
	);
	const periodMs = $derived(viewed ? pollPeriodMs(viewed, tags, collectionGroups) : null);
	const pollTarget = $derived(viewed && viewed.pens.length > 0 ? viewed : null);

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
		viewed
			? windowChoice !== null && windowChoice.id === viewed.id
				? windowChoice.sec
				: viewed.timeWindowSec
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
			clearDraft(sessionDraftStorage(), target.id);
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
		newDraft = loadDraft(sessionDraftStorage(), 'new');
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
			{#if canDraftSelected && !paneOpen}
				<button type="button" data-testid="free-trend-edit" onclick={startEdit}>ペンを編集</button>
			{/if}
			{#if selected?.canEdit}
				<button type="button" class="danger" onclick={() => void remove()} disabled={deleting}>
					削除
				</button>
			{/if}
		{/if}
		{#if available && list !== null && canCreate && !creating}
			<button
				type="button"
				data-testid="free-trend-new"
				onclick={startCreate}
				disabled={createBlock !== null}
			>
				新しく作る
			</button>
			{#if createBlock !== null}
				<span class="note warn" role="status">{createBlock}</span>
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
		{#if storedDraft !== null && selected && !creating}
			<p class="note draft" data-testid="free-trend-draft-badge" role="status">
				<strong>下書き（未保存）</strong>
				チャートは下書きのペンで描いています。タブを閉じると消えます。
				{#if draftIsStale}
					この下書きを作った後に、保存されたものが変わっています。
				{/if}
				<button type="button" class="secondary" onclick={discardSelectedDraft}>
					下書きを捨てる
				</button>
			</p>
		{/if}
		{#if newDraft !== null && !creating && canCreate}
			<p class="note draft" data-testid="free-trend-new-draft-note" role="status">
				<strong>新規の下書き</strong>
				があります（タブを閉じると消えます）。
				<button
					type="button"
					class="secondary"
					onclick={startCreate}
					disabled={createBlock !== null}
				>
					続きを編集
				</button>
			</p>
		{/if}

		<div class="body">
			<div class="main">
				{#if creating}
					<div class="empty-state">
						<p class="empty-hint">
							新しい自由トレンドを作っています。右の欄で名前とペンを決めて、保存してください。
						</p>
					</div>
				{:else if list.length === 0}
					<div class="empty-state">
						<p class="empty-message">保存された自由トレンドはありません</p>
					</div>
				{:else if viewed && selected}
					<p class="note" data-testid="free-trend-meta">
						作成 {selected.createdBy}・最終更新 {selected.updatedBy}
						{formatSqliteUtc(selected.updatedAt)}
					</p>
					<div class="panel">
						{#if viewed.pens.length === 0}
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
									group={{ name: viewed.name }}
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
			</div>

			{#if paneOpen && working}
				{@const draft = working}
				{@const hasDraft = creating ? newDraft !== null : storedDraft !== null}
				<aside class="edit-pane" data-testid="free-trend-pane" aria-label="自由トレンドの編集">
					<div class="pane-header">
						<h3>{creating ? '新しい自由トレンド' : `${selected?.name ?? ''} を編集`}</h3>
						<button
							type="button"
							class="pane-close"
							data-testid="free-trend-close-pane"
							onclick={closePane}
							aria-label="編集欄を閉じる"
							disabled={saving}
						>
							×
						</button>
					</div>
					<div class="pane-body">
						{#if picking}
							<PenTagPicker
								mode="add"
								title={`タグ追加中・追加先: ${draft.name.trim() === '' ? '新しい自由トレンド' : draft.name}`}
								connections={connections ?? []}
								{collectionGroups}
								tags={tagMeta.tags}
								hub={null}
								hubLoading={false}
								hubError={null}
								used={usedTagKeys(draft, null)}
								remaining={remainingPens(draft)}
								max={MAX_PENS_PER_FREE_TREND}
								disabled={saving}
								onpick={handlePick}
								onback={() => (picking = false)}
								onrefresh={() => {
									connections = null;
									void ensureConnections();
									void loadTagMeta();
								}}
							/>
						{:else}
							<fieldset class="editor" disabled={saving}>
								{#if connectionsError !== null}
									<p class="note warn" role="status">
										接続先を読み込めませんでした（{connectionsError}）。タグの選択で接続先が出ないことがあります。
									</p>
								{/if}
								{#if !creating && !canEditSelected}
									<p class="note" data-testid="free-trend-readonly-note">
										この自由トレンドは変えられません。内容を直して、別の自由トレンドとして保存できます。
									</p>
								{/if}
								{#if !creating && draftIsStale}
									<p class="note warn" role="status">
										この下書きを作った後に、保存されたものが変わっています。保存すると、先に変わった内容を確かめるよう求められます。
									</p>
								{/if}
								{#if conflict}
									<div class="note warn" role="alert" data-testid="free-trend-conflict">
										<p>
											ほかの人が先に変えました。保存されたものを読み込むと、下書きは捨てられます。
										</p>
										<button
											type="button"
											data-testid="free-trend-load-saved"
											onclick={() => void loadSaved()}
										>
											保存されたものを読み込む
										</button>
									</div>
								{/if}

								<label class="field">
									<span class="field-label">名前（{MAX_FREE_TREND_NAME_CHARS} 文字まで）</span>
									<input
										type="text"
										data-testid="free-trend-name"
										value={draft.name}
										maxlength={MAX_FREE_TREND_NAME_CHARS}
										oninput={onNameInput}
									/>
								</label>
								{#each saveErrors.name as message (message)}
									<p class="field-error" role="alert">{message}</p>
								{/each}

								<label class="field">
									<span class="field-label">保存する時間窓</span>
									<select
										data-testid="free-trend-window"
										value={String(draft.timeWindowSec)}
										onchange={onWindowSelect}
									>
										{#each TIME_WINDOW_OPTIONS as option (option.value)}
											<option value={String(option.value)}>{option.label}</option>
										{/each}
									</select>
								</label>
								{#each saveErrors.timeWindowSec as message (message)}
									<p class="field-error" role="alert">{message}</p>
								{/each}

								<div class="pens-head">
									<span class="field-label">
										ペン（{draft.pens.length} / {MAX_PENS_PER_FREE_TREND}）
									</span>
									<button
										type="button"
										data-testid="free-trend-add-pens"
										onclick={() => {
											picking = true;
											void ensureConnections();
										}}
										disabled={remainingPens(draft) === 0}
									>
										タグを追加
									</button>
								</div>
								{#if remainingPens(draft) === 0}
									<p class="note">ペンは {MAX_PENS_PER_FREE_TREND} 本までです。</p>
								{/if}
								{#each saveErrors.pens as message (message)}
									<p class="field-error" role="alert">{message}</p>
								{/each}
								{#if draft.pens.length === 0}
									<p class="note">ペンがありません。「タグを追加」で足してください。</p>
								{:else}
									<ol class="pen-list" data-testid="free-trend-pens">
										{#each draft.pens as pen, index (pen.tagId)}
											<li class="pen-row" data-testid="free-trend-pen-row">
												<span
													class="swatch"
													style:background={colorSlotVar(effectiveColorSlot(pen, index))}
													aria-hidden="true"
												></span>
												<span class="pen-name" title={tagLabel(pen.tagId)}>
													{tagLabel(pen.tagId)}
												</span>
												<select
													value={pen.colorSlot === null ? '' : String(pen.colorSlot)}
													onchange={(event) => onColorSelect(index, event)}
													aria-label={`${tagLabel(pen.tagId)} の色`}
												>
													<option value="">既定</option>
													{#each Array.from({ length: COLOR_SLOTS }, (_, i) => i + 1) as slot (slot)}
														<option value={String(slot)}>色 {slot}</option>
													{/each}
												</select>
												<button
													type="button"
													class="secondary"
													onclick={() => applyEdit(movePenAt(draft, index, -1))}
													disabled={index === 0}
													aria-label={`${tagLabel(pen.tagId)} を上へ`}
												>
													↑
												</button>
												<button
													type="button"
													class="secondary"
													onclick={() => applyEdit(movePenAt(draft, index, 1))}
													disabled={index === draft.pens.length - 1}
													aria-label={`${tagLabel(pen.tagId)} を下へ`}
												>
													↓
												</button>
												<button
													type="button"
													class="secondary"
													data-testid="free-trend-pen-remove"
													onclick={() => applyEdit(removePenAt(draft, index))}
													aria-label={`${tagLabel(pen.tagId)} を外す`}
												>
													外す
												</button>
												{#each saveErrors.penRows[index] ?? [] as message (message)}
													<p class="field-error row-error" role="alert">{message}</p>
												{/each}
											</li>
										{/each}
									</ol>
								{/if}

								<div class="actions">
									{#if creating ? canCreate : canEditSelected}
										<button
											type="button"
											class="primary"
											data-testid="free-trend-save"
											onclick={() => void save(creating ? 'create' : 'update')}
											disabled={creating && createBlock !== null}
										>
											{saving ? '保存しています…' : '保存'}
										</button>
									{/if}
									{#if !creating && canCreate}
										<button
											type="button"
											data-testid="free-trend-save-as-new"
											onclick={() => void save('create')}
											disabled={createBlock !== null}
										>
											別の自由トレンドとして保存
										</button>
									{/if}
									{#if hasDraft}
										<button
											type="button"
											class="secondary"
											data-testid="free-trend-draft-discard"
											onclick={discardDraft}
										>
											下書きを捨てる
										</button>
									{/if}
								</div>
								{#if createBlock !== null && (creating || canCreate)}
									<p class="note warn" role="status">{createBlock}</p>
								{/if}
							</fieldset>
						{/if}
					</div>
				</aside>
			{/if}
		</div>
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
	.body {
		flex: 1;
		min-height: 0;
		display: flex;
		gap: 0.75rem;
	}

	.main {
		flex: 1;
		min-width: 0;
		min-height: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.note.draft {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
		background: var(--banto-info-tint, var(--banto-warning-tint));
		color: var(--banto-text);
		padding: 0.35rem 0.6rem;
		border-radius: var(--banto-radius);
	}

	.edit-pane {
		flex: 0 0 min(32rem, 45%);
		min-height: 0;
		display: flex;
		flex-direction: column;
		border-left: 1px solid var(--banto-border);
		padding-left: 0.75rem;
	}

	.pane-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.pane-header h3 {
		margin: 0;
		font-size: 1rem;
	}

	.pane-body {
		flex: 1;
		min-height: 0;
		overflow: auto;
		padding-top: 0.5rem;
	}

	.editor {
		border: none;
		margin: 0;
		padding: 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.field-label {
		font-size: 0.85rem;
		color: var(--banto-text-muted);
	}

	.field-error {
		margin: 0;
		font-size: 0.85rem;
		color: var(--banto-danger-tint-text, var(--banto-danger));
	}

	.pens-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.pen-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.pen-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
	}

	.pen-name {
		flex: 1;
		min-width: 6rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.swatch {
		flex: 0 0 auto;
		width: 0.9rem;
		height: 0.9rem;
		border-radius: 2px;
	}

	.row-error {
		flex: 1 0 100%;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}
</style>
