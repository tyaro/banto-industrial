<script lang="ts">
	/**
	 * Hub 接続カテゴリ（#332 chronogazer 分）。admin 限定（`+layout.ts` の
	 * `visible.hub` 参照）。
	 *
	 * chronogazer はこれまで banto-hub に接続するコードを持っていなかった
	 * ので、この section は既存画面の移設ではなく新規。やっていることは
	 * 「接続先の設定」「6 状態の表示」「タグ一覧と選択の保存」と、
	 * #383 段階1 で足した「購読の状態と最新値」まで。トレンド表示・保存
	 * （tstore）は段階3 なのでここには無い。
	 *
	 * 購読ブロックの規律（#383 段階1）:
	 * - 購読は接続設定の 6 状態とは**別軸**。購読が張れなくても状態表示は
	 *   汚れず、張れない理由は `reason` に出る。
	 * - **未解決タグ**（Hub から消えた／権限で見えない）は一覧で出す。
	 *   空表示や「タグ0件」に潰さない。
	 * - ポーリングは**この設定ページを開いている間だけ**（`hub_subscription`
	 *   / `GET /api/hub/subscription` はメモリを読むだけでネットワークを
	 *   叩かない）。タブが隠れている間は止める。
	 * - **ポーリングが恒久的に失敗したら、そう言う**（レビュー P2-A）。
	 *   2 回連続で失敗したら見出しで「受信中」と言い切らず、値の表は残した
	 *   まま「いつ取得できた表示か」を添える。トーストは出さない（明示操作の
	 *   エラー表示を上書きしないという既存の設計どおり）。
	 * - **未保存のタグ選択を黙って捨てない**（レビュー P2-B、#378 の方針）。
	 *   「一覧を更新」「接続」はサーバーの選択で編集中のチェックを上書き
	 *   しない。代わりに注記と「サーバーの内容に戻す」を出す。ただし
	 *   **下書きは接続先ごとのもの**（レビュー P2-2）で、別の Hub に繋ぎ直し
	 *   たら破棄する - 破棄したことは画面に出す。
	 * - **画面→アプリの往復にも上限を置く**（レビュー P2-3）。応答が返って
	 *   こない相手だと、失敗を数える仕組みもポーリングのループごと止まる。
	 *   **明示操作（接続・採用・一覧更新・保存・切断・状態取得）にも同じ形で
	 *   上限を置く**（`HUB_UI_TIMEOUT_MS`）。上限が無いと `run()` が
	 *   `busy = true` のまま戻らず、画面そのものが操作不能になる。ただし
	 *   **打ち切りは「失敗した」ではない**（アプリ側の処理は止まらない）ので、
	 *   文言は `hubAbandonedDisplay` が作る通常の失敗とは別のものを出し、
	 *   打ち切った直後に必ず状態を読み直す。
	 * - **状態を再確認できていない間は、接続先に依存する変更操作を止める**
	 *   （オーナーレビュー P2-1）。打ち切った操作はバックエンドで完了している
	 *   かもしれないので、読み直しにも失敗したら「今の接続先が分からない」。
	 *   `busy` とは別のフラグ（`statusUnconfirmed`）を立て、読み直せるまで
	 *   降ろさない。抜け道として**読み取り専用の「状態を再取得」**を 1 つ出す
	 *   （画面全体を操作不能にはしない）。
	 * - **保存本体の成否は、保存後の読み直しとは別の予算で確定させる**
	 *   （オーナーレビュー P2-2）。同じ上限の内側に置くと、保存に成功しても
	 *   読み直しが返らないだけで操作全体が打ち切り扱いになり、「保存しました」
	 *   と「操作は続いている可能性があります」が併存する
	 *   （`saveSelectionWithLimits`）。
	 *
	 * 平文の API キーは画面に出さない: 手動連携の入力欄は
	 * `type="password"`、応答型（`HubView`）にキー欄は無い。
	 */
	import { onDestroy, onMount } from 'svelte';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import {
		adoptHubKey,
		applyServerSelection,
		connectHub,
		disconnectHub,
		getHubStatus,
		getHubSubscription,
		hubAbandonedDisplay,
		hubLastValueLabel,
		hubPollStaleNote,
		hubRemainderNote,
		hubStatusDetail,
		hubStatusLabel,
		hubSubscriptionDetail,
		hubSubscriptionHeadline,
		hubTimeLabel,
		HUB_UI_TIMEOUT_MS,
		isHubAvailable,
		isPollGenerationCurrent,
		isPollResultFresh,
		isSubscriptionStale,
		nextPollFailureCount,
		nextSelectionUnsaved,
		nextStatusUnconfirmed,
		pollFailureOutcome,
		readSubscriptionWithLimit,
		reconfirmStatusFailureNotice,
		refreshHubCatalog,
		runWithLimit,
		saveSelectionWithLimits,
		SELECTION_DISCARDED_NOTICE,
		setHubSelectedTags,
		showManualKeyEntry,
		showsServerSelectionDiff,
		type HubStatus,
		type HubSubscription,
		type HubTag,
		type HubView
	} from '$lib/banto/hubAdmin';
	import { errorMessage } from './shared';

	const available = isHubAvailable();

	/** 購読状態のポーリング間隔（ms）。ネットワークを伴わない読み取り。 */
	const SUBSCRIPTION_POLL_MS = 2000;

	let status = $state<HubStatus>({ state: 'notConfigured' });
	let tags = $state<HubTag[] | null>(null);
	let selected = $state<string[]>([]);
	/**
	 * 最後にサーバーから届いた選択（レビュー P2-B）。編集中の `selected` と
	 * 別に持つのは、「サーバーの内容に戻す」で戻す先と、注記を出すかどうかの
	 * 判定（[`showsServerSelectionDiff`]）に要るため。
	 */
	let serverSelected = $state<string[]>([]);
	/** 選択に未保存の変更があるか（遷移は `nextSelectionUnsaved`）。 */
	let selectionUnsaved = $state(false);
	/**
	 * 今の下書き（`selected`）が**どの接続先のものか**（レビュー P2-2）。
	 * 入るのは**サーバーが返した `HubView.endpoint`** だけで、入力欄の
	 * `endpointDraft` は入れない - まだ接続していない入力値と比べると、URL を
	 * 打ち込んだ瞬間に「別の Hub」と判定してしまう。
	 */
	let selectionEndpoint = $state<string | null>(null);
	/**
	 * 接続先が変わったので未保存の下書きを捨てた、という通知（レビュー P2-2）。
	 * `savedNotice`（保存成功）とは別に持つ - 同じ変数に入れると「保存しました」
	 * と混ざり、捨てたものが保存されたように読める。
	 */
	let selectionDiscardedNotice = $state<string | null>(null);
	const selectionDiffers = $derived(
		showsServerSelectionDiff(selected, serverSelected, selectionUnsaved)
	);
	let keyName = $state<string | null>(null);
	let subscription = $state<HubSubscription | null>(null);
	/**
	 * 購読状態のポーリングが連続で失敗した回数（成功で 0 に戻る）と、最後に
	 * 取得できた時刻（レビュー P2-A）。`SUBSCRIPTION_POLL_FAILURE_LIMIT`
	 * 回で「取得できていない」表示に切り替える。
	 */
	let pollFailures = $state(0);
	let lastPolledAt = $state<number | null>(null);
	const subscriptionStale = $derived(isSubscriptionStale(pollFailures));
	/**
	 * `lastError` を独立した行で出すか。`stopped` かつ `reason` が無いときは
	 * `hubSubscriptionDetail` がエラーを文中に入れるので、二重に出さない。
	 */
	const showLastErrorLine = $derived(
		subscription !== null && !(subscription.state === 'stopped' && !subscription.reason)
	);
	/**
	 * 保存済みの接続先があるか（= 設定 KV に `HubRecord` があるか）。入力欄の
	 * 下書き（`endpointDraft`）とは別に持つ: 到達不能な URL で「接続」した
	 * あとも下書きは残るが、設定は**保存されていない**（記録は接続に成功
	 * したときだけ作られる）ので、「切断」はそのとき出してはいけない。
	 */
	let configured = $state(false);
	let endpointDraft = $state('');
	let manualKeyDraft = $state('');
	let busy = $state(false);
	let hubError = $state<string | null>(null);
	let savedNotice = $state<string | null>(null);
	/**
	 * **接続状態を再確認できていない**（オーナーレビュー P2-1）。打ち切った操作の
	 * あと状態も読み直せなかったときに立ち、`nextStatusUnconfirmed` が言うとおり
	 * **状態を読めて `applyView()` で反映できたときだけ**降りる。
	 *
	 * `busy` と別軸にするのが要点: `busy` は `run()` の `finally` で必ず降りるので、
	 * これが無いと**接続先を確認できないまま「選択を保存」等が再び押せる**。
	 * `setHubSelectedTags` はタグ名しか送らないため、打ち切った `connect` が
	 * バックエンドで完了していると、別の Hub の購読設定に前の Hub のタグ選択を
	 * 保存してしまう（#397 で塞いだ型の再発）。
	 */
	let statusUnconfirmed = $state(false);
	/**
	 * 上のフラグに添える警告（オーナーレビュー P2-1）。**`hubError` とは別に持つ** -
	 * `run()` は冒頭で `hubError` を消すので、そこに置くと次の操作で消えてしまい、
	 * 警告がこの操作を止める役割を果たさない。
	 */
	let statusUnconfirmedNotice = $state<string | null>(null);

	/**
	 * 明示操作の結果を何回反映したか。飛行中のポーリングはこの番号を覚えて
	 * おき、着いたときに変わっていたら自分の応答を捨てる
	 * （`isPollResultFresh`）。
	 *
	 * **`applyView` を通る操作だけが明示操作、ではない**: 選択の保存のように
	 * view を返さない（204）操作も設定と購読を変えるので、番号を進めなければ
	 * 保存中に飛んでいたポーリング応答が保存後に受け入れられ、古いタグの値と
	 * 状態を表示してしまう。**view を返さない操作も
	 * `beginExplicitChange()` で番号を進めること。**
	 */
	let appliedSeq = 0;

	/**
	 * 「今から画面の状態を明示的に変える」と宣言する。これ以前に飛ばした
	 * ポーリング応答は以後受け入れられない。
	 */
	function beginExplicitChange(): void {
		appliedSeq += 1;
	}

	/**
	 * 購読状態を「今この瞬間の実物として受け取れた」と記録する（レビュー
	 * P2-A）。ポーリングだけでなく明示操作の応答も購読状態を運んでくるので、
	 * どちらもここを通す - 通さないと、明示操作で取り直した直後に「取得でき
	 * ていません」が残る。
	 */
	function markSubscriptionFresh(): void {
		pollFailures = nextPollFailureCount(pollFailures, 'ok');
		lastPolledAt = Date.now();
	}

	function applyView(view: HubView): void {
		beginExplicitChange();
		status = view.status;
		configured = view.endpoint !== null;
		keyName = view.keyName;
		serverSelected = [...view.selectedTags];
		// レビュー P2-B: 未保存の選択はサーバーの値で黙って上書きしない
		// （#378「未保存の入力を黙って捨てない」）。上書きしなかったことは
		// `selectionDiffers` の注記と「サーバーの内容に戻す」で伝える。
		// `status`/`tags`/`subscription`/`keyName` は従来どおり上書きする -
		// 編集中なのは選択だけ。
		//
		// レビュー P2-2: ただし下書きを保つのは**同じ接続先のとき**だけ。別の
		// Hub に繋ぎ直すと `tags` は新しい Hub のものに入れ替わるので、旧 Hub
		// のタグ名を下書きに残すと、画面に出ていない名前が保存の payload に
		// 混入する（バックエンドは endpoint が変われば選択を引き継がない）。
		// 判定は接続先の同一性だけ - catalog との積集合は取らない。
		const outcome = applyServerSelection(
			selected,
			view.selectedTags,
			selectionUnsaved,
			selectionEndpoint,
			view.endpoint
		);
		selected = outcome.selected;
		selectionUnsaved = outcome.unsaved;
		// 捨てるのが正しい場面でも、捨てたことは黙っていない（#378）。
		if (outcome.discardedForEndpointChange) selectionDiscardedNotice = SELECTION_DISCARDED_NOTICE;
		selectionEndpoint = view.endpoint;
		// `null`（この往復では catalog を読めていない）と `[]`（読めた結果
		// タグ 0 件）は別物。前者では前回の一覧を残さず消す - 状態表示の
		// 「接続済み・利用可能なタグなし」と食い違わせないため。
		tags = view.tags;
		subscription = view.subscription;
		markSubscriptionFresh();
		if (view.endpoint) endpointDraft = view.endpoint;
	}

	/**
	 * 明示操作を打ち切ったあとの後始末（[`hubAbandonedDisplay`] の表に従う）。
	 *
	 * **状態を必ず読み直す**: 打ち切ったのは待ち時間だけなので、アプリ側では
	 * 操作が完了しているかもしれない。読み直せたらその結果で画面を更新する
	 * （実は成功していたなら、正しい状態が出る）。読み直しも打ち切られたら
	 * **新しい状態を作らない** - 前の表示を残したまま、最新ではないと書く。
	 *
	 * 読み直しは `getHubStatus()` で、これも `HUB_UI_TIMEOUT_MS` を持つ
	 * （持たないと、ここで再び永久に戻らなくなり `busy` が解放されない）。
	 *
	 * **`busy` を降ろす前に済ませる**のは、読み直しの応答が、その間にユーザーが
	 * 起こした新しい操作の結果を巻き戻さないようにするため。待ちは最悪でも
	 * `HUB_UI_TIMEOUT_MS` の 2 回分で必ず終わる（無限に固まらない、が目的）。
	 */
	async function rereadAfterAbandon(): Promise<void> {
		const reread = await runWithLimit((signal) => getHubStatus(signal), HUB_UI_TIMEOUT_MS);
		const display = hubAbandonedDisplay(true, reread.kind === 'ok');
		if (display.applyStatus && reread.kind === 'ok') applyView(reread.value);
		// オーナーレビュー P2-1: 読み直せなかったときは、警告を出すだけでなく
		// **接続先に依存する変更操作を止める**。警告は `hubError` ではなく専用の
		// state に置く（`run()` が冒頭で `hubError` を消すため、次の操作を始めた
		// 瞬間に消えてしまい、止める役割を果たさない）。
		statusUnconfirmed = display.blocksChanges;
		if (display.blocksChanges) {
			statusUnconfirmedNotice = display.notice;
			// 止めている理由はこの 1 行に集約する（同じ内容を 2 箇所に出さない）。
			hubError = null;
		} else {
			statusUnconfirmedNotice = null;
			// `applyView` は `hubError` を触らないので、反映のあとに置いてよい。
			hubError = display.notice;
		}
	}

	/**
	 * 読み取り専用の回復導線（オーナーレビュー P2-1）。`statusUnconfirmed` が
	 * 立っている間だけ出す「状態を再取得」。
	 *
	 * **`run()` は通さない**: `run()` は打ち切ったときに `rereadAfterAbandon()`
	 * を呼ぶので、読み直しの読み直しになる。ここは `getHubStatus()` を
	 * `HUB_UI_TIMEOUT_MS` 付きで 1 回だけ走らせ、**読めたときだけ**フラグと警告を
	 * 降ろす。読めなければ**何も壊さず**そのまま（`nextStatusUnconfirmed`）。
	 *
	 * **失敗しても無反応に見せない**（#400 レビュー対応の仕上げ）。以前は
	 * `outcome.kind !== 'ok'` を一律 `failed` に畳んで `nextStatusUnconfirmed`
	 * に渡すだけで、失敗時の表示は何も変えていなかった - 押しても何も起きて
	 * いないように見えた。ここでは `outcome.kind` を `timedOut`/`failed` の
	 * ままフラグの遷移とは別に `reconfirmStatusFailureNotice()` へ渡し、
	 * 「再取得も失敗した」と分かる文言に差し替える。文言は `hubError` では
	 * なく `statusUnconfirmedNotice` に置く（`run()` の `beginRun()` が
	 * `hubError` を消すのに対し、こちらは次の操作でも消えない - 消えると
	 * 「止めている」表示ごと消える）。
	 */
	async function reconfirmStatus(): Promise<void> {
		busy = true;
		try {
			const outcome = await runWithLimit((signal) => getHubStatus(signal), HUB_UI_TIMEOUT_MS);
			if (outcome.kind === 'ok') applyView(outcome.value);
			statusUnconfirmed = nextStatusUnconfirmed(
				statusUnconfirmed,
				outcome.kind === 'ok' ? 'ok' : 'failed'
			);
			if (!statusUnconfirmed) {
				statusUnconfirmedNotice = null;
			} else {
				statusUnconfirmedNotice = reconfirmStatusFailureNotice(
					outcome.kind,
					outcome.kind === 'failed' ? errorMessage(outcome.error) : null
				);
			}
		} finally {
			busy = false;
		}
	}

	/**
	 * 明示操作を始めるときの共通の前準備。`run()` と、予算を 2 つに分けた
	 * `saveSelection()` の両方から呼ぶ（同じ初期化が 2 箇所にぶら下がるのを防ぐ）。
	 *
	 * **`statusUnconfirmedNotice` はここで消さない**: あれは「直前の操作で何が
	 * 起きたか」ではなく「今の接続先を確認できていない」という継続中の状態で、
	 * 読み直せたときだけ降りる。
	 */
	function beginRun(): void {
		busy = true;
		hubError = null;
		savedNotice = null;
		// 破棄の通知は「直前の操作で何が起きたか」なので、次の操作を始める
		// ときに畳む（`applyView` がこの後で立て直す）。
		selectionDiscardedNotice = null;
	}

	/**
	 * 各操作の共通の包み: 二重実行を防ぎ、失敗を 1 箇所で文言化し、
	 * **`HUB_UI_TIMEOUT_MS` で必ず有限時間に戻す**。
	 *
	 * 上限が無かったとき、応答しないアプリに当たると `busy` が降りず**画面が
	 * 操作不能になった**（復旧はアプリの再起動のみ）。上限で戻るようにしたので
	 * `finally` が必ず `busy` を降ろす。
	 *
	 * `action` が `signal` を受け取るのは 2 つの目的から: REST 経路に渡して
	 * 往復を畳むためと、**打ち切ったあとに遅れて解決した応答で画面を書き換え
	 * ないため**（Tauri の `invoke` は中断できないので、各 action は状態を
	 * 触る前に `signal.aborted` を見る）。見ないと、打ち切りの文言を出した
	 * あとに「保存しました」が上書きで出る。
	 */
	async function run(action: (signal: AbortSignal) => Promise<void>): Promise<void> {
		beginRun();
		try {
			const outcome = await runWithLimit(action, HUB_UI_TIMEOUT_MS);
			// 打ち切りは**失敗ではない**ので、通常のエラー文言と混ぜない。
			if (outcome.kind === 'failed') hubError = errorMessage(outcome.error);
			else if (outcome.kind === 'timedOut') await rereadAfterAbandon();
		} finally {
			busy = false;
		}
	}

	$effect(() => {
		if (!available) return;
		void run(async (signal) => {
			const view = await getHubStatus(signal);
			if (signal.aborted) return;
			applyView(view);
		});
	});

	async function connect(): Promise<void> {
		await run(async (signal) => {
			// レビュー P2-3 の続き: `connect` は Hub 側にキーを発行・保存する
			// **非冪等**な操作で、打ち切っても止まらない（`hub.rs` の
			// `HUB_MUTATING_TIMEOUT` の「残留リスク」と、#395 で直した P1-2 =
			// コード上の「#394 のレビュー P1-2」に同じ話がある）。打ち切った
			// ときに「接続できませんでした」と言い切ると、**Hub 側に残った
			// キーの存在が利用者に見えなくなる**ので、文言は
			// `hubAbandonedDisplay` の「操作は続いている可能性があります」を
			// 使う（`run()` が出す）。
			const view = await connectHub(endpointDraft, signal);
			if (signal.aborted) return;
			applyView(view);
		});
	}

	async function adopt(): Promise<void> {
		await run(async (signal) => {
			// `connect` と同じく非冪等（キーリングと設定への保存まで進む）。
			// 打ち切りの扱いは `connect` のコメント参照。
			const view = await adoptHubKey(endpointDraft, manualKeyDraft, signal);
			if (signal.aborted) return;
			applyView(view);
			// 採用できたときだけ入力欄を空にする（失敗時に貼り直させない）。
			if (view.status.state === 'connected') manualKeyDraft = '';
		});
	}

	async function refresh(): Promise<void> {
		await run(async (signal) => {
			const view = await refreshHubCatalog(signal);
			if (signal.aborted) return;
			applyView(view);
		});
	}

	/**
	 * 選択の保存（オーナーレビュー P2-2 で `run()` を通さなくなった）。
	 *
	 * **保存本体と、保存後の購読の読み直しで予算を分ける**。以前は両方が同じ
	 * `runWithLimit(action, HUB_UI_TIMEOUT_MS)` の内側にあり、保存に成功しても
	 * 読み直しだけが返らないと操作全体が `timedOut` になって
	 * `rereadAfterAbandon()` まで進んだ - 「保存しました」と「操作は続いている
	 * 可能性があります」が同時に出ていた。読み直しにも上限を足して全体の枠内に
	 * 残すだけでは、保存本体が上限近くまでかかったケースで同じことが起きる。
	 *
	 * 読み直しの失敗・打ち切りは**購読状態の取得失敗**として数える
	 * （ポーリングと同じカウンタに合流させる）。保存の打ち切り通知には進めない。
	 */
	async function saveSelection(): Promise<void> {
		beginRun();
		// 保存は view を返さない（204）が、設定も購読も変える明示操作。
		// 保存中に飛んでいたポーリング応答を捨てるために番号を進める。
		beginExplicitChange();
		const saving = [...selected];
		try {
			const outcome = await saveSelectionWithLimits(
				(signal) => setHubSelectedTags(saving, signal),
				(signal) => getHubSubscription(signal),
				() => {
					// 保存できた内容がサーバー側の選択になり、未保存の変更は無くなる。
					// **ここで明示操作の成否は確定**（後続の読み直しは影響しない）。
					serverSelected = saving;
					selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'saved');
					savedNotice = `選択したタグ（${saving.length}件）を保存しました。`;
				}
			);
			// 打ち切りは**失敗ではない**ので、通常のエラー文言と混ぜない。
			if (outcome.save.kind === 'failed') hubError = errorMessage(outcome.save.error);
			else if (outcome.save.kind === 'timedOut') await rereadAfterAbandon();

			// バックエンドは保存時に古い世代を止めている。次のポーリング
			// （最大 2 秒）まで停止済みの古い値を「受信中」として出し続けない
			// よう、ここで取り直して反映する（意図は従来どおり）。
			const reread = outcome.subscriptionReread;
			if (reread !== null) {
				if (reread.kind === 'ok') applySubscription(reread.value);
				// 失敗・打ち切りは「購読状態を取得できていない」。ポーリングと同じ
				// カウンタに合流させる（保存の成功表示は消さず、表示は次の
				// ポーリングで追いつく）。
				else pollFailures = nextPollFailureCount(pollFailures, pollFailureOutcome(reread));
			}
		} finally {
			busy = false;
		}
	}

	async function disconnect(): Promise<void> {
		await run(async (signal) => {
			// レビュー P2-B: 切断は接続レコードごと消す（選択の保存先が無く
			// なる）ので、未保存の選択も一緒に破棄してよい - 残しても戻す先が
			// 無い。`applyView` の前に降ろすのは、サーバーが返す空の選択を
			// そのまま反映させるため。
			//
			// レビュー P2-1: ただし**成功してから状態を落とす**。`await` の前に
			// 降ろすと、切断が失敗したときに未保存の注記だけが消え（エラーは
			// 出るが選択は編集中のまま）、次の「一覧を更新」でサーバーの選択に
			// 黙って上書きされる - この PR で塞いだはずの穴が失敗経路に残る。
			//
			// 打ち切ったときも同じ理由で状態を落とさない（切断できたか分からない
			// のに未保存の注記だけ消える、という同じ穴になる）。
			const view = await disconnectHub(signal);
			if (signal.aborted) return;
			selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'disconnected');
			applyView(view);
			tags = null;
			configured = false;
			endpointDraft = '';
		});
	}

	function toggleTag(externalName: string, checked: boolean): void {
		selected = checked
			? [...selected, externalName]
			: selected.filter((name) => name !== externalName);
		selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'edited');
	}

	/** レビュー P2-B: 未保存の選択を**明示的に**捨てる導線（黙って捨てない代わり）。 */
	function discardSelection(): void {
		selected = [...serverSelected];
		selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'discarded');
	}

	// --- #383 段階1: 購読状態のポーリング -----------------------------------

	/**
	 * 次の読み取りの予約。`null` は「回していない」。
	 *
	 * `setInterval` ではなく**完了してから次を予約する自己再帰**にしている:
	 * 一定間隔で投げると、遅れた古い応答が新しい応答のあとに着いて
	 * `subscription` を巻き戻せる（LAN の HTTP 経路で起きやすい）。1 本ずつ
	 * 直列に読めば、着順と発行順が入れ替わらない。
	 */
	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	/** 回している間だけ true。二重に走り始めるのを防ぐ。 */
	let polling = false;
	/**
	 * ポーリングの世代。`stopPolling()` のたびに進む。飛行中だった要求は
	 * 送信時の世代を持ち、**応答の適用も次回の予約も**世代が現役のときしか
	 * しない（`isPollGenerationCurrent`）。これが無いと、停止した瞬間に
	 * 飛んでいた要求が解決したときに次のタイマを張ってしまい、再開後の
	 * ループと二重に回り続ける。
	 */
	let pollGeneration = 0;

	/**
	 * 明示操作として購読状態を反映する（`applyView` と同じくシーケンス番号を
	 * 進める - 反映経路を 1 本にして番号の扱いを揃えるため）。
	 */
	function applySubscription(next: HubSubscription): void {
		beginExplicitChange();
		subscription = next;
		markSubscriptionFresh();
	}

	/**
	 * 購読状態だけを読み直す。**このページを開いている間だけ**回す。
	 *
	 * 失敗は**トーストにしない**（ポーリングの一時的な失敗で明示操作の
	 * エラー表示を上書きしないという既存の設計）。ただし握り潰したままに
	 * すると、サーバープロセスが落ちる・LAN が切れるなどで**恒久的に失敗
	 * しても「受信中」＋最後の値を出し続ける**（この画面には自動で明示操作を
	 * 起こす経路が無いので、ユーザーがボタンを押すまで嘘が続く）。連続失敗を
	 * 数え、`SUBSCRIPTION_POLL_FAILURE_LIMIT` 回で画面のブロック内に
	 * 「取得できていません」を出す（レビュー P2-A）。
	 *
	 * レビュー P2-3: 失敗を数えるだけでは**即座に失敗する障害にしか効かない**。
	 * TCP は繋がるが応答が返らない相手だと `catch` に入らず、数も増えないまま
	 * ここで止まる。`readSubscriptionWithLimit` で 1 回の読み取りに上限
	 * （`SUBSCRIPTION_POLL_TIMEOUT_MS`）を置き、**必ず有限時間で戻る**ように
	 * した（打ち切りは失敗として数える）。REST 経路は `AbortSignal` で実際に
	 * 往復を畳むが、**Tauri の `invoke` は中断できない**ので、そちらは
	 * 「打ち切り済み」フラグで遅れた応答を採用しないことだけを保証する。
	 */
	async function pollSubscription(generation: number): Promise<void> {
		const sentAt = appliedSeq;
		const outcome = await readSubscriptionWithLimit((signal) => getHubSubscription(signal));
		// 停止（や停止→再開）を跨いだ応答は自分のものではない。止まった世代の
		// 失敗まで数えると、再開後に前回の失敗が持ち越される。
		if (!isPollGenerationCurrent(generation, pollGeneration)) return;
		if (outcome.kind === 'ok') {
			// 読めた事実は、その値を採用するかどうかとは別（明示操作の結果を
			// 優先して捨てる場合でも、購読状態は取得できている）。
			markSubscriptionFresh();
			// 待っている間に明示操作の結果が入っていたら、こちらは古い。
			if (isPollResultFresh(sentAt, appliedSeq)) subscription = outcome.value;
			return;
		}
		// 失敗・打ち切りはどちらも「今の状態を取得できていない」。トーストは
		// 出さない（上のコメント参照）。
		pollFailures = nextPollFailureCount(pollFailures, pollFailureOutcome(outcome));
	}

	/**
	 * 1 回読んでから次を予約する。停止されていたら予約しない。
	 *
	 * 予約が `pollSubscription()` の**完了に依存している**ので、読み取りが
	 * 戻らないとループごと止まる（レビュー P2-3 の本体）。上限で必ず戻るよう
	 * にしたことで、タイムアウトしても次のポーリングは必ず予約される。
	 */
	async function pollThenSchedule(generation: number): Promise<void> {
		await pollSubscription(generation);
		if (!isPollGenerationCurrent(generation, pollGeneration)) return;
		pollTimer = setTimeout(() => void pollThenSchedule(generation), SUBSCRIPTION_POLL_MS);
	}

	function stopPolling(): void {
		// 世代を進める = 飛行中の要求はもう予約も適用もしない。
		pollGeneration += 1;
		polling = false;
		if (pollTimer !== null) {
			clearTimeout(pollTimer);
			pollTimer = null;
		}
	}

	/**
	 * 即時に 1 回読んでから回し始める。`$effect` の `getHubStatus()` が
	 * 失敗すると `subscription` が埋まらないので、この 1 回目が無いと
	 * 購読ブロックが最初の周期まで出ない。
	 */
	function startPolling(): void {
		if (!available || polling) return;
		polling = true;
		void pollThenSchedule(pollGeneration);
	}

	/** タブが隠れている間は止める（見ていない画面のために回し続けない）。 */
	function onVisibilityChange(): void {
		if (document.visibilityState === 'visible') {
			startPolling();
		} else {
			stopPolling();
		}
	}

	onMount(() => {
		if (!available) return;
		document.addEventListener('visibilitychange', onVisibilityChange);
		if (document.visibilityState === 'visible') startPolling();
	});

	onDestroy(() => {
		stopPolling();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', onVisibilityChange);
		}
	});
</script>

{#if isAdmin(sessionStore.role)}
	<section>
		<h2>Hub接続</h2>

		{#if available}
			<div class="server-fields">
				<label class="field hub-endpoint">
					接続先URL
					<input
						type="url"
						placeholder="http://127.0.0.1:3100"
						bind:value={endpointDraft}
						disabled={busy}
					/>
				</label>
			</div>

			<!--
				オーナーレビュー P2-1: 状態を再確認できていない間は「接続」も止める。
				打ち切った `connect` がまだ走っているかもしれない状態で押し直せると、
				Hub 側にキーを二重に発行しうる。
			-->
			<button
				type="button"
				onclick={connect}
				disabled={busy || statusUnconfirmed || endpointDraft.trim() === ''}
			>
				接続
			</button>

			<p class="status">
				状態: <strong>{hubStatusLabel(status)}</strong>
			</p>
			<p class="note">{hubStatusDetail(status)}</p>

			{#if keyName}
				<p class="note">このアプリのAPIキー名: <code>{keyName}</code></p>
			{/if}

			{#if hubError}
				<p class="error">{hubError}</p>
			{/if}

			<!--
				オーナーレビュー P2-1: 打ち切りのあと状態も読み直せなかったときは、
				警告を出すだけでなく**接続先に依存する変更操作を止める**。警告は
				`hubError` とは別に持つ（`run()` が冒頭で `hubError` を消すので、
				次の操作を始めた瞬間に消えて止める役割を果たさない）。
				**画面全体は操作不能にしない** - 読み取り専用の「状態を再取得」で
				必ず抜けられる。
			-->
			{#if statusUnconfirmedNotice}
				<p class="note status-unconfirmed" role="status">{statusUnconfirmedNotice}</p>
			{/if}
			{#if statusUnconfirmed}
				<div class="hub-actions">
					<button type="button" onclick={reconfirmStatus} disabled={busy}>状態を再取得</button>
				</div>
			{/if}

			<!--
				レビュー P2-2: 接続先が変わったときは未保存の下書きを捨てる（旧 Hub
				のタグ名を新しい Hub の保存に混ぜない）が、**捨てたことは黙らない**
				（#378）。接続に失敗して `connected` に入らない場合もあるので、
				タグ選択のブロック（`status.state === 'connected'` の中）ではなく
				ここに出す。保存成功の `savedNotice` とは別の行・別の文言。
			-->
			{#if selectionDiscardedNotice}
				<p class="note selection-discarded" role="status">{selectionDiscardedNotice}</p>
			{/if}

			{#if showManualKeyEntry(status, subscription)}
				<div class="server-fields">
					<label class="field hub-endpoint">
						APIキー（Hubの管理画面で発行したもの）
						<input type="password" autocomplete="off" bind:value={manualKeyDraft} disabled={busy} />
					</label>
				</div>
				<!--
					「接続」と同じ理由で、状態を再確認できていない間は採用も止める
					（打ち切った採用がまだ走っていれば、押し直しはキーの二重発行）。
				-->
				<button
					type="button"
					onclick={adopt}
					disabled={busy ||
						statusUnconfirmed ||
						manualKeyDraft.trim() === '' ||
						endpointDraft.trim() === ''}
				>
					このキーを採用
				</button>
			{/if}

			{#if status.state === 'connected'}
				<div class="hub-actions">
					<button type="button" onclick={refresh} disabled={busy || statusUnconfirmed}>
						一覧を更新
					</button>
					<button type="button" onclick={saveSelection} disabled={busy || statusUnconfirmed}>
						選択を保存
					</button>
				</div>

				<!--
					レビュー P2-B: 未保存の選択を黙って捨てない。「一覧を更新」や
					「接続」でサーバーの選択に戻さなかったことをここで伝え、
					**明示的に捨てる導線**を隣に置く。
				-->
				{#if selectionDiffers}
					<p class="note selection-unsaved" role="status">
						選択に未保存の変更があります（サーバー側の選択と異なります）。「選択を保存」で保存するか、
						<!--
							オーナーレビュー P2-1 の留保対応: 「サーバーの内容に戻す」は
							バックエンドを叩かないが、`selected` を `serverSelected` で
							上書きして未保存フラグを降ろす（`discardSelection`）。
							`statusUnconfirmed` のときは、その `serverSelected` が**今の
							接続先のものだと確認できていない**（打ち切った `connect` が
							裏で通っていれば、画面が持つのは旧 Hub の保存済み選択）。
							押すと、利用者は「保存済みの状態に戻した」つもりで**別の Hub
							の選択を下書きとして受け入れてしまう**。チェックボックス
							（下の `disabled={busy || statusUnconfirmed}`）を止めた理由と
							同じ - 接続先を確認できるまで、選択については何も確定させない。
						-->
						<button
							type="button"
							class="link"
							onclick={discardSelection}
							disabled={busy || statusUnconfirmed}
						>
							サーバーの内容に戻す
						</button>
						を押してください。
					</p>
				{/if}

				{#if tags && tags.length > 0}
					<ul class="hub-tags">
						{#each tags as tag (tag.externalName)}
							<li>
								<label class="toggle">
									<!--
										オーナーレビュー P2-1: どの接続先のタグ一覧なのか確認
										できていない間は、下書きも触らせない（保存はこの
										チェックから組み直さず `selected` をそのまま送る）。
									-->
									<input
										type="checkbox"
										checked={selected.includes(tag.externalName)}
										disabled={busy || statusUnconfirmed}
										onchange={(event) => toggleTag(tag.externalName, event.currentTarget.checked)}
									/>
									<span class="hub-tag-name">{tag.externalName}</span>
									<span class="hub-tag-meta">
										{tag.dataType}{tag.unit ? ` / ${tag.unit}` : ''}
									</span>
								</label>
							</li>
						{/each}
					</ul>
				{:else if tags}
					<p class="note">Hubに登録されているタグがありません。</p>
				{/if}

				{#if savedNotice}
					<p class="note">{savedNotice}</p>
				{/if}
			{/if}

			{#if subscription}
				<h3 class="hub-subheading">購読</h3>
				<p class="status">
					購読: <strong>{hubSubscriptionHeadline(subscription.state, subscriptionStale)}</strong>
				</p>
				<!--
					レビュー P2-A: ポーリングが 2 回連続で失敗したら、この表示が
					今の状態ではないと言う。トーストは出さない（明示操作の
					エラー表示を上書きしない）ので、このブロックの中で示す。
					値の表は消さない - 消すと「0 件」に潰れて別の嘘になる。
				-->
				{#if subscriptionStale}
					<p class="note poll-stale" role="status">{hubPollStaleNote(lastPolledAt)}</p>
				{/if}
				<p class="note">{hubSubscriptionDetail(subscription)}</p>
				<!--
					購読全体の最終受信時刻。値の表の行ごとの `t` は
					「この行がいつの値か」であって、購読が生きているかの
					目安にはならないので別に出す。
				-->
				<p class="note">
					最終受信: {hubLastValueLabel(subscription.lastValueAt, subscription.state)}
				</p>

				<!--
					`hubSubscriptionDetail` が「停止（エラー: …）」としてエラーを
					すでに述べている場合だけ、この行を省く（同じことを 2 行続けて
					読ませない）。それ以外では、説明文の補足として型名を出す。
				-->
				{#if subscription.lastError && showLastErrorLine}
					<p class="note">直近のエラー: <code>{subscription.lastError}</code></p>
				{/if}

				{#if subscription.unresolved.length > 0}
					<p class="note">
						次のタグは見つかりませんでした（Hubから消えたか、権限で見えないタグです）。{hubRemainderNote(
							subscription.subscribedCount
						)}
					</p>
					<ul class="hub-unresolved">
						{#each subscription.unresolved as name (name)}
							<li><span class="hub-tag-name">{name}</span></li>
						{/each}
					</ul>
				{/if}

				<!--
					「購読できないタグ」は「Hubから消えた」とは理由も直し方も
					違う（Hub側のタグ定義を直す vs Hubにタグを戻す）ので、同じ
					一覧に混ぜない。
				-->
				{#if subscription.unsupported.length > 0}
					<p class="note">
						次のタグはそのままでは購読できません（名前にカンマを含む、他のタグと同じタグを指しているなど）。Hub側のタグ定義を確認してください。{hubRemainderNote(
							subscription.subscribedCount
						)}
					</p>
					<ul class="hub-unresolved">
						{#each subscription.unsupported as name (name)}
							<li><span class="hub-tag-name">{name}</span></li>
						{/each}
					</ul>
				{/if}

				{#if subscription.values.length > 0}
					<table class="hub-values">
						<thead>
							<tr>
								<th scope="col">タグ</th>
								<th scope="col">値</th>
								<th scope="col">品質</th>
								<th scope="col">時刻</th>
							</tr>
						</thead>
						<tbody>
							{#each subscription.values as value (value.tag)}
								<tr>
									<td class="hub-tag-name">{value.tag}</td>
									<td>{value.v ?? '—'}</td>
									<td>{value.q}</td>
									<td>{hubTimeLabel(value.t)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				{:else}
					<p class="note">値を受信していません。</p>
				{/if}
			{/if}

			{#if configured}
				<div class="hub-actions">
					<button type="button" onclick={disconnect} disabled={busy || statusUnconfirmed}>
						切断
					</button>
				</div>
				<p class="note">
					「切断」はこのアプリの設定と保存済みAPIキーだけを削除します。Hub側のAPIキーは失効しません（必要ならHubの管理画面で失効させてください）。
				</p>
			{/if}

			<p class="note">
				Hubが試運転モード（ロックダウン前）なら、読み取り専用のAPIキーを自動で発行して安全に保管します。試運転モードのHubは同じPC上でしか待ち受けないため、この自動発行は同一PCに限られます。ロックダウン済みのHubには自動発行せず、管理者が発行したAPIキーの手入力に切り替わります。
			</p>
		{:else}
			<p class="note">
				Hub接続はデスクトップアプリ、またはデスクトップアプリが公開しているLANサーバー経由でのみ設定できます。
			</p>
		{/if}
	</section>
{/if}

<style>
	.hub-endpoint {
		flex: 1 1 22rem;
	}

	.hub-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin-top: 0.75rem;
	}

	.hub-tags {
		margin: 0.75rem 0 0;
		padding: 0;
		list-style: none;
		max-height: 18rem;
		overflow-y: auto;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.hub-tags li {
		padding: 0.3rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.hub-tags li:last-child {
		border-bottom: none;
	}

	.hub-tag-name {
		font-family: var(--banto-font-mono, monospace);
	}

	.hub-tag-meta {
		margin-left: auto;
		color: var(--banto-text-muted);
		font-size: 0.75rem;
	}

	/*
		レビュー P2-A / P2-B / P2-2 と、オーナーレビュー P2-1 の注記。いずれも
		「今の表示が正しくない／保存されていない／捨てた／接続先を確認できて
		いない」という注意喚起なので、既存の `.note`（薄いグレー）
		より目に入る色にする。エラー（`.error`）ではないので `--banto-danger`
		は使わない。
	*/
	.poll-stale,
	.selection-unsaved,
	.selection-discarded,
	.status-unconfirmed {
		color: var(--banto-text);
	}

	/*
		`.settings-layout button` は主ボタン（塗り）なので、文中に置く
		「サーバーの内容に戻す」はリンク風に上書きする（文章の流れを
		主ボタンで分断しない）。
	*/
	.selection-unsaved button.link {
		padding: 0;
		background: none;
		border: none;
		color: var(--banto-primary);
		font: inherit;
		font-weight: 600;
		text-decoration: underline;
		cursor: pointer;
	}

	.selection-unsaved button.link:hover:not(:disabled) {
		background: none;
	}

	.selection-unsaved button.link:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.hub-subheading {
		margin: 1.25rem 0 0;
		font-size: 0.95rem;
	}

	.hub-unresolved {
		margin: 0.25rem 0 0;
		padding-left: 1.25rem;
	}

	.hub-values {
		margin-top: 0.5rem;
		border-collapse: collapse;
		width: 100%;
		max-width: 40rem;
	}

	.hub-values th,
	.hub-values td {
		text-align: left;
		padding: 0.25rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
		font-size: 0.8rem;
	}
</style>
