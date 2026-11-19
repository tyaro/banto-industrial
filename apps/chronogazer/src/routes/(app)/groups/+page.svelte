<script lang="ts">
	/**
	 * 「表示グループ」ページ（#393 = #524 の段階 1、recorder-requirements.md §6 の 4
	 * 「表示グループ（ペン割当・表示種別・しきい値）」）。
	 *
	 * 表示グループは**表示の単位**（収集グループとは別物）。1 グループに最大
	 * 8 ペン（タグの割り当て）と、トレンド / デジタル / バー / 計器の表示種別を
	 * 持つ。監視画面での描画は R1-D（段階 2）で、この画面は定義の編集だけ。
	 *
	 * **3 ペイン（#1179 PR6a、2026-11-18、docs/chronogazer/chronogazer-ui-improvement-proposal.md
	 * §2.1・§4）**: 「記録」ページ（`tags/+page.svelte`）と同じ banto 系の型にした。
	 * - 左 = 表示グループの一覧（1 階層。選択・並べ替え・追加・上限 16 の案内・
	 *   読み込み誤り）。枠は `@banto/ui` の `ThreePaneLayout`（狭幅では左右が
	 *   ドロワー。狭幅の判定はビューポート = `mobileNavStore.isNarrow`）。
	 *   上のツールバーに「表示グループを追加」。
	 * - 中央 = `@banto/ui` の `Tabs`。「ペン」（ペンの一覧 = 色の見本・タグ名・
	 *   しきい値の読み取り表示と、追加・削除・順番変更。行を選ぶと右ペインに
	 *   そのペンの設定を出す）と「設定」（名前・表示種別・既定の時間窓・削除）。
	 *   保存・取り消し・未保存の表示・楽観ロックの「最新の内容を読み込む」は、
	 *   どちらのタブでも見えるよう中央の上に置く。
	 * - 右 = 編集者以上のときだけ開く。2 つのモードを持つ（#1179 PR6b）:
	 *   「ペンの設定」（今のタグと「タグを選び直す」、色）と「タグ選択」
	 *   （`PenTagPicker.svelte`。接続先 → 収集グループのツリーか検索から選ぶ。
	 *   出どころは Hub 以外の接続先の記録項目と Hub のカタログ。未登録の Hub タグは
	 *   `hubTag` のペンになり、保存で記録項目に登録される。登録先は「ペン」タブの
	 *   ペン一覧の下）。中央の「タグを追加」で追加モード、「タグを選び直す」で
	 *   置き換えモードになる。
	 * - **下書き（`draft`）はグループ単位で一つ**。ペンの編集（右ペイン）も
	 *   名前などの編集（「設定」タブ）も同じ下書きを書き換え、保存も一回。
	 *   だからタブの切り替えやペンの選択・右ペインを閉じる操作では、未保存の
	 *   確認は要らない。確かめるのは、別のグループへ移る（または新規作成へ移る）とき。
	 * - 選んだグループとタブは URL（`?group=<id>&tab=settings`、`#lib/banto/groupsView.ts`）
	 *   に持ち、再読み込み・戻る操作で同じ画面に戻る。新規作成中は
	 *   `?group=new&tab=settings`（下書きは URL に持たない）。グループ・タブを
	 *   変えると右ペインは閉じる。
	 * - 汎用の離脱の確認（`guardUnsavedChanges`）は、クエリだけが変わるページの中の移動
	 *   （`goto`・戻る・進む）では `isSamePage: isSamePath`（banto v7.6.0）で対象外にする。
	 *   確認は `confirmViewChange` が済ませる。戻る・進むは `beforeNavigate` で確かめる。
	 *
	 * - **しきい値は読み取り専用**で、各ペンのタグの値（LL / L / H / HH）を
	 *   出すだけ。しきい値はタグ定義の属性で、保存はタグ定義の経路（記録）で
	 *   行う（§3.7.1・§3.7.6、2026-10-08 オーナー決定）。
	 * - 検証はサーバー側の 1 関数（`validate_display_group`）。画面は上限・
	 *   選択肢を「選べるものだけ出す」ことで守り、誤りはサーバーの
	 *   `field_errors` を欄ごとに出す（`groupsPageLogic.ts` の `sortFieldErrors`。
	 *   どの欄にも出せないものはトーストへ - 黙って捨てない）。
	 * - 更新は楽観ロック（`expectedRevision`）。先に別の人が更新していたら
	 *   保存を拒否し、入力は残したまま「最新の内容を読み込む」を出す。
	 * - viewer は閲覧のみ（一覧と各グループの中身は見えるが、入力欄は無効で、
	 *   右ペインは開かない）。
	 */
	import { onDestroy, onMount, untrack } from 'svelte';
	import { page } from '$app/state';
	import { beforeNavigate, goto, type BeforeNavigate } from '$app/navigation';
	import { isSamePath, UnsavedChangesNotice } from '@banto/forms';
	import { Tabs, ThreePaneLayout } from '@banto/ui';
	import { isProviderError } from '@banto/admin-core';
	import { guardUnsavedChanges, UNSAVED_DISCARD, UNSAVED_NOTICE } from '#lib/unsavedChanges.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import { toastStore } from '#lib/toast.svelte.js';
	import { sessionStore } from '#lib/session.svelte.js';
	import { canWriteResources } from '#lib/permissions.js';
	import {
		listTags,
		listCollectionGroups,
		listPlcConnections,
		isTagRegistryAvailable,
		ALLOWED_PERIOD_MS,
		DEMO_MODE_MESSAGE,
		type Tag,
		type CollectionGroup,
		type PlcConnection
	} from '#lib/banto/tagRegistryAdmin.js';
	import { getHubCatalog } from '#lib/banto/hubAdmin.js';
	import {
		collectOperationDisplay,
		collectTimeLabel,
		COLLECT_READ_TIMEOUT_MS,
		COLLECT_UI_TIMEOUT_MS,
		getCollectConnections,
		getCollectStatus,
		getCollectValues,
		isCollectAvailable,
		runCollectAction,
		runWithLimit,
		toCollectStateView,
		type CollectorStateView
	} from '#lib/banto/collectAdmin.js';
	import {
		applyBand,
		isPenUnsaved,
		penApplyState,
		nextStatusRecheckMs,
		reflectGate,
		STATUS_RECHECK_MS
	} from '#lib/monitor/applyState.js';
	import { MONITOR_POLL_DEFAULT_MS } from '#lib/monitor/monitorLogic.js';
	import { ValuesPoller } from '#lib/monitor/valuesPoller.svelte.js';
	import { valueCell, valuesNote } from '#lib/monitor/valueColumn.js';
	import { hubGroupsOf } from '#lib/banto/hubRegistration.js';
	import PenTagPicker from '#lib/components/PenTagPicker.svelte';
	import {
		describePenTag,
		penFromRow,
		type PickerHubCatalog,
		type PickerRow
	} from '#lib/components/penTagPicker.js';
	import { errorText } from '../settings/modeSwitch';
	import { listTagThresholds, type TagThresholds } from '#lib/banto/tagThresholdsAdmin.js';
	import {
		listDisplayGroups,
		createDisplayGroup,
		updateDisplayGroup,
		deleteDisplayGroup,
		reorderDisplayGroups,
		type DisplayGroup,
		type DisplayKind
	} from '#lib/banto/displayGroupsAdmin.js';
	import {
		MISSING_GROUP_MESSAGE,
		groupsViewSearch,
		parseGroupsView,
		resolveGroupsView,
		sameGroupsView,
		type GroupsSelection,
		type GroupsTab,
		type GroupsView
	} from '#lib/banto/groupsView.js';
	import {
		COLOR_SLOTS,
		KIND_OPTIONS,
		MAX_DISPLAY_GROUPS,
		MAX_PENS,
		DEFAULT_HUB_GROUP_NAME,
		TIME_WINDOW_OPTIONS,
		appendPens,
		colorSlotVar,
		defaultColorSlot,
		defaultHubTarget,
		hasHubPens,
		remainingPens,
		replacePenTag,
		draftFromGroup,
		draftProblems,
		draftToInput,
		effectiveColorSlot,
		emptyDraft,
		emptyFieldErrors,
		isDraftDirty,
		kindLabel,
		movePen,
		movedGroupIds,
		removePen,
		reloadLatestGroup,
		fieldErrorTabs,
		sortFieldErrors,
		tagsUsedByOtherPens,
		penThresholdText,
		type GroupDraft
	} from './groupsPageLogic';

	const available = isTagRegistryAvailable();
	/** 新しい収集グループの周期の既定（`DEFAULT_HUB_GROUP_PERIOD_MS` と同じ 1 秒）。 */
	const DEFAULT_PERIOD = 1000;
	const periodOptions = ALLOWED_PERIOD_MS.map((ms) => ({
		value: ms,
		label: ms >= 1000 ? `${ms / 1000}s` : `${ms}ms`
	}));
	const canWrite = $derived(canWriteResources(sessionStore.role));

	// --- 一覧（「読めていない」と「0 件」を分ける - チェックリスト §5） --------

	let groups = $state<DisplayGroup[] | null>(null);
	let groupsError = $state<string | null>(null);
	let groupsLoading = $state(false);

	let tags = $state<Tag[] | null>(null);
	let tagsError = $state<string | null>(null);
	let collectionGroups = $state<CollectionGroup[]>([]);
	let connections = $state<PlcConnection[]>([]);

	function errorMessage(err: unknown): string {
		if (isProviderError(err)) {
			return err.body.kind === 'validation'
				? err.body.field_errors.map((fe) => fe.message).join(' / ')
				: err.message;
		}
		return errorText(err);
	}

	async function reloadGroups(): Promise<void> {
		groupsLoading = true;
		try {
			groups = await listDisplayGroups();
			groupsError = null;
		} catch (err) {
			groupsError = errorMessage(err);
		} finally {
			groupsLoading = false;
		}
	}

	async function reloadTags(): Promise<void> {
		void reloadThresholds();
		try {
			const [tagRows, groupRows, connectionRows] = await Promise.all([
				listTags(),
				listCollectionGroups(),
				listPlcConnections()
			]);
			tags = tagRows;
			collectionGroups = groupRows;
			connections = connectionRows;
			tagsError = null;
		} catch (err) {
			tagsError = errorMessage(err);
		}
	}

	// #532: しきい値は記録計の側の設定（`/api/tag-thresholds`）から読む（読み取り
	// 専用の表示）。タグの一覧とは別に読み、読めなかったことは「しきい値なし」と
	// 区別して出す（`penThresholdText`）。読み直しに失敗したら前の値は捨てる（古い
	// しきい値を今の設定のように見せない。監視画面の `applyTagMetaLoad` と同じ）。
	let thresholdsByTag = $state<Map<number, TagThresholds> | null>(null);
	let thresholdsError = $state<string | null>(null);

	async function reloadThresholds(): Promise<void> {
		try {
			const rows = await listTagThresholds();
			thresholdsByTag = new Map(rows.map((row) => [row.tagId, row]));
			thresholdsError = null;
		} catch (err) {
			thresholdsByTag = null;
			thresholdsError = errorMessage(err);
		}
	}

	const tagsById = $derived(new Map((tags ?? []).map((tag) => [tag.id, tag])));

	// --- Hub のカタログ（タグ選択を最初に開いたときに読み、「一覧を更新」で読み直す）---

	let hubCatalog = $state<PickerHubCatalog | null>(null);
	let hubLoading = $state(false);
	let hubError = $state<string | null>(null);
	let hubRequested = false;
	let hubSeq = 0;

	async function reloadHubCatalog(): Promise<void> {
		const seq = ++hubSeq;
		hubLoading = true;
		try {
			const catalog = await getHubCatalog();
			if (seq !== hubSeq) return;
			hubCatalog = { status: catalog.status, tags: catalog.tags, registered: catalog.registered };
			hubError = null;
		} catch (err) {
			if (seq !== hubSeq) return;
			hubCatalog = null;
			hubError = errorMessage(err);
		} finally {
			if (seq === hubSeq) hubLoading = false;
		}
	}

	async function refreshPicker(): Promise<void> {
		await Promise.all([reloadTags(), reloadHubCatalog()]);
	}

	/** Hub 接続の下の収集グループ（`hubTag` の登録先の候補）と、登録先の既定。 */
	const hubGroups = $derived(hubGroupsOf(connections, collectionGroups));
	const fallbackTarget = $derived(defaultHubTarget(hubGroups));

	function tagLabel(tagId: number): string {
		return tagsById.get(tagId)?.name ?? `（見つからないタグ ID ${tagId}）`;
	}

	// --- 画面の状態（URL に持つもの）-------------------------------------------

	/** 左の一覧で選んでいるグループとタブ（URL の `group`・`tab`）。 */
	let view = $state<GroupsView>({ group: null, tab: 'pens' });
	/** `'new'` = 新規作成中、数値 = 編集中のグループの ID、`null` = 未選択。 */
	const selected = $derived<GroupsSelection>(view.group);
	const activeTab = $derived<GroupsTab>(view.tab);
	/** 一覧を読み終えたか（URL のグループの存在を判定してよいか）。 */
	const groupsLoaded = $derived(groups !== null);

	function applyView(next: GroupsView): void {
		if (!sameGroupsView(view, next)) view = { ...next };
	}

	// --- 編集 -----------------------------------------------------------------

	/** 下書きが今どのグループのものか（`null` = 下書きなし）。`selected` と揃ったら描く。 */
	let draftOwner = $state<GroupsSelection>(null);
	let editingRevision = $state<number | undefined>(undefined);
	let draft = $state<GroupDraft>(emptyDraft());
	let baseline = $state<GroupDraft>(emptyDraft());
	let fieldErrors = $state(emptyFieldErrors());
	let saving = $state(false);
	let deleting = $state(false);
	let reordering = $state(false);
	/** 「最新の内容を読み込む」の待ち（P2-2: 待っている間は編集・選択の操作を止める）。 */
	let reloadingLatest = $state(false);
	/** 「最新の内容を読み込む」が失敗した理由（P2-5: 入力は残したまま出す）。 */
	let latestReloadError = $state<string | null>(null);
	/**
	 * 選択・編集の世代。開き直す・新規・閉じる・取り消すのたびに進める。遅れて届いた
	 * 「最新の内容」の応答は、世代が変わっていたら捨てる（P2-2 の二重の守り）。
	 */
	let editGeneration = 0;

	const dirty = $derived(draftOwner !== null && isDraftDirty(draft, baseline));
	const busy = $derived(saving || deleting || reordering || reloadingLatest);
	/** 下書きが選択中のグループのものとして用意できているか。 */
	const draftReady = $derived(selected !== null && draftOwner === selected);
	const selectedGroup = $derived(
		typeof selected === 'number' ? (groups ?? []).find((group) => group.id === selected) : undefined
	);
	const atGroupLimit = $derived((groups?.length ?? 0) >= MAX_DISPLAY_GROUPS);

	function cloneDraft(value: GroupDraft): GroupDraft {
		return JSON.parse(JSON.stringify(value)) as GroupDraft;
	}

	function openGroup(group: DisplayGroup): void {
		editGeneration += 1;
		latestReloadError = null;
		draftOwner = group.id;
		editingRevision = group.revision;
		baseline = draftFromGroup(group);
		draft = cloneDraft(baseline);
		fieldErrors = emptyFieldErrors();
	}

	function openNewDraft(): void {
		editGeneration += 1;
		latestReloadError = null;
		draftOwner = 'new';
		editingRevision = undefined;
		baseline = emptyDraft();
		draft = emptyDraft();
		fieldErrors = emptyFieldErrors();
	}

	// 選んだグループ（`selected`）に下書きを合わせる。グループの切り替え（確認は
	// `confirmViewChange` が済ませてある）・URL からの復元・一覧の読み込み完了で
	// 走る。同じグループの下書きは作り直さない（入力中の値を消さない）。
	$effect(() => {
		const target = selected;
		const rows = groups;
		untrack(() => {
			if (target === null) {
				draftOwner = null;
				return;
			}
			if (draftOwner === target) return;
			if (target === 'new') {
				openNewDraft();
				return;
			}
			const row = rows?.find((group) => group.id === target);
			if (row) openGroup(row);
		});
	});

	function discardChanges(): void {
		editGeneration += 1;
		draft = cloneDraft(baseline);
		fieldErrors = emptyFieldErrors();
		if (selectedPen !== null && selectedPen >= draft.pens.length) closeRightPane();
	}

	/**
	 * 版の食い違いのあと: 入力を捨てて、最新の内容を開き直す。判断は
	 * `reloadLatestGroup`（`groupsPageLogic.ts`）: 待っている間に選択・編集が
	 * 変わったら何もしない（P2-2）、読めなかったら古い行で開き直さず入力を残して
	 * 理由を出す（P2-5）。待っている間は `busy` で編集・選択の操作を止める。
	 */
	async function reloadLatest(): Promise<void> {
		if (busy || typeof selected !== 'number') return;
		const id = selected;
		const generation = editGeneration;
		reloadingLatest = true;
		latestReloadError = null;
		try {
			const outcome = await reloadLatestGroup({
				id,
				load: listDisplayGroups,
				isCurrent: () => editGeneration === generation && selected === id,
				describeError: errorMessage
			});
			switch (outcome.kind) {
				case 'open':
					groups = outcome.groups;
					groupsError = null;
					closeRightPane();
					openGroup(outcome.group);
					break;
				case 'deleted':
					groups = outcome.groups;
					groupsError = null;
					editGeneration += 1;
					closeRightPane();
					forceView({ group: null, tab: 'pens' });
					toastStore.push('error', 'このグループは削除されています');
					break;
				case 'failed':
					latestReloadError = outcome.message;
					break;
				case 'stale':
					break;
			}
		} finally {
			reloadingLatest = false;
		}
	}

	function setKind(kind: DisplayKind): void {
		draft.kind = kind;
	}

	// `hubTag` のペンの登録先（下書きの一部）。
	function setTargetKind(kind: 'existing' | 'new'): void {
		draft.hubTarget =
			kind === 'existing'
				? { kind: 'existing', collectionGroupId: hubGroups[0]?.id ?? null }
				: { kind: 'new', name: DEFAULT_HUB_GROUP_NAME, periodMs: DEFAULT_PERIOD };
	}

	function setTargetGroup(value: string): void {
		const id = Number(value);
		draft.hubTarget = {
			kind: 'existing',
			collectionGroupId: value !== '' && Number.isFinite(id) ? id : null
		};
	}

	function setTargetName(name: string): void {
		const target = draft.hubTarget;
		if (target?.kind === 'new') draft.hubTarget = { ...target, name };
	}

	function setTargetPeriod(value: string): void {
		const target = draft.hubTarget;
		if (target?.kind === 'new') draft.hubTarget = { ...target, periodMs: Number(value) };
	}

	function setPenColor(index: number, value: string): void {
		draft.pens[index].colorSlot = value === '' ? null : Number(value);
	}

	async function save(): Promise<void> {
		if (busy || selected === null || !draftReady) return;
		const problems = draftProblems(draft);
		if (problems.length > 0) {
			for (const problem of problems) toastStore.push('error', problem);
			return;
		}
		saving = true;
		fieldErrors = emptyFieldErrors();
		latestReloadError = null;
		const target = selected;
		let switchTo: GroupsTab | null = null;
		try {
			const saved =
				target === 'new'
					? await createDisplayGroup(draftToInput(draft))
					: await updateDisplayGroup(target, draftToInput(draft, editingRevision));
			const registered = saved.registeredTags ?? [];
			// 登録があれば収集の状態を取り直す（保存済み・未反映の判定のため）。
			if (registered.length > 0) void refreshCollectState();
			toastStore.push(
				'success',
				(target === 'new' ? `${saved.name} を作成しました` : `${saved.name} を更新しました`) +
					(registered.length > 0
						? `。${registered.length} 件を記録項目に登録しました。上の「記録へ反映」で記録が始まります。`
						: '')
			);
			await reloadGroups();
			if (registered.length > 0) {
				// 登録された記録項目・収集グループを読み直す（ペンの名前を引けるように）。
				await reloadTags();
				if (hubCatalog !== null) {
					const names = new Set([...hubCatalog.registered, ...registered.map((t) => t.address)]);
					hubCatalog = { ...hubCatalog, registered: [...names] };
				}
			}
			openGroup(saved);
			// 作成したら、新規作成中の URL を作ったグループへ置き換える（右ペインは閉じない）。
			if (target === 'new') forceView({ group: saved.id, tab: view.tab }, { replace: true });
		} catch (err) {
			if (isProviderError(err) && err.body.kind === 'validation') {
				fieldErrors = sortFieldErrors(err.body.field_errors);
				for (const message of fieldErrors.other) toastStore.push('error', message);
				// 欄のエラーが今見ていないタブにだけあるときは、そのタブへ移す（見えないままにしない）。
				const tabs = fieldErrorTabs(fieldErrors);
				if (view.tab === 'settings' && !tabs.settings && tabs.pens) {
					switchTo = 'pens';
				} else if (view.tab === 'pens' && !tabs.pens && tabs.settings) {
					switchTo = 'settings';
				}
				if (fieldErrors.revision.length > 0) void reloadGroups();
			} else {
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			saving = false;
			// `saving` を下ろしてから切り替える（`busy` の間は `confirmViewChange` が拒む）。
			if (switchTo !== null) requestView({ group: view.group, tab: switchTo });
		}
	}

	async function remove(): Promise<void> {
		if (busy || typeof selected !== 'number' || !selectedGroup) return;
		if (!window.confirm(`${selectedGroup.name} を削除しますか？`)) return;
		deleting = true;
		const name = selectedGroup.name;
		try {
			await deleteDisplayGroup(selected);
			toastStore.push('success', `${name} を削除しました`);
			closeRightPane();
			forceView({ group: null, tab: 'pens' });
			await reloadGroups();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	async function moveGroup(id: number, delta: -1 | 1): Promise<void> {
		if (busy || !groups) return;
		const ids = movedGroupIds(groups, id, delta);
		if (!ids) return;
		reordering = true;
		try {
			groups = await reorderDisplayGroups(ids);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			await reloadGroups();
		} finally {
			reordering = false;
		}
	}

	// --- 右ペイン（選んだペンの設定）--------------------------------------------

	/** 右ペインで設定しているペンの番号（`null` = 閉じている）。 */
	let selectedPen = $state<number | null>(null);
	/** 狭幅で右ペイン（ドロワー）が開いているか（`ThreePaneLayout` の `rightDrawerOpen`）。 */
	let rightDrawerOpen = $state(false);
	/** 狭幅で左の一覧（ドロワー）が開いているか（`ThreePaneLayout` の `leftDrawerOpen`）。 */
	let listOpen = $state(false);
	/** 枠（`ThreePaneLayout`）が狭幅か（部品の `bind:narrow`、出力専用）。 */
	let layoutNarrow = $state(false);

	/**
	 * 右ペインのタグ選択のモード（`null` = ペンの設定）。`add` = ペンを足す（元に選んでいた
	 * ペン `selectedPen` は保つ）、`replace` = `selectedPen` のタグを選び直す。
	 */
	let pickMode = $state<'add' | 'replace' | null>(null);

	// --- 現在値・品質の併記（#1198 PR2。値は ChronoGazer の収集が読んでいる `/api/collect/values`）---
	//
	// 監視画面と同じポーラーを、このページを開いている間 1 本だけ回す。止める条件:
	// 離れたとき（`onDestroy`）・タブが隠れたとき・値を並べる場面が無いとき（「ペン」タブ
	// 以外、または登録済みのペンもタグ選択も無い）。Hub にしか無いタグは取りに行かない。
	const valuesPoller = new ValuesPoller({ fetch: (signal) => getCollectValues(signal) });
	let pageVisible = $state(true);
	const POLL_KEY = 0;

	const valuesWanted = $derived(
		available &&
			isCollectAvailable() &&
			pageVisible &&
			draftReady &&
			activeTab === 'pens' &&
			(pickMode !== null || draft.pens.some((pen) => pen.tagId !== null))
	);

	$effect(() => {
		const wanted = valuesWanted;
		untrack(() => {
			if (wanted) valuesPoller.start(POLL_KEY, MONITOR_POLL_DEFAULT_MS, { fresh: true });
			else valuesPoller.stop();
		});
	});

	const polledValues = $derived(
		valuesWanted && valuesPoller.key === POLL_KEY ? valuesPoller.state : null
	);
	/** 値の表（読めていなければ `null` = 各行は「—」）。 */
	const valuesTable = $derived(polledValues?.values ?? null);
	/** 値が出せない理由（一か所にだけ出す）。 */
	const valuesNoteText = $derived(
		polledValues === null
			? null
			: valuesNote(true, polledValues, valuesPoller.stale, collectTimeLabel)
	);

	function onVisibilityChange(): void {
		pageVisible = document.visibilityState === 'visible';
		if (pageVisible) void refreshCollectState();
		else clearStatusTimer();
	}

	// --- 操作の状態（#1198 PR3。編集中 → 保存済み・未反映 → 記録中）---
	//
	// 収集の状態（`GET /api/collect`）は、開いたとき・保存が成功した後・「記録へ反映」の後・
	// タブが見えるようになったときに加え、見えている間は定期的に（`starting` は 1 秒、それ以外は
	// 5 秒。`nextStatusRecheckMs`）取り直す。別クライアントの停止・開始に追従するため。タイマは
	// `statusTimer` の 1 本だけで、`refreshCollectState` の最後に次の 1 回を予約する（見えなく
	// なったら・離れたら止める）。遅れて返った応答が新しい状態を巻き戻さないよう世代番号で捨てる。取れなかったら `null`（= 不明）にして、
	// 「未反映」と誤って言わない。
	let collectState = $state<CollectorStateView | null>(null);
	/**
	 * 走っている収集の接続ごとのシミュレーション（`conn:<id>` → `simulation`）。走っている
	 * 収集が起動時に使った値で、シミュレーションの接続のタグは値が記録されない。取れていない
	 * （走っていない・取得失敗）ときは `null`。レジストリの接続の `simulation` では代えない:
	 * 保存値と実行状態は再起動まで食い違う（運転中にオフにして保存しても、走っている収集は
	 * シミュレーションのまま）ので、取れないときは「記録中」と断定せず `targetUnknown` にする。
	 */
	let runningSimulation = $state<ReadonlyMap<string, boolean> | null>(null);
	let collectGen = 0;
	// 離れた後に、待っていた操作の続きが取り直しとタイマを張り直さないための印（#1219 のレビュー）。
	let disposed = false;
	let reflecting = $state(false);
	let reflectNotice = $state<string | null>(null);
	let reflectError = $state<string | null>(null);
	/** 反映後などの 1 回きりの取り直し。 */
	let followUpTimer: ReturnType<typeof setTimeout> | null = null;
	/** 定期の取り直し（常に 1 本だけ）。 */
	let statusTimer: ReturnType<typeof setTimeout> | null = null;
	let startingRechecks = 0;

	function clearStatusTimer(): void {
		if (statusTimer !== null) clearTimeout(statusTimer);
		statusTimer = null;
	}

	/** 今回の取得が最新として採用され `collectState` に書かれたときだけ `true`。 */
	async function refreshCollectState(): Promise<boolean> {
		if (disposed || !available || !isCollectAvailable()) return false;
		const gen = ++collectGen;
		let next: CollectorStateView | null = null;
		try {
			const outcome = await runWithLimit(
				(signal) => getCollectStatus(signal),
				COLLECT_READ_TIMEOUT_MS
			);
			if (outcome.kind === 'ok') next = outcome.value;
		} catch {
			next = null;
		}
		let sim: Map<string, boolean> | null = null;
		if (next?.state === 'running') {
			try {
				const outcome = await runWithLimit(
					(signal) => getCollectConnections(signal),
					COLLECT_READ_TIMEOUT_MS
				);
				if (outcome.kind === 'ok' && outcome.value.state === 'ready') {
					sim = new Map(
						Object.entries(outcome.value.data).map(([key, view]) => [key, view.simulation])
					);
				}
			} catch {
				sim = null;
			}
		}
		if (disposed || gen !== collectGen) return false;
		collectState = next;
		runningSimulation = sim;
		// `starting` のまま止まらないよう上限まで 1 秒おき、それ以外は 5 秒おきに取り直す。
		const wait = nextStatusRecheckMs(next, startingRechecks, pageVisible);
		if (next?.state === 'starting') {
			if (wait !== null && wait < STATUS_RECHECK_MS) startingRechecks++;
		} else {
			startingRechecks = 0;
		}
		clearStatusTimer();
		if (wait !== null) statusTimer = setTimeout(() => void refreshCollectState(), wait);
		return true;
	}

	const baselineTagIds = $derived(
		new Set(baseline.pens.flatMap((pen) => (pen.tagId === null ? [] : [pen.tagId])))
	);
	const groupsById = $derived(new Map(collectionGroups.map((g) => [g.id, g])));
	const connectionsById = $derived(new Map(connections.map((c) => [c.id, c])));

	/**
	 * 反映状態の判定に渡すタグ。収集グループや接続が無効なら、タグが有効でも収集の対象に
	 * 入らず、再起動しても入らないので「無効」として扱う（「未反映」と言って「記録へ反映」を
	 * 出さない）。
	 */
	function applyTagOf(
		tagId: number
	): { enabled: boolean; collectionGroupId: number; simulated: boolean | null } | undefined {
		const tag = tagsById.get(tagId);
		if (tag === undefined) return undefined;
		const group = groupsById.get(tag.collectionGroupId);
		const connection = group && connectionsById.get(group.plcConnectionId);
		const enabled = tag.enabled && (group?.enabled ?? true) && (connection?.enabled ?? true);
		const connKey = group === undefined ? null : `conn:${group.plcConnectionId}`;
		// 走っている収集の接続の状態が分からないときは `null`（レジストリの値で代えない）。
		const simulated = (connKey === null ? undefined : runningSimulation?.get(connKey)) ?? null;
		return { enabled, collectionGroupId: tag.collectionGroupId, simulated };
	}

	/** `draft.pens` と同じ並びのペンごとの反映状態。 */
	const penApplyStates = $derived(
		draft.pens.map((pen) =>
			penApplyState(
				pen,
				isPenUnsaved(pen, baselineTagIds),
				pen.tagId === null ? undefined : applyTagOf(pen.tagId),
				collectState
			)
		)
	);
	const band = $derived(
		applyBand({ dirty, collect: collectState, states: penApplyStates, canWrite, busy: reflecting })
	);

	async function reflectToRecording(): Promise<void> {
		const shown = band.reflect;
		if (shown === null || shown.disabled || reflecting) return;
		reflecting = true; // 取り直しの間も二重押しを防ぐ。
		reflectNotice = null;
		reflectError = null;
		try {
			// 確認の前に前提（収集の状態）を取り直す（`reflecting` の間は `reflect.disabled` が立つので見ない）。別クライアントで変わっていたら確認を出さない。
			const adopted = await refreshCollectState();
			const reflect = band.reflect;
			const gate = reflectGate(adopted, disposed, shown, reflect, dirty);
			if (gate === 'abort') return;
			if (gate === 'unconfirmed') {
				reflectNotice =
					'収集の状態を確かめられませんでした。表示を確認してからもう一度押してください。';
				return;
			}
			if (gate === 'changed' || reflect === null) {
				reflectNotice = '収集の状態が変わりました。表示を確認してからもう一度押してください。';
				return;
			}
			if (!window.confirm(reflect.confirm)) return;
			await runReflect(reflect.action);
		} finally {
			reflecting = false;
		}
	}

	async function runReflect(action: 'start' | 'restart'): Promise<void> {
		collectGen++; // 操作中に飛んでいた取得の応答は、もう古い。
		{
			const outcome = await runWithLimit(
				(signal) => runCollectAction(action, signal),
				COLLECT_UI_TIMEOUT_MS
			);
			if (disposed) return; // 離れた後は状態も通知も書かず、取り直しも予約しない。
			const display = collectOperationDisplay(
				action,
				outcome,
				outcome.kind === 'failed' ? errorMessage(outcome.error) : null
			);
			reflectNotice = display.notice;
			reflectError = display.error;
			if (outcome.kind === 'ok') {
				collectGen++;
				collectState = toCollectStateView(outcome.value.status);
			}
			await refreshCollectState();
			if (disposed) return;
			// 受け付けただけ・打ち切りのときは、終わった頃にもう一度だけ取り直す。
			if (outcome.kind === 'timedOut' || (outcome.kind === 'ok' && outcome.value.pending)) {
				if (followUpTimer !== null) clearTimeout(followUpTimer);
				followUpTimer = setTimeout(() => {
					if (!disposed) void refreshCollectState();
				}, 3000);
			}
		}
	}

	onMount(() => {
		pageVisible = document.visibilityState === 'visible';
		document.addEventListener('visibilitychange', onVisibilityChange);
		void refreshCollectState();
	});

	onDestroy(() => {
		disposed = true;
		valuesPoller.stop();
		collectGen++;
		if (followUpTimer !== null) clearTimeout(followUpTimer);
		clearStatusTimer();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	});

	/** 右ペインを出すか（編集者以上・「ペン」タブ・下書きの用意ができていて、タグ選択かペンの設定）。 */
	const penPaneOpen = $derived(
		canWrite &&
			draftReady &&
			activeTab === 'pens' &&
			(pickMode === 'add' || (selectedPen !== null && selectedPen < draft.pens.length))
	);

	function openRightDrawerIfNarrow(): void {
		if (layoutNarrow) {
			listOpen = false;
			rightDrawerOpen = true;
		}
	}

	function openPen(index: number): void {
		if (!canWrite) return;
		selectedPen = index;
		pickMode = null;
		openRightDrawerIfNarrow();
	}

	function closeRightPane(): void {
		selectedPen = null;
		pickMode = null;
		rightDrawerOpen = false;
	}

	/** 「タグを追加」: 右ペインをタグ選択（追加）にする。元に選んでいたペンは保つ。 */
	function openTagPicker(): void {
		if (!canWrite) return;
		pickMode = 'add';
		if (!hubRequested) {
			hubRequested = true;
			void reloadHubCatalog();
		}
		openRightDrawerIfNarrow();
	}

	/** 「タグを選び直す」: 選んでいるペンのタグを選び直すタグ選択にする。 */
	function startReplace(): void {
		if (selectedPen === null) return;
		pickMode = 'replace';
		if (!hubRequested) {
			hubRequested = true;
			void reloadHubCatalog();
		}
	}

	/** 「戻る」: ペンを選んでいたらそのペンの設定へ、無ければ右ペインを閉じる。 */
	function backFromPicker(): void {
		if (pickMode === 'add' && selectedPen === null) closeRightPane();
		else pickMode = null;
	}

	function handlePick(rows: PickerRow[]): void {
		const picks = rows.map(penFromRow);
		if (pickMode === 'add') {
			draft = appendPens(draft, picks, fallbackTarget);
		} else if (pickMode === 'replace' && selectedPen !== null && picks.length === 1) {
			draft = replacePenTag(draft, selectedPen, picks[0], fallbackTarget);
			pickMode = null;
		}
	}

	/** タグ選択の追加先（固定の見出し）。 */
	const pickTargetName = $derived(
		selected === 'new' ? '新しい表示グループ' : (selectedGroup?.name ?? draft.name)
	);

	function movePenAt(index: number, delta: -1 | 1): void {
		draft = movePen(draft, index, delta);
		// 右ペインで開いているペンは、並べ替えても同じペンを指し続ける。
		if (selectedPen === index) selectedPen = index + delta;
		else if (selectedPen === index + delta) selectedPen = index;
	}

	function removePenAt(index: number): void {
		draft = removePen(draft, index);
		if (selectedPen === index) closeRightPane();
		else if (selectedPen !== null && selectedPen > index) selectedPen -= 1;
	}

	/** 右ペインの見出し（と `ThreePaneLayout` の右ペインの名前）。 */
	const rightPaneTitle = $derived(
		pickMode !== null
			? 'タグ選択'
			: selectedPen === null
				? 'ペン'
				: `ペン ${selectedPen + 1} の設定`
	);

	// --- 画面の状態の切り替えと URL ---------------------------------------------
	//
	// 「記録」ページ（`tags/+page.svelte`）と同じ作り:
	// - 状態の持ち主は `view`。ユーザーの操作は `requestView`（確認 → 状態を変える →
	//   URL を `goto` で追従）、戻る・進む・再読み込みは下の `$effect`（URL → 状態）。
	//   ページ内の `goto` が終わるまでその `$effect` を止める（`viewNavsInFlight`）。
	// - 未保存の確認は `confirmViewChange`: **別のグループへ移るときだけ**（タブの
	//   切り替えは同じ下書きのままなので確かめない）。キャンセルなら何も変えない。
	// - ページの中の移動では汎用の離脱の確認（`guardUnsavedChanges`）を出さない
	//   （guard に `isSamePage: isSamePath` を渡す。クエリだけの移動は同じページ）。
	//   戻る・進むは `handleBeforeNavigate`（`beforeNavigate`）で確かめる。
	// - グループ・タブを変えると右ペインは閉じる（右ペインの中身は前のグループの
	//   ペンだったため）。ただし作成が成功して URL を作ったグループへ置き換える
	//   `forceView` は閉じない（同じ下書きが続くため）。

	/** 未保存の下書きを捨てて切り替えてよいかの確認文。 */
	const DISCARD_CONFIRM = '保存していない変更があります。変更を破棄して切り替えますか？';

	/**
	 * 画面の状態を変えてよいか確かめる（キャンセルなら `false`。そのときは何も
	 * 変えない）。保存中は変えさせない。
	 */
	function confirmViewChange(next: GroupsView): boolean {
		if (sameGroupsView(view, next)) return true;
		if (busy) return false;
		if (view.group !== next.group && dirty && !window.confirm(DISCARD_CONFIRM)) return false;
		closeRightPane();
		return true;
	}

	/** ページ内の `goto`（`requestView`・URL の正規化）の行き先。`beforeNavigate` は確認を飛ばす。 */
	const viewNavHrefs = new Set<string>();
	/** 進行中のページ内の `goto` の数。0 になったら URL から状態を決め直す。 */
	let viewNavsInFlight = $state(0);

	/** 画面の状態を `next` にする。未保存の確認でキャンセルされたら `false`。 */
	function requestView(next: GroupsView): boolean {
		if (sameGroupsView(view, next)) return true;
		if (!confirmViewChange(next)) return false;
		applyView(next);
		void navigateToView(next, { replace: false });
		return true;
	}

	/** 確認なしで画面の状態を変える（削除・作成の成功の後。右ペインはここでは触らない）。 */
	function forceView(next: GroupsView, options: { replace?: boolean } = {}): void {
		applyView(next);
		void navigateToView(next, { replace: options.replace === true });
	}

	async function navigateToView(next: GroupsView, options: { replace: boolean }): Promise<void> {
		const url = new URL(page.url.href);
		url.search = groupsViewSearch(next);
		// 先の `goto` がまだ終わっていない間は `page.url` が古いので、同じに見えても省かない。
		if (url.href === page.url.href && viewNavsInFlight === 0) return;
		const href = url.href;
		viewNavHrefs.add(href);
		viewNavsInFlight += 1;
		try {
			// `reset: false`: フォーカスとスクロールを動かさない（同じページの中の切り替え）。
			await goto(url, { replace: options.replace, reset: false });
		} catch {
			// 失敗しても、下の `$effect` がその時点の URL へ状態を戻す。
		} finally {
			viewNavHrefs.delete(href);
			viewNavsInFlight -= 1;
		}
	}

	/**
	 * URL から画面の状態を決める（戻る・進む・再読み込み・リンク、とページ内の
	 * `goto` が取り消された場合）。一覧を読み終えたら、読めない値・一覧に無い id は
	 * 未選択へ倒し、URL も正規化した形へ置き換える（`replace`）。案内は URL から
	 * 来たグループが見つからないときだけ（一度は見つかったグループが消えたときは出さない）。
	 */
	let lastFoundGroup: GroupsSelection = null;
	$effect(() => {
		const search = page.url.search;
		const loaded = groupsLoaded;
		const rows = groups ?? [];
		const canCreate = canWrite;
		if (viewNavsInFlight > 0) return;
		const parsed = parseGroupsView(new URLSearchParams(search));
		const resolved = resolveGroupsView(parsed, rows, { loaded, canCreate });
		untrack(() => {
			if (loaded && !resolved.missingGroup) lastFoundGroup = resolved.view.group;
			const wasFound = typeof parsed.group === 'number' && parsed.group === lastFoundGroup;
			if (loaded && resolved.missingGroup && !wasFound) {
				toastStore.push('info', MISSING_GROUP_MESSAGE);
			}
			if (!sameGroupsView(view, resolved.view)) {
				closeRightPane();
				applyView(resolved.view);
			}
			if (loaded && search !== groupsViewSearch(resolved.view)) {
				void navigateToView(resolved.view, { replace: true });
			}
		});
	});

	/**
	 * ページの `beforeNavigate`。戻る・進むなどでグループ・タブが変わるときの
	 * 未保存の確認（`confirmViewChange`）をする。
	 */
	function handleBeforeNavigate(nav: BeforeNavigate): void {
		const to = nav.to?.url;
		if (!to) return;
		// ページ内の `goto`: 呼ぶ前に `requestView` などが確かめ済み。
		if (viewNavHrefs.has(to.href)) return;
		if (!nav.willUnload && to.pathname === page.url.pathname) {
			const next = resolveGroupsView(parseGroupsView(to.searchParams), groups ?? [], {
				loaded: groupsLoaded,
				canCreate: canWrite
			}).view;
			if (!confirmViewChange(next)) nav.cancel();
		}
		// 別のページ・再読み込みは guard に任せる。
	}
	beforeNavigate(handleBeforeNavigate);

	// 未保存の入力の確認（banto #214 の流儀、ChronoGazer #508）: 編集中の下書きが
	// 読み込み時と違えば、別のページへ移る・再読み込みの前に確かめる。
	guardUnsavedChanges({
		isDirty: () => dirty,
		isSaving: () => saving || deleting,
		isSamePage: isSamePath
	});

	/** 欄のエラーがあるタブ（保存の帯の要約用。`fieldErrors` から導出する）。 */
	const errorTabs = $derived(fieldErrorTabs(fieldErrors));

	function setActiveTab(id: string | undefined): void {
		requestView({ group: view.group, tab: id === 'settings' ? 'settings' : 'pens' });
	}

	function selectGroup(group: DisplayGroup): void {
		const moved = requestView({ group: group.id, tab: view.tab });
		// 狭幅では、選んだら一覧のドロワーを閉じて中央を見せる。
		if (moved && layoutNarrow) listOpen = false;
	}

	function startNew(): void {
		if (busy || !canWrite || atGroupLimit || groups === null) return;
		const moved = requestView({ group: 'new', tab: 'settings' });
		if (moved && layoutNarrow) listOpen = false;
	}

	function cancelNew(): void {
		requestView({ group: null, tab: 'pens' });
	}

	/** 一覧を閉じていても何を開いているか分かるよう、枠の帯に出す名前。 */
	const listSelectionLabel = $derived.by((): string => {
		if (selected === 'new') return '新しい表示グループ';
		return selectedGroup?.name ?? '表示グループ';
	});

	/** 左ペインとそのトグルの名前。 */
	const LIST_PANE_LABEL = '表示グループ';

	/** 中央のタブ。 */
	const viewTabs = [
		{ id: 'pens', label: 'ペン' },
		{ id: 'settings', label: '設定' }
	];

	// 層の約束（`@banto/ui` の `layering.ts`）: 下位の層（枠の中のドロワー）を開くなら
	// 上位の層（サイドバー）を先に畳む（「記録」ページと同じ。開いた遷移で畳む）。
	$effect(() => {
		if (!listOpen) return;
		untrack(() => {
			if (mobileNavStore.open) mobileNavStore.closeNav();
		});
	});

	$effect(() => {
		if (!available) return;
		void reloadGroups();
		void reloadTags();
	});
</script>

<div class="page">
	<h2>表示グループ</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはDBが無いため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<!--
			#1179 PR6a: 右ペイン = 選んだペンの設定（タグの選択と色）。編集者以上の
			ときだけ開く。ペンの編集は中央の下書きを直接書き換える（保存は中央の上）。
		-->
		{#snippet rightPaneContent()}
			<div class="detail-pane" data-testid="display-group-pen-pane">
				<div class="detail-pane-header">
					<h3>{rightPaneTitle}</h3>
					<button type="button" class="pane-close" onclick={closeRightPane} aria-label="閉じる">
						×
					</button>
				</div>
				<div class="detail-pane-body">
					{#if pickMode !== null}
						<PenTagPicker
							mode={pickMode}
							title={pickMode === 'add'
								? `タグ追加中・追加先: ${pickTargetName}`
								: `ペン ${(selectedPen ?? 0) + 1} のタグを選び直す`}
							{connections}
							{collectionGroups}
							tags={tags ?? []}
							hub={hubCatalog}
							{hubLoading}
							{hubError}
							used={tagsUsedByOtherPens(draft, pickMode === 'replace' ? selectedPen : null)}
							remaining={remainingPens(draft)}
							max={MAX_PENS}
							values={valuesTable}
							valuesNote={valuesNoteText}
							disabled={busy}
							onpick={handlePick}
							onback={backFromPicker}
							onrefresh={() => void refreshPicker()}
						/>
					{:else if selectedPen !== null && selectedPen < draft.pens.length}
						{@const index = selectedPen}
						{@const pen = draft.pens[index]}
						{@const summary = describePenTag(pen, tagsById, collectionGroups, connections)}
						<fieldset class="editor" disabled={busy}>
							<div class="current-tag" data-testid="pen-current-tag">
								<span class="field-label">ペン {index + 1} のタグ</span>
								<strong>{summary?.name ?? 'タグ未選択'}</strong>
								{#if summary?.connection || summary?.group}
									<span class="meta">
										{[summary?.connection, summary?.group].filter(Boolean).join(' / ')}
									</span>
								{/if}
								{#if summary?.unregistered}
									<span class="unreg-badge">未登録（保存で記録に追加）</span>
								{/if}
								<button type="button" class="secondary" onclick={startReplace}>
									タグを選び直す
								</button>
							</div>
							<label class="field">
								<span>ペン {index + 1} の色</span>
								<select
									value={pen.colorSlot === null ? '' : String(pen.colorSlot)}
									onchange={(e) => setPenColor(index, e.currentTarget.value)}
								>
									<option value="">既定（色 {defaultColorSlot(index)}）</option>
									{#each Array.from({ length: COLOR_SLOTS }, (_, i) => i + 1) as colorSlot (colorSlot)}
										<option value={String(colorSlot)}>色 {colorSlot}</option>
									{/each}
								</select>
							</label>
						</fieldset>
						{#each fieldErrors.penRows[index] ?? [] as message, i (i)}
							<p class="field-error">{message}</p>
						{/each}
						<p class="note">変更は下書きに入り、中央の上の「保存」で確定します。</p>
					{/if}
				</div>
			</div>
		{/snippet}

		<!--
			#1179 PR6a: 左の一覧・中央のタブ・右のペイン設定の 3 ペイン（`@banto/ui` の
			`ThreePaneLayout`）。狭幅の判定はビューポート（`mobileNavStore.isNarrow`）。
			帯には、一覧を閉じていても何を開いているか分かるよう、選択中の名前を出す。
		-->
		<div class="groups-frame">
			<ThreePaneLayout
				narrowOverride={mobileNavStore.isNarrow}
				bind:leftDrawerOpen={listOpen}
				bind:rightDrawerOpen
				bind:narrow={layoutNarrow}
				right={penPaneOpen ? rightPaneContent : undefined}
				messages={{
					threePaneLeft: () => LIST_PANE_LABEL,
					threePaneResizeLeft: () => `${LIST_PANE_LABEL}の幅`,
					threePaneRight: () => rightPaneTitle,
					threePaneResizeRight: () => `${rightPaneTitle}の幅`
				}}
			>
				{#snippet left()}
					<div class="list-pane" data-testid="display-group-list-pane">
						{#if canWrite}
							<div class="list-toolbar">
								<button
									type="button"
									class="secondary"
									onclick={startNew}
									disabled={busy || atGroupLimit || groups === null}
								>
									表示グループを追加
								</button>
							</div>
						{/if}
						{#if canWrite && atGroupLimit}
							<p class="note">
								表示グループは {MAX_DISPLAY_GROUPS} 個までです。新しく作るには不要なグループを削除してください。
							</p>
						{/if}

						<section class="group-list-section" aria-label="表示グループの一覧">
							{#if groups === null && groupsError === null}
								<p class="loading">読み込み中…</p>
							{:else if groups === null}
								<p class="load-error" role="alert">
									表示グループの一覧を読み込めませんでした（{groupsError}）。登録が0件という意味ではありません。
									<button type="button" onclick={() => void reloadGroups()} disabled={groupsLoading}
										>再試行</button
									>
								</p>
							{:else}
								{#if groupsError !== null}
									<p class="load-error" role="alert">
										一覧を更新できませんでした（{groupsError}）。表示は最後に読み込めた内容です。
										<button
											type="button"
											onclick={() => void reloadGroups()}
											disabled={groupsLoading}>再試行</button
										>
									</p>
								{/if}
								{#if groups.length === 0}
									<p class="note">表示グループはまだありません。</p>
								{:else}
									<ul class="group-list">
										{#each groups as group, index (group.id)}
											<li class:selected={selected === group.id}>
												<button
													type="button"
													class="group-open"
													aria-current={selected === group.id ? 'true' : undefined}
													onclick={() => selectGroup(group)}
													disabled={busy}
												>
													<span class="group-name">{group.name}</span>
													<span class="group-meta"
														>{kindLabel(group.kind)}・ペン {group.pens.length} 本</span
													>
												</button>
												{#if canWrite}
													<button
														type="button"
														class="icon"
														aria-label={`${group.name} を上へ`}
														onclick={() => void moveGroup(group.id, -1)}
														disabled={busy || index === 0}
													>
														↑
													</button>
													<button
														type="button"
														class="icon"
														aria-label={`${group.name} を下へ`}
														onclick={() => void moveGroup(group.id, 1)}
														disabled={busy || index === groups.length - 1}
													>
														↓
													</button>
												{/if}
											</li>
										{/each}
									</ul>
								{/if}
							{/if}
						</section>
					</div>
				{/snippet}
				{#snippet titleContent()}
					<span class="list-selection" data-testid="display-group-selection">
						{listSelectionLabel}
					</span>
				{/snippet}

				<div class="center-pane" data-testid="display-group-center">
					{#if selected === null}
						<p class="note">
							{canWrite
								? '左の一覧から表示グループを選ぶか、「表示グループを追加」で作ります。'
								: '左の一覧から表示グループを選んでください。'}
						</p>
					{:else if !draftReady}
						<p class="loading">読み込み中…</p>
					{:else}
						<!--
							保存・取り消し・未保存の表示・楽観ロックの案内は、どちらのタブでも見える
							ようタブの外（中央の上）に置く。下書きはグループ単位で一つ。
						-->
						<div class="edit-bar">
							<div class="edit-bar-head">
								<h3>
									{selected === 'new' ? '新しい表示グループ' : (selectedGroup?.name ?? '')}
								</h3>
								<UnsavedChangesNotice pending={dirty} label={UNSAVED_NOTICE} />
							</div>

							{#if fieldErrors.revision.length > 0}
								<p class="load-error" role="alert">
									{fieldErrors.revision.join(' / ')}
									<button type="button" onclick={() => void reloadLatest()} disabled={busy}>
										{reloadingLatest ? '読み込み中…' : '最新の内容を読み込む（入力を破棄）'}
									</button>
								</p>
								{#if latestReloadError !== null}
									<p class="load-error" role="alert">
										最新の内容を読み込めませんでした（{latestReloadError}）。入力はそのまま残っています。
									</p>
								{/if}
							{/if}

							{#if errorTabs.settings || errorTabs.pens}
								<p class="load-error" role="alert" data-testid="display-group-error-tabs">
									保存できませんでした。
									{#if errorTabs.settings}
										<button type="button" onclick={() => setActiveTab('settings')}>設定</button>
									{/if}
									{#if errorTabs.settings && errorTabs.pens}と{/if}
									{#if errorTabs.pens}
										<button type="button" onclick={() => setActiveTab('pens')}>ペン</button>
									{/if}
									タブの入力を確かめてください
								</p>
							{/if}

							<div class="apply-band" data-testid="apply-band" data-kind={band.kind}>
								<span class="apply-text" role="status">{band.text}</span>
								{#if band.reflect !== null}
									<button
										type="button"
										data-testid="apply-reflect"
										onclick={() => void reflectToRecording()}
										disabled={band.reflect.disabled}
										title={band.reflect.title ?? undefined}
									>
										{band.reflect.label}
									</button>
								{/if}
							</div>
							{#if reflectNotice}
								<p class="note" role="status">{reflectNotice}</p>
							{/if}
							{#if reflectError}
								<p class="load-error" role="alert">{reflectError}</p>
							{/if}

							<div class="actions">
								{#if canWrite}
									<button type="button" class="primary" onclick={() => void save()} disabled={busy}>
										{selected === 'new' ? '作成' : '保存'}
									</button>
									<button type="button" onclick={discardChanges} disabled={busy || !dirty}>
										{UNSAVED_DISCARD}
									</button>
									{#if selected === 'new'}
										<button type="button" onclick={cancelNew} disabled={busy}>やめる</button>
									{/if}
								{:else}
									<p class="note">閲覧のみ（編集には編集者以上の権限が必要です）。</p>
								{/if}
							</div>
						</div>

						<div class="view-tabs">
							<Tabs
								tabs={viewTabs}
								bind:value={() => activeTab, setActiveTab}
								label="表示の切り替え"
							>
								{#snippet panel(id)}
									{#if id === 'pens'}
										<div class="pens">
											<div class="pens-head">
												<span class="pens-title">ペン（{draft.pens.length} / {MAX_PENS}）</span>
												{#if canWrite}
													<button
														type="button"
														class="secondary"
														data-testid="display-group-add-tag"
														onclick={openTagPicker}
														disabled={busy || tags === null}
													>
														タグを追加
													</button>
												{/if}
											</div>
											{#if tagsError !== null}
												<p class="load-error" role="alert">
													タグの一覧を読み込めませんでした（{tagsError}）。
													<button type="button" onclick={() => void reloadTags()}>再試行</button>
												</p>
											{/if}
											{#each fieldErrors.pens as message, i (i)}
												<p class="field-error">{message}</p>
											{/each}
											{#if valuesNoteText !== null}
												<p class="note values-note" data-testid="pen-values-note" role="status">
													{valuesNoteText}
												</p>
											{/if}
											{#if draft.pens.length === 0}
												<p class="note">
													ペンがありません。{canWrite ? '「タグを追加」でタグを割り当てます。' : ''}
												</p>
											{:else}
												<ol class="pen-list">
													{#each draft.pens as pen, index (index)}
														{@const slot = effectiveColorSlot(pen, index)}
														{#snippet penSummary()}
															<span
																class="swatch"
																style:background={colorSlotVar(slot)}
																aria-hidden="true"
															></span>
															<span class="pen-number">ペン {index + 1}</span>
															<span class="pen-tag">
																{pen.hubTag !== null
																	? pen.hubTag.name
																	: pen.tagId === null
																		? 'タグ未選択'
																		: tagLabel(pen.tagId)}
																{#if pen.hubTag !== null}
																	<span class="unreg-badge">未登録（保存で記録に追加）</span>
																{/if}
																{#if penApplyStates[index]?.label}
																	<span
																		class="apply-badge"
																		data-testid="pen-apply-state"
																		data-kind={penApplyStates[index].kind}
																	>
																		{penApplyStates[index].label}
																	</span>
																{/if}
															</span>
															{@const cell = valueCell(
																// 未登録の Hub のタグは隣のバッジが「未登録」を示すので、品質には重ねない。
																pen.hubTag === null && pen.tagId !== null
																	? { kind: 'tag', tagId: pen.tagId }
																	: { kind: 'none' },
																tagsById,
																valuesTable
															)}
															<span
																class="pen-value"
																data-testid="pen-value"
																data-state={cell.state}
															>
																<span class="pen-value-text">{cell.text}</span>
																{#if cell.quality !== null}
																	<span class="pen-quality">{cell.quality}</span>
																{/if}
															</span>
															<span
																class="thresholds"
																title="しきい値は「記録」ページの「しきい値（記録計の設定）」で変更します"
															>
																しきい値: {penThresholdText(
																	pen.tagId,
																	pen.tagId !== null && tagsById.has(pen.tagId),
																	thresholdsByTag,
																	thresholdsError !== null
																)}
															</span>
														{/snippet}
														<li class="pen" class:selected={penPaneOpen && selectedPen === index}>
															{#if canWrite}
																<button
																	type="button"
																	class="pen-open"
																	data-testid={`display-group-pen-${index + 1}`}
																	aria-current={penPaneOpen && selectedPen === index
																		? 'true'
																		: undefined}
																	onclick={() => openPen(index)}
																>
																	{@render penSummary()}
																</button>
																<span class="pen-actions">
																	<button
																		type="button"
																		class="icon"
																		aria-label={`ペン ${index + 1} を上へ`}
																		onclick={() => movePenAt(index, -1)}
																		disabled={busy || index === 0}
																	>
																		↑
																	</button>
																	<button
																		type="button"
																		class="icon"
																		aria-label={`ペン ${index + 1} を下へ`}
																		onclick={() => movePenAt(index, 1)}
																		disabled={busy || index === draft.pens.length - 1}
																	>
																		↓
																	</button>
																	<button
																		type="button"
																		class="icon"
																		aria-label={`ペン ${index + 1} を外す`}
																		onclick={() => removePenAt(index)}
																		disabled={busy}
																	>
																		×
																	</button>
																</span>
															{:else}
																<div class="pen-open static">{@render penSummary()}</div>
															{/if}
															{#each fieldErrors.penRows[index] ?? [] as message, i (i)}
																<span class="field-error pen-error">{message}</span>
															{/each}
														</li>
													{/each}
												</ol>
											{/if}
											{#if canWrite && hasHubPens(draft)}
												{@const target = draft.hubTarget ?? fallbackTarget}
												<fieldset class="hub-target" disabled={busy} data-testid="hub-target">
													<legend>Hub から新しく記録するタグの登録先</legend>
													{#if hubGroups.length > 0}
														<label class="choice">
															<input
																type="radio"
																name="display-group-hub-target"
																checked={target.kind === 'existing'}
																onchange={() => setTargetKind('existing')}
															/>
															既存の Hub の収集グループ
														</label>
														{#if target.kind === 'existing'}
															<select
																aria-label="登録先の収集グループ"
																value={target.collectionGroupId === null
																	? ''
																	: String(target.collectionGroupId)}
																onchange={(e) => setTargetGroup(e.currentTarget.value)}
															>
																{#each hubGroups as hubGroup (hubGroup.id)}
																	<option value={String(hubGroup.id)}>{hubGroup.name}</option>
																{/each}
															</select>
														{/if}
													{/if}
													<label class="choice">
														<input
															type="radio"
															name="display-group-hub-target"
															checked={target.kind === 'new'}
															onchange={() => setTargetKind('new')}
														/>
														新しい収集グループを作る{hubGroups.length === 0
															? '（Hub 接続も作成します）'
															: ''}
													</label>
													{#if target.kind === 'new'}
														<div class="new-group">
															<label class="field">
																<span>名前</span>
																<input
																	type="text"
																	maxlength="100"
																	value={target.name}
																	oninput={(e) => setTargetName(e.currentTarget.value)}
																/>
															</label>
															<label class="field">
																<span>収集周期</span>
																<select
																	value={target.periodMs}
																	onchange={(e) => setTargetPeriod(e.currentTarget.value)}
																>
																	{#each periodOptions as option (option.value)}
																		<option value={option.value}>{option.label}</option>
																	{/each}
																</select>
															</label>
														</div>
													{/if}
													{#each fieldErrors.hubTagGroup as message, i (i)}
														<p class="field-error">{message}</p>
													{/each}
												</fieldset>
											{/if}
											<p class="note">しきい値はタグの設定で、ここでは確認だけできます。</p>
										</div>
									{:else}
										<fieldset class="editor" disabled={!canWrite || busy}>
											<label class="field">
												<span>名前</span>
												<input type="text" maxlength="100" bind:value={draft.name} />
												{#each fieldErrors.name as message, i (i)}
													<span class="field-error">{message}</span>
												{/each}
											</label>

											<label class="field">
												<span>表示種別</span>
												<select
													value={draft.kind}
													onchange={(e) => setKind(e.currentTarget.value as DisplayKind)}
												>
													{#each KIND_OPTIONS as option (option.value)}
														<option value={option.value}>{option.label}</option>
													{/each}
												</select>
												{#each fieldErrors.kind as message, i (i)}
													<span class="field-error">{message}</span>
												{/each}
											</label>

											{#if draft.kind === 'trend'}
												<label class="field">
													<span>既定の時間窓</span>
													<select
														value={String(draft.timeWindowSec)}
														onchange={(e) => (draft.timeWindowSec = Number(e.currentTarget.value))}
													>
														{#each TIME_WINDOW_OPTIONS as option (option.value)}
															<option value={String(option.value)}>{option.label}</option>
														{/each}
													</select>
													{#each fieldErrors.timeWindowSec as message, i (i)}
														<span class="field-error">{message}</span>
													{/each}
												</label>
											{/if}
										</fieldset>
										{#if canWrite && typeof selected === 'number'}
											<div class="actions">
												<button
													type="button"
													class="danger"
													onclick={() => void remove()}
													disabled={busy}
												>
													削除
												</button>
											</div>
										{/if}
									{/if}
								{/snippet}
							</Tabs>
						</div>
					{/if}
				</div>
			</ThreePaneLayout>
		</div>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		height: calc(100vh - var(--banto-shell-header-height) - 2.5rem);
		min-height: 0;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	h3 {
		margin: 0;
		font-size: 0.95rem;
	}

	/* 3 ペインの枠が残りの高さを埋める。 */
	.groups-frame {
		flex: 1;
		min-height: 360px;
		display: flex;
		flex-direction: column;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		overflow: hidden;
	}

	.list-pane {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		height: 100%;
		min-height: 0;
		box-sizing: border-box;
		padding: 0.5rem;
		overflow-y: auto;
	}

	.list-toolbar {
		flex: 0 0 auto;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.list-toolbar button {
		font-size: 0.78rem;
		padding: 0 0.6rem;
	}

	.list-selection {
		font-weight: 600;
	}

	.center-pane {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		height: 100%;
		min-height: 0;
		box-sizing: border-box;
		padding: 0.75rem 1rem;
		overflow-y: auto;
	}

	.edit-bar {
		flex: 0 0 auto;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.edit-bar-head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}

	.edit-bar-head h3 {
		margin-right: auto;
	}

	.view-tabs {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}

	.view-tabs > :global(.banto-tabs__panel:not([hidden])) {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		gap: 0.75rem;
		max-width: 48rem;
	}

	.note {
		margin: 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.load-error {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	button {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		background: transparent;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		cursor: pointer;
	}

	button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	button.primary {
		background: var(--banto-primary-solid);
		border-color: var(--banto-primary-solid);
		color: var(--banto-on-solid);
		font-weight: 600;
	}

	button.danger {
		border-color: var(--banto-danger);
		color: var(--banto-danger);
		font-weight: 600;
	}

	button.icon {
		width: var(--banto-control-height);
		padding: 0;
	}

	.group-list-section {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.group-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.group-list li {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}

	.group-open {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		text-align: left;
	}

	.group-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.group-list li.selected .group-open {
		border-color: var(--banto-primary);
	}

	.group-meta {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
		flex: none;
	}

	.editor {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		border: none;
		margin: 0;
		padding: 0;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-size: 0.85rem;
	}

	.field input,
	.field select {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.5rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
		color: inherit;
		max-width: 24rem;
	}

	.field-error {
		margin: 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.current-tag {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.25rem;
		font-size: 0.85rem;
	}

	.current-tag .meta {
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.unreg-badge {
		color: var(--banto-warning);
		font-size: 0.75rem;
		font-weight: 600;
	}

	.hub-target {
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
		margin: 0;
		padding: 0.5rem 0.75rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		font-size: 0.85rem;
	}

	.hub-target .choice {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.hub-target .new-group {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
	}

	.pens {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.pens-head {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.pens-title {
		font-size: 0.85rem;
		font-weight: 600;
	}

	.pen-list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}

	.pen {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.pen-open {
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: center;
		gap: 0.75rem;
		text-align: left;
	}

	.pen-open.static {
		height: var(--banto-control-height);
		box-sizing: border-box;
		padding: 0 0.75rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-md);
	}

	.pen.selected .pen-open {
		border-color: var(--banto-primary);
	}

	.swatch {
		width: 1rem;
		height: 1rem;
		border-radius: 50%;
		flex: none;
	}

	.pen-number {
		font-weight: 600;
		font-size: 0.85rem;
		flex: none;
	}

	.pen-tag {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pen-value {
		flex: none;
		display: inline-flex;
		align-items: baseline;
		gap: 0.4rem;
		font-variant-numeric: tabular-nums;
	}

	.pen-value-text {
		font-weight: 600;
	}

	.apply-band {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		font-size: 0.85rem;
	}

	.apply-band[data-kind='editing'] .apply-text,
	.apply-band[data-kind='pending'] .apply-text {
		color: var(--banto-warning);
		font-weight: 600;
	}

	.apply-badge {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.apply-badge[data-kind='unsaved'],
	.apply-badge[data-kind='pending'],
	.apply-badge[data-kind='excluded'] {
		color: var(--banto-warning);
		font-weight: 600;
	}

	.pen-quality {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.pen-value[data-state='bad'] .pen-quality,
	.pen-value[data-state='stale'] .pen-quality,
	.pen-value[data-state='invalid'] .pen-quality,
	.pen-value[data-state='unregistered'] .pen-quality {
		color: var(--banto-warning);
	}

	.thresholds {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		flex: none;
	}

	.pen-actions {
		display: flex;
		gap: 0.25rem;
	}

	.pen-error {
		flex-basis: 100%;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.detail-pane {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}

	.detail-pane-header {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.75rem 1rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.detail-pane-header h3 {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pane-close {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		font-size: 1.25rem;
		line-height: 1;
		height: auto;
		padding: 0.15rem 0.4rem;
		cursor: pointer;
		border-radius: var(--banto-radius);
	}

	.pane-close:hover {
		color: var(--banto-text);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.detail-pane-body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.75rem 1rem 1.25rem;
	}
</style>
