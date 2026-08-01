<script lang="ts">
	/**
	 * 書き込み先（write_targets）CRUD 画面（plan W2）。ルールが書き込む PLC
	 * デバイスの登録。viewer は閲覧のみ、editor 以上が作成/編集/削除できる
	 * （backend も同じ権限で二経路対称 — REST/Tauri）。デモモード（バックエンド
	 * なし）ではレジストリDBが無いため案内文のみ表示する。
	 *
	 * PLC接続は R1-B の PLC接続画面（/plc-connections）で登録されたものを
	 * プルダウンで選択する（かつては一覧APIが無く数値ID直接入力だった）。
	 * 選択肢のラベルには数値IDも含める（監査ログの entityId/detail が ID
	 * ベースなので、突き合わせられるように）。エンジンが実際に書き込むのは
	 * SLMP 接続のみのため、ラベルにプロトコルを表示して正直に示す。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listWriteTargets,
		createWriteTarget,
		updateWriteTarget,
		deleteWriteTarget,
		isWriteRegistryAvailable,
		DEMO_MODE_MESSAGE,
		type WriteTarget,
		type WriteTargetInput,
		type WriteDataType
	} from '$lib/banto/writeRegistryAdmin';
	import { listPlcConnections, type PlcConnection } from '$lib/banto/tagRegistryAdmin';

	const dataTypeOptions: WriteDataType[] = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32'];

	const available = isWriteRegistryAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	/** Editable form state (shared by create + edit). Strings for numeric inputs so empty = unset. */
	interface FormState {
		name: string;
		plcConnectionId: string;
		address: string;
		dataType: WriteDataType;
		unit: string;
		decimals: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			plcConnectionId: '',
			address: '',
			dataType: 'i16',
			unit: '',
			decimals: '0',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			enabled: true
		};
	}

	function formFromTarget(t: WriteTarget): FormState {
		return {
			name: t.name,
			plcConnectionId: String(t.plcConnectionId),
			address: t.address,
			dataType: t.dataType,
			unit: t.unit ?? '',
			decimals: String(t.decimals),
			rawLo: t.rawLo === null ? '' : String(t.rawLo),
			rawHi: t.rawHi === null ? '' : String(t.rawHi),
			engLo: t.engLo === null ? '' : String(t.engLo),
			engHi: t.engHi === null ? '' : String(t.engHi),
			enabled: t.enabled
		};
	}

	function numOrNull(value: string): number | null {
		const trimmed = value.trim();
		return trimmed === '' ? null : Number(trimmed);
	}

	function toInput(form: FormState): WriteTargetInput {
		return {
			name: form.name,
			plcConnectionId: Number(form.plcConnectionId),
			address: form.address,
			dataType: form.dataType,
			unit: form.unit.trim() === '' ? null : form.unit,
			decimals: Number(form.decimals),
			rawLo: numOrNull(form.rawLo),
			rawHi: numOrNull(form.rawHi),
			engLo: numOrNull(form.engLo),
			engHi: numOrNull(form.engHi),
			enabled: form.enabled
		};
	}

	let targets: WriteTarget[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let loading = $state(false);

	/** Option label: keep the numeric ID visible so audit rows (entityId/
	 *  detail are ID-based) remain interpretable against the UI. */
	function connectionLabel(c: PlcConnection): string {
		return `${c.id}: ${c.name}（${c.protocol}）`;
	}

	function connectionName(id: number): string {
		const c = connections.find((entry) => entry.id === id);
		return c ? `${c.name}（${c.protocol}）` : `#${id}`;
	}

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			const [targetList, connectionList] = await Promise.all([
				listWriteTargets(),
				listPlcConnections()
			]);
			targets = targetList;
			connections = connectionList;
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
			await createWriteTarget(toInput(createForm));
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
	let selected: WriteTarget | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectTarget(t: WriteTarget): void {
		selected = t;
		editForm = formFromTarget(t);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateWriteTarget(selected.id, toInput(editForm));
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
			await deleteWriteTarget(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	const columns: GridColumn<WriteTarget>[] = [
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
			accessor: (t) => connectionName(t.plcConnectionId),
			width: 150
		},
		{ id: 'address', header: 'アドレス', accessor: 'address', width: 110 },
		{ id: 'dataType', header: '型', accessor: 'dataType', width: 70 },
		{ id: 'unit', header: '単位', accessor: 'unit', width: 80 },
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
	<h2>書き込み先</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはレジストリDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		{#if canWrite}
			<section class="create">
				<h3>新規作成</h3>
				<div class="form-grid">
					<label class="field">
						名前
						<input type="text" bind:value={createForm.name} />
						{#if createErrors.name}<span class="err">{createErrors.name}</span>{/if}
					</label>
					<label class="field">
						PLC接続
						<select bind:value={createForm.plcConnectionId}>
							<option value="">選択してください</option>
							{#each connections as c (c.id)}
								<option value={String(c.id)}>{connectionLabel(c)}</option>
							{/each}
						</select>
						<span class="hint">エンジンが書き込むのは SLMP 接続のみです。</span>
						{#if createErrors.plcConnectionId}<span class="err">{createErrors.plcConnectionId}</span
							>{/if}
					</label>
					<label class="field">
						アドレス
						<input type="text" bind:value={createForm.address} placeholder="D100" />
						{#if createErrors.address}<span class="err">{createErrors.address}</span>{/if}
					</label>
					<label class="field">
						データ型
						<select bind:value={createForm.dataType}>
							{#each dataTypeOptions as dt (dt)}
								<option value={dt}>{dt}</option>
							{/each}
						</select>
						{#if createErrors.dataType}<span class="err">{createErrors.dataType}</span>{/if}
					</label>
					<label class="field">
						単位
						<input type="text" bind:value={createForm.unit} />
					</label>
					<label class="field">
						小数桁
						<input type="number" min="0" max="6" bind:value={createForm.decimals} />
						{#if createErrors.decimals}<span class="err">{createErrors.decimals}</span>{/if}
					</label>
					<label class="field">
						raw下限
						<input type="number" bind:value={createForm.rawLo} />
					</label>
					<label class="field">
						raw上限
						<input type="number" bind:value={createForm.rawHi} />
					</label>
					<label class="field">
						eng下限
						<input type="number" bind:value={createForm.engLo} />
					</label>
					<label class="field">
						eng上限
						<input type="number" bind:value={createForm.engHi} />
					</label>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={createForm.enabled} />
						有効
					</label>
				</div>
				{#if createErrors.scaling}<p class="err">{createErrors.scaling}</p>{/if}
				<button type="button" onclick={handleCreate} disabled={creating}>作成</button>
			</section>
		{/if}

		<section class="list">
			<h3>一覧</h3>
			<p class="note">
				{canWrite
					? '行をクリックすると下に編集パネルが表示されます。'
					: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
			</p>
			{#if loading && targets.length === 0}
				<p class="loading">読み込み中…</p>
			{:else}
				<div class="grid-wrap">
					<BantoGrid
						rows={targets}
						{columns}
						getRowId={(t) => t.id}
						onRowClick={canWrite ? selectTarget : undefined}
					/>
				</div>
			{/if}
		</section>

		{#if selected && canWrite}
			<section class="detail">
				<h3>{selected.name} を編集</h3>
				<div class="form-grid">
					<label class="field">
						名前
						<input type="text" bind:value={editForm.name} />
						{#if editErrors.name}<span class="err">{editErrors.name}</span>{/if}
					</label>
					<label class="field">
						PLC接続
						<select bind:value={editForm.plcConnectionId}>
							<option value="">選択してください</option>
							{#each connections as c (c.id)}
								<option value={String(c.id)}>{connectionLabel(c)}</option>
							{/each}
						</select>
						<span class="hint">エンジンが書き込むのは SLMP 接続のみです。</span>
						{#if editErrors.plcConnectionId}<span class="err">{editErrors.plcConnectionId}</span
							>{/if}
					</label>
					<label class="field">
						アドレス
						<input type="text" bind:value={editForm.address} />
						{#if editErrors.address}<span class="err">{editErrors.address}</span>{/if}
					</label>
					<label class="field">
						データ型
						<select bind:value={editForm.dataType}>
							{#each dataTypeOptions as dt (dt)}
								<option value={dt}>{dt}</option>
							{/each}
						</select>
					</label>
					<label class="field">
						単位
						<input type="text" bind:value={editForm.unit} />
					</label>
					<label class="field">
						小数桁
						<input type="number" min="0" max="6" bind:value={editForm.decimals} />
						{#if editErrors.decimals}<span class="err">{editErrors.decimals}</span>{/if}
					</label>
					<label class="field">
						raw下限
						<input type="number" bind:value={editForm.rawLo} />
					</label>
					<label class="field">
						raw上限
						<input type="number" bind:value={editForm.rawHi} />
					</label>
					<label class="field">
						eng下限
						<input type="number" bind:value={editForm.engLo} />
					</label>
					<label class="field">
						eng上限
						<input type="number" bind:value={editForm.engHi} />
					</label>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={editForm.enabled} />
						有効
					</label>
				</div>
				{#if editErrors.scaling}<p class="err">{editErrors.scaling}</p>{/if}
				<div class="actions">
					<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
					<button type="button" class="danger" onclick={handleDelete}>削除</button>
				</div>
			</section>
		{/if}
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
		grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
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

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
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
