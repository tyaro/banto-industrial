<script lang="ts">
	/**
	 * 収集グループ（collection_groups）CRUD 画面。`plc-connections/+page.svelte`
	 * と同じシンプル型（BantoGrid一覧＋行クリックで Drawer）を反復した実装。
	 *
	 * T18-6b（TAG-UX-7/TAG-UX-8、2026-08-27 オーナー決定「収集グループの作成／
	 * 再設定を Drawer に寄せる」）: 作成・再設定のフォーム本体は
	 * `$lib/components/CollectionGroupDrawer.svelte` へ切り出した
	 * （`plc-connections` の T18-6a と同じ狙い - タグツリー右クリックからも
	 * 同じ部品を開けるようにする、実装は T18-6d 別スライス）。このページは
	 * 一覧の表示・行選択・Drawer の開閉・作成直後の「次へ」導線バナー・
	 * 前工程（PLC接続）が無い場合の案内だけを持つ薄い実装に変わった。
	 *
	 * PLC接続の選択肢は `listPlcConnections()` から取得して Drawer に渡す
	 * （収集グループは必ずどれか1つの PLC 接続にぶら下がる - banto-tags の
	 * 外部キー制約）。周期（periodMs）は `ALLOWED_PERIOD_MS`
	 * （banto_tags::ALLOWED_PERIOD_MS のミラー）からの select のみ許可
	 * （`CollectionGroupDrawer.svelte` 側の実装）。
	 *
	 * 削除は、タグが参照している場合にサービス層の Validation エラー
	 * （「…タグが N 件あるため削除できません」）で拒否されるので、それを
	 * トーストで表示する（`CollectionGroupDrawer.svelte::handleDelete` が担う、
	 * plc-connections 画面と同じパターン）。
	 */
	import { page } from '$app/state';
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import CollectionGroupDrawer from '$lib/components/CollectionGroupDrawer.svelte';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listCollectionGroups,
		listPlcConnections,
		type CollectionGroup,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';
	import { resolvePresetConnectionId, tagsHref } from '$lib/banto/tagOnboarding';

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let connections: PlcConnection[] = $state([]);
	let groups: CollectionGroup[] = $state([]);
	let loading = $state(false);

	function connectionName(id: number): string {
		return connections.find((c) => c.id === id)?.name ?? `#${id}`;
	}

	async function reload(): Promise<void> {
		loading = true;
		try {
			[connections, groups] = await Promise.all([listPlcConnections(), listCollectionGroups()]);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	/**
	 * T18-2d（TAG-UX-A「ツリーで選択中の接続／グループを単票・連続登録へ
	 * プリセットする」の姉妹機能 - 画面間の「PLC接続を作成→次のグループ
	 * へ」導線）: `/collection-groups?connectionId=` で渡された接続を新規
	 * 作成フォームへプリセットする。実装指示4の `CollectionGroupDrawer`
	 * `presetPlcConnectionId` prop へ渡すだけの薄い橋渡し - Drawer 側は
	 * 「新規作成で開いた瞬間の値」を使うので、旧実装が持っていた
	 * 「一度だけ適用」ガード（`presetApplied`）は不要になった
	 * （Drawer はページの再取得のたびにフォームを作り直さない - 開閉の
	 * `lastOpenKey` 遷移時にしか初期化しないため）。無効な値（未指定・
	 * 非数値・存在しない ID・calc/mem 等の virtual 接続）は
	 * `resolvePresetConnectionId` が `null` にするのでプリセットしない。
	 */
	const presetConnectionId = $derived(
		resolvePresetConnectionId(page.url.searchParams.get('connectionId'), connections)
	);

	// --- Drawer 表示制御 -------------------------------------------------------
	// `drawerGroup` は「編集対象」。`null` かつ `drawerOpen` なら新規作成。
	let drawerOpen = $state(false);
	let drawerGroup: CollectionGroup | null = $state(null);

	/**
	 * T18-2d（TAG-UX-A「グループ作成後は次のタグへ進む CTA を表示する」）:
	 * plc-connections ページの `lastCreated` と同じパターン。
	 */
	let lastCreated: CollectionGroup | null = $state(null);

	function openCreateDrawer(): void {
		drawerGroup = null;
		drawerOpen = true;
	}

	function selectGroup(g: CollectionGroup): void {
		drawerGroup = g;
		drawerOpen = true;
	}

	function closeDrawer(): void {
		drawerOpen = false;
	}

	async function handleDrawerSaved(saved: CollectionGroup): Promise<void> {
		// 新規作成のときだけオンボーディングCTAを出す（再設定では出さない -
		// `drawerGroup` が保存前に null だったか（＝作成フローだったか）で
		// 判定する - plc-connections/+page.svelte の同種ハンドラと同じ区別）。
		if (drawerGroup === null) {
			lastCreated = saved;
		}
		await reload();
	}

	async function handleDrawerDeleted(): Promise<void> {
		await reload();
	}

	const columns: GridColumn<CollectionGroup>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'plcConnectionId',
			header: 'PLC接続',
			accessor: (row) => connectionName(row.plcConnectionId),
			width: 160
		},
		{ id: 'periodMs', header: '周期(ms)', accessor: 'periodMs', width: 100, align: 'right' },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];
</script>

<div class="page">
	<h2>収集グループ</h2>

	{#if canWrite}
		<div class="toolbar">
			<button type="button" onclick={openCreateDrawer} disabled={connections.length === 0}>
				新規作成
			</button>
		</div>
		{#if connections.length === 0}
			<!--
				T18-2d（TAG-UX-A「空状態を…不足する前工程と移動ボタンを示す」）:
				前工程（PLC接続）が無いことを示し、その場で移動できるようにする。
			-->
			<p class="note">
				先に PLC接続 を1件以上登録してください。
				<a class="onboarding-cta" href="/plc-connections">PLC接続ページへ移動</a>
			</p>
		{/if}
	{/if}

	{#if lastCreated}
		<!-- T18-2d（TAG-UX-A「グループ作成後は次のタグへ進む CTA を表示する」）。
			文言は「登録が完了しました」とし、上の成功トースト（`作成しました`）と
			部分一致しないようにする（tags/+page.svelte 側の同種バナーで
			`getByText('作成しました')` が strict mode violation を起こした
			実測回帰、2026-08-12、PR #135 CI、と同じ理由の予防）。 -->
		<div class="onboarding-banner">
			<span>「{lastCreated.name}」の登録が完了しました。</span>
			<a class="onboarding-cta" href={tagsHref(lastCreated.id)}>次へ: タグを登録</a>
			<button type="button" class="secondary" onclick={() => (lastCreated = null)}>閉じる</button>
		</div>
	{/if}

	<section class="list">
		<h3>一覧</h3>
		<p class="note">
			{canWrite
				? '行をクリックするとグループの再設定用 Drawer が開きます。'
				: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
		</p>
		{#if loading && groups.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<div class="grid-wrap">
				<BantoGrid
					rows={groups}
					{columns}
					getRowId={(g) => g.id}
					onRowClick={canWrite ? selectGroup : undefined}
				/>
			</div>
		{/if}
	</section>
</div>

<CollectionGroupDrawer
	open={drawerOpen}
	group={drawerGroup}
	existingNames={groups.map((g) => g.name)}
	{connections}
	presetPlcConnectionId={presetConnectionId}
	onClose={closeDrawer}
	onSaved={handleDrawerSaved}
	onDeleted={handleDrawerDeleted}
/>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 860px;
	}

	h2 {
		margin: 0;
		font-size: 1.1rem;
	}

	.toolbar {
		display: flex;
		justify-content: flex-end;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	.grid-wrap {
		height: 320px;
	}

	/* T18-2d（TAG-UX-A）: 前工程への移動リンク・作成直後の「次へ」導線バナー。 */
	.onboarding-banner {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.75rem;
		padding: 0.6rem 0.9rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		font-size: 0.85rem;
	}

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

	button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	button:hover:not(:disabled) {
		background: var(--banto-primary-hover);
	}

	button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
	}

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
		color: var(--banto-text);
	}
</style>
