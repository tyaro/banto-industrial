<script lang="ts">
	/**
	 * Hub 接続カテゴリ（#332 chronogazer 分）。admin 限定（`+layout.ts` の
	 * `visible.hub` 参照）。
	 *
	 * chronogazer はこれまで banto-hub に接続するコードを持っていなかった
	 * ので、この section は既存画面の移設ではなく新規。やっていることは
	 * 「接続先の設定」「6 状態の表示」「タグ一覧と選択の保存」までで、
	 * **選んだタグをトレンド等のデータ源に繋ぐのは別 issue**（購読
	 * = `banto-tagclient` の `start()` はこの PR では一切呼ばない）。
	 *
	 * 平文の API キーは画面に出さない: 手動連携の入力欄は
	 * `type="password"`、応答型（`HubView`）にキー欄は無い。
	 */
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import {
		adoptHubKey,
		connectHub,
		disconnectHub,
		getHubStatus,
		hubStatusDetail,
		hubStatusLabel,
		isHubAvailable,
		needsManualKey,
		refreshHubCatalog,
		setHubSelectedTags,
		type HubStatus,
		type HubTag,
		type HubView
	} from '$lib/banto/hubAdmin';
	import { errorMessage } from './shared';

	const available = isHubAvailable();

	let status = $state<HubStatus>({ state: 'notConfigured' });
	let tags = $state<HubTag[] | null>(null);
	let selected = $state<string[]>([]);
	let keyName = $state<string | null>(null);
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

			{#if needsManualKey(status)}
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
</style>
