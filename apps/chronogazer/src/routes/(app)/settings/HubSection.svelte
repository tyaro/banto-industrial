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
	 *   しない。代わりに注記と「サーバーの内容に戻す」を出す。
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
		hubLastValueLabel,
		hubPollStaleNote,
		hubRemainderNote,
		hubStatusDetail,
		hubStatusLabel,
		hubSubscriptionDetail,
		hubSubscriptionHeadline,
		hubTimeLabel,
		isHubAvailable,
		isPollGenerationCurrent,
		isPollResultFresh,
		isSubscriptionStale,
		nextPollFailureCount,
		nextSelectionUnsaved,
		refreshHubCatalog,
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
		selected = applyServerSelection(selected, view.selectedTags, selectionUnsaved);
		// `null`（この往復では catalog を読めていない）と `[]`（読めた結果
		// タグ 0 件）は別物。前者では前回の一覧を残さず消す - 状態表示の
		// 「接続済み・利用可能なタグなし」と食い違わせないため。
		tags = view.tags;
		subscription = view.subscription;
		markSubscriptionFresh();
		if (view.endpoint) endpointDraft = view.endpoint;
	}

	/** 各操作の共通の包み: 二重実行を防ぎ、失敗を 1 箇所で文言化する。 */
	async function run(action: () => Promise<void>): Promise<void> {
		busy = true;
		hubError = null;
		savedNotice = null;
		try {
			await action();
		} catch (err) {
			hubError = errorMessage(err);
		} finally {
			busy = false;
		}
	}

	$effect(() => {
		if (!available) return;
		void run(async () => {
			applyView(await getHubStatus());
		});
	});

	async function connect(): Promise<void> {
		await run(async () => {
			applyView(await connectHub(endpointDraft));
		});
	}

	async function adopt(): Promise<void> {
		await run(async () => {
			const view = await adoptHubKey(endpointDraft, manualKeyDraft);
			applyView(view);
			// 採用できたときだけ入力欄を空にする（失敗時に貼り直させない）。
			if (view.status.state === 'connected') manualKeyDraft = '';
		});
	}

	async function refresh(): Promise<void> {
		await run(async () => {
			applyView(await refreshHubCatalog());
		});
	}

	async function saveSelection(): Promise<void> {
		await run(async () => {
			// 保存は view を返さない（204）が、設定も購読も変える明示操作。
			// 保存中に飛んでいたポーリング応答を捨てるために番号を進める。
			beginExplicitChange();
			const saving = [...selected];
			await setHubSelectedTags(saving);
			// 保存できた内容がサーバー側の選択になり、未保存の変更は無くなる。
			serverSelected = saving;
			selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'saved');
			savedNotice = `選択したタグ（${saving.length}件）を保存しました。`;
			// バックエンドは保存時に古い世代を止めている。次のポーリング
			// （最大 2 秒）まで停止済みの古い値を「受信中」として出し続けない
			// よう、ここで取り直して反映する。読み直しの失敗は保存の失敗では
			// ないので、保存の成功表示を消さずに捨てる（表示は次のポーリングで
			// 追いつく）。
			try {
				applySubscription(await getHubSubscription());
			} catch {
				// 握りつぶす（上のコメント参照）。
			}
		});
	}

	async function disconnect(): Promise<void> {
		await run(async () => {
			// レビュー P2-B: 切断は接続レコードごと消す（選択の保存先が無く
			// なる）ので、未保存の選択も一緒に破棄してよい - 残しても戻す先が
			// 無い。`applyView` の前に降ろすのは、サーバーが返す空の選択を
			// そのまま反映させるため。
			selectionUnsaved = nextSelectionUnsaved(selectionUnsaved, 'disconnected');
			applyView(await disconnectHub());
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
	 */
	async function pollSubscription(generation: number): Promise<void> {
		const sentAt = appliedSeq;
		try {
			const polled = await getHubSubscription();
			// 停止（や停止→再開）を跨いだ応答は自分のものではない。
			if (!isPollGenerationCurrent(generation, pollGeneration)) return;
			// 読めた事実は、その値を採用するかどうかとは別（明示操作の結果を
			// 優先して捨てる場合でも、購読状態は取得できている）。
			markSubscriptionFresh();
			// 待っている間に明示操作の結果が入っていたら、こちらは古い。
			if (isPollResultFresh(sentAt, appliedSeq)) subscription = polled;
		} catch {
			// トーストは出さない（上のコメント参照）。止まった世代の失敗まで
			// 数えると、再開後に前回の失敗が持ち越される。
			if (!isPollGenerationCurrent(generation, pollGeneration)) return;
			pollFailures = nextPollFailureCount(pollFailures, 'failed');
		}
	}

	/** 1 回読んでから次を予約する。停止されていたら予約しない。 */
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

			<button type="button" onclick={connect} disabled={busy || endpointDraft.trim() === ''}>
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

			{#if showManualKeyEntry(status, subscription)}
				<div class="server-fields">
					<label class="field hub-endpoint">
						APIキー（Hubの管理画面で発行したもの）
						<input type="password" autocomplete="off" bind:value={manualKeyDraft} disabled={busy} />
					</label>
				</div>
				<button
					type="button"
					onclick={adopt}
					disabled={busy || manualKeyDraft.trim() === '' || endpointDraft.trim() === ''}
				>
					このキーを採用
				</button>
			{/if}

			{#if status.state === 'connected'}
				<div class="hub-actions">
					<button type="button" onclick={refresh} disabled={busy}>一覧を更新</button>
					<button type="button" onclick={saveSelection} disabled={busy}>選択を保存</button>
				</div>

				<!--
					レビュー P2-B: 未保存の選択を黙って捨てない。「一覧を更新」や
					「接続」でサーバーの選択に戻さなかったことをここで伝え、
					**明示的に捨てる導線**を隣に置く。
				-->
				{#if selectionDiffers}
					<p class="note selection-unsaved" role="status">
						選択に未保存の変更があります（サーバー側の選択と異なります）。「選択を保存」で保存するか、
						<button type="button" class="link" onclick={discardSelection} disabled={busy}>
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
									<input
										type="checkbox"
										checked={selected.includes(tag.externalName)}
										disabled={busy}
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
					<button type="button" onclick={disconnect} disabled={busy}>切断</button>
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
		レビュー P2-A / P2-B の 2 つの注記。どちらも「今の表示が正しくない／
		保存されていない」という注意喚起なので、既存の `.note`（薄いグレー）
		より目に入る色にする。エラー（`.error`）ではないので `--banto-danger`
		は使わない。
	*/
	.poll-stale,
	.selection-unsaved {
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
