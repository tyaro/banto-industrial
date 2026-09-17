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
	 *
	 * 平文の API キーは画面に出さない: 手動連携の入力欄は
	 * `type="password"`、応答型（`HubView`）にキー欄は無い。
	 */
	import { onDestroy, onMount } from 'svelte';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import {
		adoptHubKey,
		connectHub,
		disconnectHub,
		getHubStatus,
		getHubSubscription,
		hubLastValueLabel,
		hubStatusDetail,
		hubStatusLabel,
		hubSubscriptionDetail,
		hubSubscriptionLabel,
		hubTimeLabel,
		isHubAvailable,
		refreshHubCatalog,
		setHubSelectedTags,
		showManualKeyEntry,
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
	let keyName = $state<string | null>(null);
	let subscription = $state<HubSubscription | null>(null);
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

	function applyView(view: HubView): void {
		status = view.status;
		configured = view.endpoint !== null;
		keyName = view.keyName;
		selected = [...view.selectedTags];
		// `null`（この往復では catalog を読めていない）と `[]`（読めた結果
		// タグ 0 件）は別物。前者では前回の一覧を残さず消す - 状態表示の
		// 「接続済み・利用可能なタグなし」と食い違わせないため。
		tags = view.tags;
		subscription = view.subscription;
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
			await setHubSelectedTags(selected);
			savedNotice = `選択したタグ（${selected.length}件）を保存しました。`;
		});
	}

	async function disconnect(): Promise<void> {
		await run(async () => {
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
	/** 回している間だけ true。予約前に見て、停止後の予約を防ぐ。 */
	let polling = false;

	/**
	 * 購読状態だけを読み直す。**このページを開いている間だけ**回し、失敗は
	 * 黙って捨てる（ポーリングの一時的な失敗で操作用のエラー表示を上書き
	 * しない。恒久的な失敗は次の明示操作で出る）。
	 */
	async function pollSubscription(): Promise<void> {
		try {
			subscription = await getHubSubscription();
		} catch {
			// 握りつぶす（上のコメント参照）。
		}
	}

	/** 1 回読んでから次を予約する。停止されていたら予約しない。 */
	async function pollThenSchedule(): Promise<void> {
		await pollSubscription();
		if (!polling) return;
		pollTimer = setTimeout(() => void pollThenSchedule(), SUBSCRIPTION_POLL_MS);
	}

	function stopPolling(): void {
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
		void pollThenSchedule();
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
					購読: <strong>{hubSubscriptionLabel(subscription.state)}</strong>
				</p>
				<p class="note">{hubSubscriptionDetail(subscription)}</p>
				<!--
					購読全体の最終受信時刻。値の表の行ごとの `t` は
					「この行がいつの値か」であって、購読が生きているかの
					目安にはならないので別に出す。
				-->
				<p class="note">最終受信: {hubLastValueLabel(subscription.lastValueAt)}</p>

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
						次のタグは見つかりませんでした（Hubから消えたか、権限で見えないタグです）。残りのタグだけを購読しています。
					</p>
					<ul class="hub-unresolved">
						{#each subscription.unresolved as name (name)}
							<li><span class="hub-tag-name">{name}</span></li>
						{/each}
					</ul>
				{/if}

				<!--
					「購読できない名前」は「Hubから消えた」とは理由も直し方も
					違う（名前を直す vs Hubにタグを戻す）ので、同じ一覧に混ぜない。
				-->
				{#if subscription.unsupported.length > 0}
					<p class="note">
						次のタグは購読できない名前です（カンマを含むなど、購読プロトコルが受け付けません）。残りのタグだけを購読しています。
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
