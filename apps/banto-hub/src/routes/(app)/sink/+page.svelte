<script lang="ts">
	/**
	 * S6（docs/banto-hub-external-db-design.md §5.2・§5.3・§5.6・§7 row S6）:
	 * DB Sink（`hub_sink_groups`）の管理画面。sink group の一覧・作成・編集・
	 * 削除（`SinkGroupDrawer.svelte`）、サイドカー用 API キーの発行導線、
	 * `banto-hub-sink.toml`の設定スニペット表示を1画面にまとめる。
	 *
	 * 稼働状態（サイドカーの online/unknown・各 group の queued/dropped 等）
	 * はここでは表示しない - `status/+page.svelte`の「DB Sink」節が担う
	 * （表示規約の二重管理を避ける、UX-30 と同じ理由）。
	 *
	 * サイドカーは別プロセス（`apps/banto-hub-sink`）なので、この画面から
	 * インストール・起動・停止はできない - Windows サービスとしての起動停止は
	 * `status/+page.svelte`の「サービス一覧」（ローカルシェル限定）が担う。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources, isAdmin } from '$lib/permissions';
	import SinkGroupDrawer from '$lib/components/SinkGroupDrawer.svelte';
	import {
		listPlcConnections,
		listCollectionGroups,
		listTags,
		isDbSourceConnection,
		type PlcConnection,
		type CollectionGroup,
		type Tag
	} from '$lib/banto/tagRegistryAdmin';
	import { listSinkGroups, deleteSinkGroup, type SinkGroup } from '$lib/banto/sinkGroupsAdmin';
	import { SINK_MODE_OPTIONS } from '$lib/banto/sinkGroupForm';

	const canWrite = $derived(canWriteResources(sessionStore.role));
	const hubAdmin = $derived(isAdmin(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let sinkGroups: SinkGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let collectionGroups: CollectionGroup[] = $state([]);
	let tags: Tag[] = $state([]);
	let loading = $state(true);
	let loadError: string | null = $state(null);

	async function reload(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			const [nextSinkGroups, nextConnections, nextGroups, nextTags] = await Promise.all([
				listSinkGroups(),
				listPlcConnections(),
				listCollectionGroups(),
				listTags()
			]);
			sinkGroups = nextSinkGroups;
			connections = nextConnections;
			collectionGroups = nextGroups;
			tags = nextTags;
		} catch (err) {
			loadError = errorMessage(err);
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	const dbConnectionOptions = $derived(connections.filter(isDbSourceConnection));

	function connectionName(id: number): string {
		return connections.find((c) => c.id === id)?.name ?? `#${id}`;
	}

	function modeLabel(mode: string): string {
		return SINK_MODE_OPTIONS.find((o) => o.value === mode)?.label ?? mode;
	}

	// --- Drawer ---
	let drawerOpen = $state(false);
	let editingGroup: SinkGroup | null = $state(null);

	function openCreate(): void {
		editingGroup = null;
		drawerOpen = true;
	}

	function openEdit(group: SinkGroup): void {
		editingGroup = group;
		drawerOpen = true;
	}

	function closeDrawer(): void {
		drawerOpen = false;
	}

	function handleSaved(saved: SinkGroup): void {
		const index = sinkGroups.findIndex((g) => g.id === saved.id);
		if (index >= 0) {
			sinkGroups = sinkGroups.map((g) => (g.id === saved.id ? saved : g));
		} else {
			sinkGroups = [...sinkGroups, saved].sort((a, b) => a.name.localeCompare(b.name));
		}
	}

	function handleDeleted(id: number): void {
		sinkGroups = sinkGroups.filter((g) => g.id !== id);
	}

	// --- 既存の確認パターン（一覧行の削除ボタン - Drawer を開かず直接削除する軽量操作） ---
	let deletingId: number | null = $state(null);

	async function handleQuickDelete(group: SinkGroup): Promise<void> {
		if (!window.confirm(`sink group '${group.name}' を削除しますか？`)) return;
		deletingId = group.id;
		try {
			await deleteSinkGroup(group.id);
			toastStore.push('success', '削除しました');
			handleDeleted(group.id);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			deletingId = null;
		}
	}

	// --- サイドカー用 API キー（設計 §5.2・実装指示3） ---
	const SINK_API_KEY_PRESET_NAME = 'banto-hub-sink';
	const sinkApiKeyIssueHref = `/api-keys?presetName=${encodeURIComponent(SINK_API_KEY_PRESET_NAME)}&presetScopes=admin,read`;
</script>

<div class="page">
	<section>
		<h2>DB Sink</h2>
		<p class="note">
			ここで設定した内容は即時に反映されます（保留中の変更キューには載りません）。サイドカー （<code
				>banto-hub-sink</code
			>）はこの変更を <code>config_refresh_secs</code>（既定30秒）以内に
			取り込みます。サイドカー自体の起動・停止は
			<a href="/status#collection-control">状態画面</a>の「サービス一覧」から行えます。
		</p>

		{#if loading && sinkGroups.length === 0}
			<p class="note">読み込み中…</p>
		{:else if loadError}
			<p class="config-error">{loadError}</p>
		{/if}

		{#if canWrite}
			<button type="button" onclick={openCreate}>新規作成</button>
		{/if}

		{#if !loading && sinkGroups.length === 0 && !loadError}
			<p class="note">sink group が登録されていません。</p>
		{:else if sinkGroups.length > 0}
			<table class="sink-table">
				<thead>
					<tr>
						<th>名前</th>
						<th>DB 接続</th>
						<th>mode</th>
						<th>間隔(ms)</th>
						<th>保存先テーブル</th>
						<th>Bad/Stale保存</th>
						<th>有効</th>
						<th>タグ数</th>
						<th></th>
					</tr>
				</thead>
				<tbody>
					{#each sinkGroups as group (group.id)}
						<tr>
							<td>{group.name}</td>
							<td>{connectionName(group.dbConnectionId)}</td>
							<td>{modeLabel(group.mode)}</td>
							<td>{group.intervalMs}</td>
							<td><code>{group.tableName}</code></td>
							<td>{group.storeBad ? 'はい' : 'いいえ'}</td>
							<td>{group.enabled ? '有効' : '無効'}</td>
							<td>{group.tagIds.length}</td>
							<td class="actions">
								<button type="button" class="secondary" onclick={() => openEdit(group)}>
									{canWrite ? '編集' : '詳細'}
								</button>
								{#if canWrite}
									<button
										type="button"
										class="danger"
										onclick={() => void handleQuickDelete(group)}
										disabled={deletingId === group.id}
									>
										削除
									</button>
								{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	</section>

	{#if hubAdmin}
		<section>
			<h2>サイドカー用 API キー</h2>
			<p class="note">
				<code>banto-hub-sink</code> サイドカーは <code>GET /api/sink/config</code>（DB
				接続のパスワードを含む設定取得）と <code>PUT /api/sink/status</code>（状態 push）の両方を
				呼ぶため、発行する API キーには <strong>admin と read の両方のスコープ</strong>
				が必要です（admin だけでは <code>/api/v1/*</code> の購読が 403 になります、2026-09-06訂正）。
			</p>
			<a class="issue-key-button" href={sinkApiKeyIssueHref}>
				APIキー発行画面を開く（admin + read プリセット済み）
			</a>
			<p class="note">
				発行直後の画面にしか平文キーは表示されません。コピーしたら
				<code>banto-hub-sink.toml</code>（<code>%ProgramData%\BantoHub\</code>、インストーラが
				雛形を用意します。無い場合はサイドカー exe と同じディレクトリでも可）へ設定してください。
			</p>
			<div class="config-snippet">
				<pre>hub_url = "http://127.0.0.1:&lt;Hub のポート&gt;"
api_key = "&lt;発行したキー&gt;"</pre>
			</div>
			<p class="note warning">
				⚠ <code>GET /api/sink/config</code> は DB 接続のパスワードを平文で返します。サイドカーは Hub と同一マシンでループバック
				（127.0.0.1）接続する運用にしてください。
			</p>
		</section>
	{/if}
</div>

<SinkGroupDrawer
	open={drawerOpen}
	group={editingGroup}
	existingNames={sinkGroups.map((g) => g.name)}
	connections={dbConnectionOptions}
	{collectionGroups}
	{tags}
	onClose={closeDrawer}
	onSaved={handleSaved}
	onDeleted={handleDeleted}
/>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	.note {
		margin: 0 0 0.75rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.note.warning {
		color: var(--banto-danger);
	}

	.config-error {
		margin: 0 0 0.75rem;
		padding: 0.5rem 0.7rem;
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
		margin-top: 0.75rem;
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

	td.actions {
		display: flex;
		gap: 0.4rem;
	}

	.config-snippet {
		margin-bottom: 0.75rem;
	}

	.config-snippet pre {
		margin: 0;
		padding: 0.6rem 0.75rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		font-size: 0.78rem;
		white-space: pre-wrap;
	}

	.issue-key-button {
		display: inline-block;
		margin-bottom: 0.75rem;
		padding: 0.5rem 1rem;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		text-decoration: none;
	}

	button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
		margin-bottom: 0.75rem;
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
		font-weight: 400;
		padding: 0.3rem 0.6rem;
		font-size: 0.8rem;
		margin-bottom: 0;
	}

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
		padding: 0.3rem 0.6rem;
		font-size: 0.8rem;
		margin-bottom: 0;
	}
</style>
