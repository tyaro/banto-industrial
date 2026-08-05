<script lang="ts">
	/**
	 * 収集グループ（collection_groups）CRUD 画面。plc-connections/+page.svelte
	 * と同じシンプル型（BantoGrid一覧＋行クリック編集）を反復した新規作成。
	 *
	 * PLC接続の選択肢は `listPlcConnections()` から取得して select に出す
	 * （収集グループは必ずどれか1つの PLC 接続にぶら下がる - banto-tags の
	 * 外部キー制約）。周期（periodMs）は `ALLOWED_PERIOD_MS`
	 * （banto_tags::ALLOWED_PERIOD_MS のミラー）からの select のみ許可。
	 *
	 * 削除は、タグが参照している場合にサービス層の Validation エラー
	 * （「…タグが N 件あるため削除できません」）で拒否されるので、それを
	 * トーストで表示する（plc-connections 画面と同じパターン）。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listCollectionGroups,
		createCollectionGroup,
		updateCollectionGroup,
		deleteCollectionGroup,
		listPlcConnections,
		ALLOWED_PERIOD_MS,
		type CollectionGroup,
		type CollectionGroupInput,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	interface FormState {
		name: string;
		plcConnectionId: string;
		periodMs: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			plcConnectionId: '',
			periodMs: String(ALLOWED_PERIOD_MS[0]),
			enabled: true
		};
	}

	function formFromGroup(g: CollectionGroup): FormState {
		return {
			name: g.name,
			plcConnectionId: String(g.plcConnectionId),
			periodMs: String(g.periodMs),
			enabled: g.enabled
		};
	}

	function toInput(form: FormState): CollectionGroupInput {
		return {
			name: form.name,
			plcConnectionId: Number(form.plcConnectionId),
			periodMs: Number(form.periodMs),
			enabled: form.enabled
		};
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

	// --- create ---
	let createForm = $state(blankForm());
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	async function handleCreate(): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			await createCollectionGroup(toInput(createForm));
			toastStore.push('success', '作成しました');
			createForm = blankForm();
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) createErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			creating = false;
		}
	}

	// --- edit ---
	let selected: CollectionGroup | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectGroup(g: CollectionGroup): void {
		selected = g;
		editForm = formFromGroup(g);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateCollectionGroup(selected.id, toInput(editForm));
			toastStore.push('success', '更新しました');
			selected = updated;
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) editErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!selected) return;
		if (!window.confirm(`${selected.name} を削除しますか？`)) return;
		try {
			await deleteCollectionGroup(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
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

{#snippet groupFields(form: FormState, errors: Record<string, string>)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			PLC接続
			<select bind:value={form.plcConnectionId}>
				<option value="" disabled>選択してください</option>
				{#each connections as conn (conn.id)}
					<option value={String(conn.id)}>{conn.name}</option>
				{/each}
			</select>
			{#if errors.plcConnectionId}<span class="err">{errors.plcConnectionId}</span>{/if}
		</label>
		<label class="field">
			収集周期
			<select bind:value={form.periodMs}>
				{#each ALLOWED_PERIOD_MS as ms (ms)}
					<option value={String(ms)}>{ms} ms</option>
				{/each}
			</select>
			{#if errors.periodMs}<span class="err">{errors.periodMs}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
{/snippet}

<div class="page">
	<h2>収集グループ</h2>

	{#if canWrite}
		<section class="create">
			<h3>新規作成</h3>
			{@render groupFields(createForm, createErrors)}
			<button type="button" onclick={handleCreate} disabled={creating || connections.length === 0}
				>作成</button
			>
			{#if connections.length === 0}
				<p class="note">先に PLC接続 を1件以上登録してください。</p>
			{/if}
		</section>
	{/if}

	<section class="list">
		<h3>一覧</h3>
		<p class="note">
			{canWrite
				? '行をクリックすると下に編集パネルが表示されます。'
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

	{#if selected && canWrite}
		<section class="detail">
			<h3>{selected.name} を編集</h3>
			{@render groupFields(editForm, editErrors)}
			<div class="actions">
				<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
				<button type="button" class="danger" onclick={handleDelete}>削除</button>
			</div>
		</section>
	{/if}
</div>

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

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
	}

	.field input,
	.field select {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	.field.checkbox input {
		width: auto;
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
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

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
