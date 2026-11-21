<script lang="ts">
	/**
	 * ライブタグモニタ画面（T10、docs/archive/ux-plan.md §2、2026-08-06 オーナー
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
	 * T18-4a（docs/hub/banto-hub-t18-design.md「T18-4a モニタの Tree/検索統合」、
	 * T13-2 移管）: 当初のフラット一覧 + 接続/グループのプルダウン
	 * フィルタを、タグ登録ページ（`(app)/tags/+page.svelte`）と同じ
	 * 2 ペインの枠 + ConnectionTree + 検索ボックスの Tree/検索 UI へ置換した
	 * （枠は #1179 PR2b で自前の `SplitPane` から `@banto/ui` の `ThreePaneLayout` へ） -
	 * 登録と同じ操作感でモニタ対象を絞り込めるようにする、という目的の
	 * スライス。ツリー・絞り込みは表示専用（`ConnectionTree` の
	 * `oncontextmenu` は渡さない - このページに作成系 UI は無い）。ツリーへ
	 * 渡す `connections`/`groups`/`adminTags` は tagRegistryAdmin.ts の既存
	 * 一覧 API から読むだけの補助データで、値表示自体は従来どおり
	 * catalog（`rows`）+ WS が正。絞り込みロジックは依存ゼロの純関数
	 * `filterMonitorRows`（`#lib/banto/monitorFilter.ts`）に切り出してある。
	 *
	 * T18-4b（docs/hub/banto-hub-t18-design.md「T18-4b 選択購読と再接続堅牢化」、
	 * TAG-UX-H の一部）: T18-4a まで WS 購読（`connectTagStream`）は常に
	 * 全タグ購読（`tags:['*']`）で、ツリー選択は表示絞り込み
	 * （`filterMonitorRows`）にしか使っていなかった。ここから購読自体も
	 * ツリー選択に合わせて絞る - `monitorSubscription.ts::subscriptionPatternsFor`
	 * が `treeFilter`/`connections`/`groups` からサーバー向けパターン文字列を
	 * 組み立て、`connectTagStream` の第2引数（`getSubscriptionTags`）として
	 * 渡す。加えて、①WS バックプレッシャ切断（close code 1013）を通常の
	 * 再接続と区別して表示し、②再接続時は初期スナップショット受信まで
	 * 「現在値」を古いまま出さず stale 扱いする（`awaitingSnapshot`）。
	 * どちらも `row.v`/`row.q`/`row.t` 自体は書き換えず、表示用ヘルパー
	 * `monitorStreamView.ts` の `monitorCellDisplay` で包むだけ - 再接続後の初期
	 * スナップショットが届けば通常どおり上書きされる。
	 *
	 * 権限: relay-wright のモニタは書き込みセルを editor 以上に限定するが、
	 * この画面には書き込み要素が無いため viewer を含む全ロールが無条件で
	 * 閲覧できる（ゲートすべき対象が無い）。ツリー・検索も同様に読み取り
	 * 専用の絞り込みでしかないため、権限ゲートは追加しない。
	 *
	 * #1179 PR3b（docs/hub/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、段階 ①）:
	 * ツリーの選択を URL（`?target=`、形はタグ登録と同じ `#lib/banto/tagsView.ts`）に
	 * 持ち、タグ登録とナビで行き来したときに同じ対象を開く（`#lib/treeSelection.svelte.ts`）。
	 *
	 * #1253（docs/hub/banto-hub-ui-improvement-proposal.md §ドック型の配置）: 枠を
	 * `@banto/ui` の `ThreePaneLayout` から `@banto/dock-svelte` の `ThreePaneDock` へ
	 * 載せ替えた（左 = ツリー、中央 = 一覧。右は無し。下のログのパネルは次の PR で
	 * `panels` に足す）。選択中の対象は今までどおり URL（`?target=`）、パネルの配置は
	 * 利用者ごとの保存（`#lib/dockLayoutStore.ts`、キー `monitor`）で URL には載せない。
	 * 部品には開閉の帯が無いので、ツリーの表示・非表示と「表示」メニュー（レイアウトを
	 * 既定に戻す）は中央のツールバーに置く。狭い幅では部品の狭い表示に任せる（ツリーは
	 * 左端の自動で隠す帯へ。帯のタブで引き出し、Esc・外の押下・選択で戻り、フォーカスは
	 * 帯のタブへ戻る。#378・#381 の約束の置き換え）。
	 */
	import { onDestroy, tick, untrack } from 'svelte';
	import { page } from '$app/state';
	import { afterNavigate, goto } from '$app/navigation';
	import { toastStore } from '#lib/toast.svelte.js';
	import { resolveAppPath } from '#lib/navigation.js';
	import { mobileNavStore } from '#lib/mobileNav.svelte.js';
	import {
		getCatalog,
		connectTagStream,
		type CatalogTagEntry,
		type StreamValue
	} from '#lib/banto/tagMonitorAdmin.js';
	import {
		listPlcConnections,
		listCollectionGroups,
		listTags,
		type PlcConnection,
		type CollectionGroup,
		type Tag
	} from '#lib/banto/tagRegistryAdmin.js';
	import { filterMonitorRows, type MonitorTreeFilter } from '#lib/banto/monitorFilter.js';
	import {
		MISSING_TREE_TARGET_MESSAGE,
		parseTreeTarget,
		resolveTreeTarget,
		sameTreeTarget
	} from '#lib/banto/tagsView.js';
	import { monitorViewSearch, parseMonitorFocus } from '#lib/banto/monitorView.js';
	import { sharedTreeTarget } from '#lib/treeSelection.svelte.js';
	import { subscriptionPatternsFor } from '#lib/banto/monitorSubscription.js';
	import {
		applyTagValues,
		replaceOrMergeTagValues,
		type RowValue
	} from '#lib/banto/monitorValues.js';
	import {
		cellDisplayMode,
		initialStreamView,
		monitorCellDisplay,
		monitorColumnLabels,
		streamViewReducer,
		type StreamViewEvent
	} from '#lib/banto/monitorStreamView.js';
	import {
		probeSessionAfterReconnectFailures,
		recheckSessionAfterStreamClose
	} from '#lib/banto/sessionRecheck.js';
	import { Menu, MenuItem, MenuSeparator } from '@banto/ui';
	import { createDockState, defaultDockMessages, ThreePaneDock } from '@banto/dock-svelte';
	import { createDockLayoutStore } from '#lib/dockLayoutStore.js';
	import { persistThreePaneLayout } from '#lib/dockLayoutPersist.svelte.js';
	import { lowScreenLogBand } from '#lib/dockLogBand.svelte.js';
	import ConnectionTree from '#lib/components/ConnectionTree.svelte';
	import LogPanel from '#lib/components/LogPanel.svelte';
	import { createLogFeedState } from '#lib/banto/logFeedState.svelte.js';
	import { isDockPanelOnScreen } from '#lib/dockPanelOnScreen.js';
	import type { ConnectionTreeNodeData } from '#lib/components/connectionTreeTypes.js';
	import { isProviderError } from '@banto/admin-core';

	/** catalog の1タグ + WS から届く最新の現在値。 */
	interface Row extends CatalogTagEntry {
		v: number | null;
		q: string;
		t: number;
		unverified?: boolean;
	}

	const FLASH_MS = 700;

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let rows = $state<Row[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	/**
	 * ストリームの表示状態（接続中か・バックプレッシャ切断（1013）か・
	 * 最初の値の待ち（T18-4b）・close `1008` で止まっている理由（#441））。
	 * 出来事を `streamViewReducer`（`monitorStreamView.ts`、vitest で表に
	 * して確認）で畳む。表のセルの表示は `cellDisplayMode` と
	 * `monitorCellDisplay` で決め、`row.v`/`q`/`t` 自体は書き換えない。
	 */
	let streamView = $state(initialStreamView());
	const displayMode = $derived(cellDisplayMode(streamView));
	const columnLabels = $derived(monitorColumnLabels(displayMode));
	let streamResume: (() => void) | null = null;

	/** 出来事を状態へ畳む。`$effect` の中から呼んでも `streamView` を依存に
	 * 加えない（読み書きする effect が自分を起こし続けないように）。 */
	function dispatchStreamView(event: StreamViewEvent): void {
		untrack(() => {
			streamView = streamViewReducer(streamView, event);
		});
	}

	/** 直近で値/品質が変化した外部名（一時的なハイライト表示用）。 */
	let flashing = $state<Record<string, boolean>>({});

	function triggerFlash(externalName: string): void {
		flashing[externalName] = true;
		setTimeout(() => {
			delete flashing[externalName];
		}, FLASH_MS);
	}

	/**
	 * WS `data` から届いた最新値のキャッシュ（`external_name` → 値）。
	 *
	 * **2026-08-31 実機診断で特定した不具合の修正の核心**
	 * （`#lib/banto/monitorValues.ts` 冒頭 doc comment に詳細）: 下の
	 * 初期化 `$effect` は `reloadCatalog()`（catalog の HTTP 取得、非同期・
	 * 未 await）を呼んだ直後に `connectTagStream()`（WS 接続、即座に
	 * `subscribe` して初期スナップショットを受ける）を呼ぶ。実機の速度では
	 * WS の初期スナップショットの方が catalog の HTTP 応答より先に届く
	 * レースが常態的に発生し、旧実装はその場の（まだ空の）`rows` にしか
	 * 突き合わせていなかったため、先着した値がそのまま失われていた。
	 * PLC の値が全て `0` で変化しない実機環境では `mode: 'on_change'` に
	 * よりそれ以降の配信が無く、一度の取りこぼしがそのまま「値が永久に
	 * 反映されない」不具合になっていた。
	 *
	 * 修正: 受け取った値を `rows` から独立したこのマップへ蓄積し、
	 * catalog 再構築時（`toRow`）は必ずこのマップを見る - catalog と WS
	 * のどちらが先に終わっても、両方終わった時点で必ず値が反映される
	 * （$state にする必要はない - このマップ自体を直接描画に使わず、
	 * 常に `rows` への反映を経由するため）。 */
	let latestValues = new Map<string, RowValue>();

	function toRow(entry: CatalogTagEntry, previous?: Row): Row {
		const cached = latestValues.get(entry.external_name);
		return {
			...entry,
			v: cached?.v ?? previous?.v ?? null,
			q: cached?.q ?? previous?.q ?? 'stale',
			t: cached?.t ?? previous?.t ?? 0,
			unverified: cached ? !!cached.unverified : !!previous?.unverified
		};
	}

	/** catalog を（再）取得し、既存の生値（v/q/t）または WS 到着済みの
	 * 最新値（`latestValues`、上の doc comment 参照）を反映しつつ行を
	 * 作り直す。新規タグは追加され、消えたタグは自然に脱落する
	 * （`catalog.tags` をそのまま元に組み立てるため）。 */
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
	/**
	 * #381 レビュー対応19回目: 補助データ（接続/グループ/タグ）を**一度でも
	 * 読み終えたか**。`resolveTreeTarget`（`pruneTreeFilter`）へ渡す（一覧が空か
	 * どうかから「未ロード」を推測しない - 同ファイルの doc 参照）。失敗時は立てない。
	 */
	let adminLoaded = $state(false);

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
			adminLoaded = true;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	/**
	 * WS `data` の `values` を `latestValues`（上の doc comment 参照）へ
	 * 畳み込んだ上で `rows` に反映する。`latestValues` を経由することで、
	 * この時点で `rows` がまだ空（catalog 未取得）でも値を失わない -
	 * 後から `reloadCatalog`/`toRow` が同じマップを見て埋める。
	 */
	function applyStreamData(values: StreamValue[], snapshot: boolean): void {
		if (values.length === 0 && !snapshot) return;
		latestValues = replaceOrMergeTagValues(latestValues, values, snapshot);
		const existingNames = new Set(rows.map((r) => r.external_name));
		// スナップショットは置き換え: 載っていない行は値なし（`toRow` の既定と同じ形）にする。
		const base = snapshot
			? rows.map((r) =>
					latestValues.has(r.external_name) || (r.v === null && r.q === 'stale' && r.t === 0)
						? r
						: { ...r, v: null, q: 'stale', t: 0, unverified: false }
				)
			: rows;
		rows = applyTagValues(base, latestValues);
		// フラッシュ表示は「今回のメッセージで届いた」タグに限る（過去分も
		// 含む latestValues 全体ではなく、あくまで今回の差分に対して一瞬
		// ハイライトする、という従来どおりの挙動を維持する）。
		for (const value of values) {
			if (existingNames.has(value.tag)) triggerFlash(value.tag);
		}
	}

	// --- T18-4a: ツリー選択 + 検索（クライアント側のみ・再取得なし） --------
	//
	// tags ページの `TreeFilter`/`handleTreeSelect`/`treeSelectedId` と同じ
	// 形（`monitorFilter.ts` 冒頭の doc comment 参照）。`treeFilter` は
	// T18-4b で購読範囲（`subscriptionPatternsFor`）にも使うため、接続を
	// 張る $effect より先に宣言する。

	let treeFilter: MonitorTreeFilter = $state({ type: 'all' });
	let searchQuery = $state('');

	const treeSelectedId = $derived.by((): string => {
		if (treeFilter.type === 'all') return 'all';
		if (treeFilter.type === 'connection') return `conn:${treeFilter.id}`;
		return `group:${treeFilter.id}`;
	});

	/**
	 * #1253: ドックの枠（`ThreePaneDock`）。`dock` を自分で持ち、`layoutSnapshot()` の変化で
	 * 配置を保存する（`layoutSnapshot()` を保存する。狭い表示の間は控えた広い配置を返すので、
	 * 狭い表示の一時の配置は保存しない）。
	 *
	 * 狭幅の判定は従来どおりビューポート（`mobileNavStore.isNarrow`、サイドバーの
	 * オフキャンバスと同じ境界。2026-11-18 オーナー決定、#1182）を `narrowOverride` で渡し、
	 * 結果を `layoutNarrow` へ書き出す。`treeShown` は広い表示でのツリーの表示・非表示
	 * （「表示」メニューのチェックと同じ 1 か所の状態、部品の `leftOpen`）。
	 */
	const dock = createDockState();
	const dockLayoutStore = createDockLayoutStore('monitor');
	onDestroy(() => dockLayoutStore.dispose());
	let threePane: ReturnType<typeof ThreePaneDock> | undefined = $state();
	let treeShown = $state(true);
	let layoutNarrow = $state(false);
	/** 狭い表示で、ツリーが帯から引き出されているか（#378 の「開いている」）。 */
	const treeDrawerOpen = $derived(layoutNarrow && dock.flyoutPanel === 'left');
	let treePaneEl: HTMLDivElement | undefined = $state();

	/**
	 * #1253: 下のログのパネル（追加パネル `log`、既定は下の外周）。取得と保持はページが持ち
	 * （パネルは隠すと作り直されることがある）、実際に画面に出ている間だけ読む
	 * （`isDockPanelOnScreen`。狭い表示では部品が追加パネルを隠すので止まる）。
	 */
	const logFeed = createLogFeedState();
	onDestroy(() => logFeed.dispose());
	const logPanelOnScreen = $derived(isDockPanelOnScreen(dock.layout, 'log', dock.flyoutPanel));
	$effect(() => logFeed.setVisible(logPanelOnScreen));
	const dockPanels = [
		{ id: 'log', title: 'ログ', content: logPanel, initial: { edge: 'bottom' as const } }
	];

	const dockMessages = {
		...defaultDockMessages,
		threePaneLeft: () => '接続とグループ',
		threePaneCenter: () => 'タグの値',
		threePaneRight: () => '詳細'
	};

	// 低い画面では、部品が作った既定の配置（保存値が無い・既定に戻す）でログを下端の帯で
	// 始める（2026-11-21 オーナー決定、`dockLogBand.svelte.ts`）。
	const logBand = lowScreenLogBand({
		pane: () => threePane,
		dock,
		narrow: () => layoutNarrow
	});
	/** 「表示」メニューの項目（左のツリー・区切り・レイアウトを既定に戻す）。 */
	const viewItems = $derived(
		logBand.viewItems(threePane?.viewMenuItems({ resetLayout: 'レイアウトを既定に戻す' }) ?? [])
	);

	// 配置の保存。最初の 1 回（マウントした時点の配置）は保存の口が基準として受け取るだけ。
	// `layoutSnapshot()` の読み取りを追跡する（狭い表示の「既定に戻す」・復元で、`dock.layout`
	// の後に確定する控えた広い配置も拾う。PR #1268 レビュー P2）。
	// 広い ⇄ 狭いの切り替えの途中（求める表示と出している表示が食い違う間）は保存しない
	// （狭い表示の一時の木を書かない。PR #1268 再レビュー P2）。
	persistThreePaneLayout(
		() => threePane,
		dockLayoutStore,
		{
			wanted: () => mobileNavStore.isNarrow,
			shown: () => layoutNarrow
		},
		logBand
	);

	// サーバーの保存値（利用者ごと）を、枠のマウント後に 1 回だけ読んで当てる。
	let savedLayoutRequested = false;
	$effect(() => {
		const pane = threePane;
		if (!pane || savedLayoutRequested) return;
		savedLayoutRequested = true;
		void dockLayoutStore.loadSaved().then((saved) => {
			if (saved === undefined || threePane !== pane) return;
			// 部品が保存値を採らず既定へ戻したら、その既定にも低い画面の補正を当てて保存しない
			// （PR #1270 再レビュー P2、`dockLogBand.svelte.ts`）。
			logBand.restoreSaved(pane, saved);
		});
	});

	/**
	 * 「表示」メニューの項目を選ぶ。狭い表示でツリーを引き出したときは、フォーカスをツリーへ
	 * 入れる（帯のタブで開いたときと同じ。Esc で閉じられるように）。メニューが閉じて
	 * フォーカスをトリガーへ戻した後に移すため、描画を 1 回待つ。狭い表示ではパネルの項目を
	 * チェックではなく「開く」操作として出す（チェックの項目は選んでもメニューを閉じない。
	 * 引き出した pane の上にメニューが残らないように）。
	 */
	function selectViewItem(item: { id: string; onSelect: () => void }): void {
		item.onSelect();
		if (item.id !== 'panel:left' || !layoutNarrow) return;
		void tick().then(() =>
			requestAnimationFrame(() => {
				if (dock.flyoutPanel !== 'left') return;
				const target =
					treePaneEl?.querySelector<HTMLElement>('[role="treeitem"][tabindex="0"]') ??
					treePaneEl?.querySelector<HTMLElement>('[role="tree"]');
				target?.focus();
			})
		);
	}

	// --- #1179 PR3b: ツリーの選択と URL -----------------------------------
	//
	// タグ登録（`(app)/tags/+page.svelte` の #1179 PR3a）と同じ作り（タブと未保存の
	// 確認が無い分だけ小さい）:
	//
	// - **ツリーの操作は状態を先に同期で変え、URL は後から追う**（`requestTarget`）。
	//   BantoTree は `selection` の制御で「呼び出しの直後の値」を正とするので、
	//   非同期の `goto` を待ってから状態を変えると上下キーの移動で元の行へ戻される。
	// - **URL から状態を決めるのは、戻る・進む・再読み込み・リンク**（下の `$effect`）。
	//   ページ内の `goto` の間（`targetNavsInFlight`）は止める。
	// - **履歴の積み方**: クリック・Enter・Space での選択は 1 回ごとに積み、上下キー
	//   などの移動が続く間は同じ項目を置き換える（タグ登録の `viewEntryFromKeyNavigation`
	//   と同じ理由）。
	// - 一覧を読み終えたら、読めない値・一覧に無い id は「すべて」へ倒し、URL も
	//   正規化する（`replace`）。案内は URL から来た対象が見つからないときだけ
	//   （#381 レビュー対応3回目の「選択中の接続・収集グループが消えたら『すべて』へ
	//   戻す」も、ここで同じ純関数 `pruneTreeFilter` を通して行う - 消えた id で
	//   絞られたままだと一覧が常に空になり、購読（`subscriptionPatternsFor`）も
	//   空のままになる。削除の結果なので案内は出さない）。

	/** 進行中のページ内の `goto` の数。0 になったら URL から状態を決め直す。 */
	let targetNavsInFlight = $state(0);
	/** いまの履歴の項目が、ツリーの上下キーなどの移動で積んだものか。 */
	let entryFromKeyNavigation = false;

	/** ツリーで選んだ対象にする（URL は後から追う）。 */
	function requestTarget(next: MonitorTreeFilter, options: { keyNavigation: boolean }): void {
		if (sameTreeTarget(treeFilter, next)) return;
		const replace = options.keyNavigation && entryFromKeyNavigation;
		entryFromKeyNavigation = options.keyNavigation;
		treeFilter = next;
		void navigateToTarget(next, { replace });
	}

	async function navigateToTarget(
		target: MonitorTreeFilter,
		options: { replace: boolean }
	): Promise<void> {
		const url = new URL(page.url.href);
		// T18-4c の確認対象（`focus`）は選択を変えても残す（`#lib/banto/monitorView.ts`）。
		url.search = monitorViewSearch(target, parseMonitorFocus(url.searchParams));
		// 先の `goto` がまだ終わっていない間は `page.url` が古いので、同じに見えても
		// 省かない（上下キーを続けて押し、元の行へ戻ったときに URL だけが先の行に残る）。
		if (url.href === page.url.href && targetNavsInFlight === 0) return;
		targetNavsInFlight += 1;
		try {
			// `reset: false`: フォーカスとスクロールを動かさない（同じページの中の切り替え）。
			await goto(url, { replace: options.replace, reset: false });
		} catch {
			// 失敗しても、下の `$effect` がその時点の URL へ状態を戻す。
		} finally {
			targetNavsInFlight -= 1;
		}
	}

	/** 一覧で一度は見つかった対象（消えたときに案内しないため、タグ登録と同じ）。 */
	let lastFoundTarget: MonitorTreeFilter = { type: 'all' };
	$effect(() => {
		const search = page.url.search;
		const loaded = adminLoaded;
		const currentConnections = connections;
		const currentGroups = groups;
		if (targetNavsInFlight > 0) return;
		const params = new URLSearchParams(search);
		const parsed = parseTreeTarget(params);
		const resolved = resolveTreeTarget(parsed, currentConnections, currentGroups, { loaded });
		untrack(() => {
			if (loaded && !resolved.missingTarget) lastFoundTarget = resolved.target;
			// 案内は、表示が変わるかとは別に判定する（すでに「すべて」を表示中に読めない・
			// 存在しない対象の URL で来ても出す）。URL はこのあと正規化されるので、出るのは 1 回。
			const wasFound = parsed !== null && sameTreeTarget(parsed, lastFoundTarget);
			if (loaded && resolved.missingTarget && !wasFound) {
				toastStore.push('info', MISSING_TREE_TARGET_MESSAGE);
			}
			if (!sameTreeTarget(treeFilter, resolved.target)) treeFilter = resolved.target;
			const canonical = monitorViewSearch(resolved.target, parseMonitorFocus(params));
			if (loaded && search !== canonical) {
				void navigateToTarget(resolved.target, { replace: true });
			}
		});
	});

	afterNavigate((nav) => {
		// 戻る・進むで来た項目は、どの操作で積んだものか分からない。
		if (nav.type === 'popstate') entryFromKeyNavigation = false;
	});

	// 選んでいる対象をタグ登録と共有する（ナビのリンクが `?target=` に付ける）。
	$effect(() => {
		sharedTreeTarget.set(treeFilter);
	});

	/**
	 * #381 レビュー対応8回目（層の約束・項目6、`@banto/ui` の `layering.ts`）: **下位の層を
	 * 開くなら上位の層を先に畳む。** サイドバー（z-index 710）はモーダルでは
	 * ないのでフォーカストラップで塞げず、開いたまま Tab でヘッダー経由この
	 * トグルへ到達できる。そのままツリー（枠の中のドロワー）を開くと重なりが逆順になり、
	 * Esc で下の層から閉じることになる。サイドバーは未保存状態を持たない常設
	 * ナビなので、閉じて安全（`Sidebar.svelte` のリンククリックでも閉じている）。
	 *
	 * #1179 PR2b: トグルは枠の帯にあり、押したことはページから拾えないので、
	 * **開いた遷移**で畳む（#1253 から、狭い表示でツリーが帯から引き出された =
	 * `dock.flyoutPanel === 'left'`）。依存はそれだけ（サイドバーの開閉では再実行
	 * しない - ツリーを開いたあとでサイドバーを開くのは正しい重なりなので、そこで
	 * 畳んではいけない）。
	 */
	$effect(() => {
		if (!treeDrawerOpen) return;
		untrack(() => {
			if (mobileNavStore.open) mobileNavStore.closeNav();
		});
	});

	/** #378: 閉じていても何で絞られているか分かるよう、中央のツールバーに出す選択名。 */
	const treeSelectionLabel = $derived.by((): string => {
		if (treeFilter.type === 'connection') {
			const connectionId = treeFilter.id;
			return connections.find((c) => c.id === connectionId)?.name ?? 'すべて';
		}
		if (treeFilter.type === 'group') {
			const groupId = treeFilter.id;
			return groups.find((g) => g.id === groupId)?.name ?? 'すべて';
		}
		return 'すべて';
	});

	function handleTreeSelect(
		data: ConnectionTreeNodeData,
		{ navigation }: { navigation: boolean }
	): void {
		const next: MonitorTreeFilter =
			data.kind === 'all'
				? { type: 'all' }
				: data.kind === 'connection'
					? { type: 'connection', id: data.connection.id }
					: { type: 'group', id: data.group.id };
		requestTarget(next, { keyNavigation: navigation });
		// #378: 狭幅では「選んだ」時点（クリック・Space・Enter）で退避パネルを
		// 閉じる（タグ登録ページと同じ）。狭幅は `selectionFollowsFocus=false`
		// なので、上下キーは選択を起こさず、ここへ来るのは選んだときだけ。
		// #1253: 退避パネルは枠の帯から引き出した pane。戻すとフォーカスは帯のタブへ移る（部品）。
		if (treeDrawerOpen) dock.closeFlyout();
	}

	// --- T18-4c: 確認導線のディープリンク受け口 -----------------------------
	//
	// タグ登録ページの CTA（`monitorHref`、`#lib/banto/tagOnboarding.ts`）から
	// `?target=`/`?focus=` 付きで遷移してきたときに、ツリー選択と「確認対象」
	// ハイライトへ反映する。#1179 PR3b から、対象は上の URL の同期が決め
	// （以前の `?group=`/`?connection=` は受け付けない）、`focus` も URL から
	// 読む（ツリーの選択を変えても URL に残すので、強調は保たれる。再読み込みでも戻る）。
	// T18-4c の当時と同じく、virtual（calc/mem）接続・配下グループも除外しない
	// （モニタは computed/internal タグも閲覧する読み取り専用画面）。

	/** T18-4c: `?focus=` で渡された external_name の集合。持続的な「確認対象」強調に使う。 */
	const focusSet = $derived(new Set(parseMonitorFocus(page.url.searchParams)));

	// --- T18-4b: WS 接続（1本を維持） --------------------------------------
	//
	// `streamResubscribe` はこの $effect が張ったソケットの `resubscribe`
	// を、下のツリー選択監視 $effect から呼べるように保持するだけの素の
	// クロージャ参照（`$state` にする必要はない - 参照する側は `treeFilter`
	// の変化にだけ反応すればよく、この変数自体の再代入を検知する必要は
	// 無いため）。

	let streamResubscribe: (() => void) | null = null;

	$effect(() => {
		void reloadCatalog();
		void reloadAdmin();

		const { disconnect, resubscribe, resume } = connectTagStream(
			{
				onData: (values, { snapshot }) => {
					applyStreamData(values, snapshot);
					// 初期スナップショット（またはそれ以降の on_change）を
					// 1回でも受けたら stale 強制表示を解除する。
					dispatchStreamView({ type: 'data' });
				},
				onConfigChanged: () => {
					void reloadCatalog();
					void reloadAdmin();
				},
				onStatusChange: (connected, closeCode) => {
					// 接続: 新しい購読の初期スナップショットが届くまで stale
					// 表示にし、バックプレッシャ表示は解除する。切断: 1013
					// （`stream.rs::BACKPRESSURE_CLOSE_CODE`、送信キュー溢れ）
					// なら通常の再接続中とは別の文言を出す。
					dispatchStreamView(
						connected ? { type: 'connected' } : { type: 'disconnected', code: closeCode }
					);
				},
				// #445: 再接続が続けて失敗したら（切れている間の失効はブラウザには
				// `1006` しか見えない）、画面を動かさずにログイン状態を確かめる。
				// 失効を確認できたら `onHalt` の `recheckSession`（下）へ合流する。
				probeSession: probeSessionAfterReconnectFailures,
				onHalt: (action) => {
					dispatchStreamView({ type: 'halted', action });
					if (action.kind !== 'recheckSession') return;
					// ルートガードを走らせ直す（`sessionRecheck.ts`）。失効なら
					// `/login`、照合できなければエラー画面へ移り、この画面は
					// 外れる（`disconnect()` 済みなので `resume()` は何もしない）。
					// まだ有効ならこの画面に残るので、購読を再開する。
					recheckSessionAfterStreamClose().then(
						() => {
							dispatchStreamView({ type: 'resumed' });
							resume();
						},
						() => {
							dispatchStreamView({ type: 'recheckFailed', reason: action.reason });
						}
					);
				}
			},
			() => subscriptionPatternsFor(treeFilter, connections, groups)
		);
		streamResubscribe = resubscribe;
		streamResume = resume;

		return () => {
			streamResubscribe = null;
			streamResume = null;
			disconnect();
		};
	});

	/** #441: `halt` で止まった購読を、利用者の操作で再開する。 */
	function resumeStream(): void {
		dispatchStreamView({ type: 'resumed' });
		streamResume?.();
	}

	// --- T18-4b: ツリー選択の変更を購読に反映する（再接続はしない） --------
	//
	// 接続そのものは上の $effect が1本維持する - ここは `treeFilter` が
	// 変わるたびに既存ソケット上で unsubscribe→再 subscribe するだけ
	// （`connectTagStream` の doc comment 参照）。マウント直後にも1回走るが、
	// その時点ではソケットがまだ開いていないことが多く `resubscribe()` は
	// no-op になる（次の onopen が現行の `treeFilter` で購読するので問題
	// ない）。
	//
	// #1179 PR3b: URL（`?target=`）の対象は一覧を読み終える前から当てるので、その間の
	// 購読は「すべて」（`subscriptionPatternsFor` が引けない id を全件に倒す）。読み終えて
	// パターンが変わったときにも購読し直すよう、`treeFilter` ではなくパターンを見る。
	const subscriptionKey = $derived(
		subscriptionPatternsFor(treeFilter, connections, groups).join('\n')
	);
	$effect(() => {
		void subscriptionKey;
		streamResubscribe?.();
		dispatchStreamView({ type: 'resubscribed' });
	});

	const filteredRows = $derived(filterMonitorRows(rows, treeFilter, searchQuery));

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
		<p
			class="note status-line"
			class:status-warning={!streamView.connected &&
				(streamView.backpressure || streamView.halt !== null)}
			data-testid="monitor-stream-status"
		>
			<span class="ws-dot" class:on={streamView.connected} class:off={!streamView.connected}></span>
			{#if streamView.connected}
				接続中（リアルタイム更新中）
			{:else if streamView.halt?.kind === 'recheckSession'}
				<!-- #441: close 1008 + session_revoked（試運転の grant の失効も同じ）。
					#445: 再接続が続けて拒否され、確かめたら失効していた／再接続の
					最中にトークンが消えた。
					ルートガードの確認が終わるまでのあいだだけ出る。 -->
				ログイン状態を確認しています…（リアルタイム更新は止まっています）
			{:else if streamView.halt?.kind === 'halt'}
				<!-- #441: close 1008 + api_key_* / 未知の理由文。再接続はしない。 -->
				{streamView.halt.message}
				<button type="button" class="resume-button" onclick={resumeStream}>再接続</button>
			{:else if streamView.backpressure}
				<!-- T18-4b: BACKPRESSURE_CLOSE_CODE (1013) - 送信キュー溢れによる
					強制切断。通常の再接続中と見た目・文言を変えて区別する。 -->
				サーバーが遅い購読者として切断しました（自動再接続中…）
			{:else}
				再接続中…（値は最後の受信内容のまま停止しています）
			{/if}
		</p>

		{#if loadError}
			<p class="error-text">{loadError}</p>
		{/if}
	</div>

	<!--
		T18-4a: タグ登録ページと同じ枠 + ConnectionTree + 検索ボックス。左ツリーは
		接続/グループを選択して絞り込むだけの表示専用（`oncontextmenu` は渡さない -
		このページに作成系 UI は無い）。#1253 から枠は `@banto/dock-svelte` の
		`ThreePaneDock`（右は使わないので `right` を渡さない）。

		#381 レビュー対応6回目: **タグが0件でも枠を出す**。条件マウントだと、ツリーや
		トグルにフォーカスがある状態で最後の行が消えたときに**フォーカスの戻り先ごと
		アンマウントされる**（フォーカスが `<body>` に落ちる）。#1253 から、最初の
		読み込み中も枠を出して案内を中央に置く（枠を作り直すと配置の保存・読み直しの
		基準が崩れるため）。
	-->
	<div class="content">
		<!--
			#378（2026-09-16 オーナー決定）: 狭幅では左ツリーを退避する。#1253 から退避は
			部品の狭い表示（左端の自動で隠す帯。タブで引き出し、Esc・外の押下で戻り、
			フォーカスはタブへ戻る）。狭幅の判定はビューポート基準（`narrowOverride`）。
			ツリーは引き出すたびに作り直さない（`keepMounted`。展開の状態を保つ）。
		-->
		<ThreePaneDock
			bind:this={threePane}
			{dock}
			initialLayout={dockLayoutStore.initial}
			onRestore={logBand.onRestore}
			bind:leftOpen={treeShown}
			bind:narrow={layoutNarrow}
			narrowOverride={mobileNavStore.isNarrow}
			keepMounted={(id) => id === 'left'}
			panels={dockPanels}
			messages={dockMessages}
		>
			{#snippet left()}
				<div class="tree-pane" bind:this={treePaneEl}>
					<ConnectionTree
						{connections}
						{groups}
						tags={adminTags}
						selectedId={treeSelectedId}
						selectionFollowsFocus={!layoutNarrow}
						onselect={handleTreeSelect}
					/>
				</div>
			{/snippet}
			<div class="right-pane">
				<div class="toolbar">
					{#if !layoutNarrow}
						<!-- 広い表示でのツリーの表示・非表示（「表示」メニューのチェックと同じ状態）。
							狭い表示では左端の帯のタブが開く経路（このボタンは出さない）。 -->
						<button
							type="button"
							class="tool-button"
							aria-pressed={treeShown}
							data-testid="monitor-tree-toggle"
							onclick={() => (treeShown = !treeShown)}>接続とグループ</button
						>
					{/if}
					<span class="tree-selection" data-testid="monitor-tree-selection">
						{treeSelectionLabel}
					</span>
					<input
						type="search"
						class="search-box"
						placeholder="外部名・名前・アドレスで検索"
						bind:value={searchQuery}
					/>
					<span class="count">{filteredRows.length} / {rows.length} 件</span>
					{#if viewItems.length > 0}
						<Menu label="表示">
							{#snippet trigger(props)}
								<button type="button" class="tool-button" data-testid="monitor-view-menu" {...props}
									>表示</button
								>
							{/snippet}
							{#each viewItems as item (item.id)}
								{#if item.kind === 'separator'}
									<MenuSeparator />
								{:else if item.kind === 'checkbox' && !layoutNarrow}
									<MenuItem
										label={item.label}
										kind="checkbox"
										checked={item.checked}
										onSelect={() => selectViewItem(item)}
									/>
								{:else}
									<MenuItem label={item.label} onSelect={() => selectViewItem(item)} />
								{/if}
							{/each}
						</Menu>
					{/if}
				</div>
				{#if loading && rows.length === 0}
					<p class="note">読み込み中…</p>
				{:else if rows.length === 0}
					<!--
								T18-2d（docs/hub/banto-hub-desktop-plan.md §9.4 TAG-UX-A「空状態を…
								不足する前工程と移動ボタンを示す」）: タグが1件も無い（フィルタ
								の問題ではなく真の空）場合は、前工程（タグ登録）へ案内する。
								文言・CTA は従来のまま、置き場所だけ右ペインの中へ移した
								（#381 レビュー対応6回目 - 上の枠のコメント参照）。
							-->
					<p class="note">
						登録されているタグがありません。先に タグの登録画面 からタグを作成してください。
					</p>
					<a class="onboarding-cta" href={resolveAppPath('/tags')}>タグの登録画面へ移動</a>
				{:else if filteredRows.length === 0}
					<p class="note">条件に一致するタグがありません。</p>
				{:else}
					<div class="table-wrap">
						<table class="values-table">
							<thead>
								<tr>
									<th>外部名</th>
									<th>接続</th>
									<th>グループ</th>
									<th>{columnLabels.value}</th>
									<th
										title="未確認 = Hub の起動後に、まだ一度も実機から値を読めていない状態（収集開始後の不良とは別）"
										>{columnLabels.quality}</th
									>
									<th>時刻</th>
								</tr>
							</thead>
							<tbody>
								{#each filteredRows as row (row.external_name)}
									{@const cell = monitorCellDisplay(row, displayMode)}
									<tr
										class:flash={flashing[row.external_name]}
										class:confirm-target={focusSet.has(row.external_name)}
									>
										<td class="tag-name">{row.external_name}</td>
										<td>
											{row.connection}
											{#if row.simulation}
												<span class="sim-badge" title="シミュレーション接続（実機ではありません）"
													>⚠ SIM</span
												>
											{/if}
										</td>
										<td>{row.group}</td>
										<td class="value quality-{cell.qualityClass}">{cell.value}</td>
										<td class="quality quality-{cell.qualityClass}">{cell.qualityLabel}</td>
										<td>{formatTime(row.t)}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</div>
		</ThreePaneDock>
	</div>
</div>

{#snippet logPanel()}
	<LogPanel feed={logFeed} />
{/snippet}

<style>
	/* T18-4a: tags ページ（`(app)/tags/+page.svelte`）と同じ
	   page/page-header/content/right-pane/toolbar 構成 - 枠（#1253 から `ThreePaneDock`）が
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

	/* T18-4b: バックプレッシャ切断（1013）は通常の再接続中と文言だけでなく
	   色でも区別する - 既存の .ws-dot.off と同じ --banto-danger を流用。 */
	.status-line.status-warning {
		color: var(--banto-danger);
	}

	.resume-button {
		padding: 0.1rem 0.6rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		color: var(--banto-text);
		font: inherit;
		cursor: pointer;
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

	/* #1253: ツリーの表示・非表示と「表示」メニューのボタン（`.resume-button` と同じ見た目）。 */
	.tool-button {
		padding: 0.3rem 0.6rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-surface);
		color: var(--banto-text);
		font: inherit;
		font-size: 0.8rem;
		cursor: pointer;
	}

	.tool-button[aria-pressed='true'] {
		background: var(--banto-bg);
		border-color: var(--banto-primary);
	}

	/* #378: 何で絞られているか（ツリーを閉じていても見える）。 */
	.tree-selection {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.8rem;
		font-weight: 600;
	}

	.tree-pane {
		height: 100%;
		min-height: 0;
		overflow: auto;
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

	/* 未確認: 通信異常（赤）ではなく、まだ読めていないだけ。stale より斜体で見分ける。 */
	.quality-unverified {
		color: var(--banto-text-muted);
		font-style: italic;
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

	/* T18-4c（確認導線）: `?focus=` で渡された行の持続的な強調表示。
	   `tr.flash`（一時的な値変化ハイライト、数百ms で消える）とは別物 -
	   左ボーダー＋淡い背景を消えないまま保つ。配色は tags ページの
	   `.tag-row-selected`（`(app)/tags/+page.svelte` の
	   `:global(.row.tag-row-selected)`）に合わせてある。`tr.flash` と同時に
	   付いても両立する（flash 中は一時的に background が上書きされ、
	   flash が消えると confirm-target の背景へ遷移で戻る）。 */
	tr.confirm-target td {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
	}

	tr.confirm-target td:first-child {
		border-left: 3px solid var(--banto-primary);
	}
</style>
