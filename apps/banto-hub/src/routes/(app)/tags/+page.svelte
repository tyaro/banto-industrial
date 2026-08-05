<script lang="ts">
	/**
	 * タグ登録（tags）CRUD 画面。plc-connections/collection-groups と同じ
	 * シンプル型を反復した新規作成（実装指示: 「tags 画面は 1737 行版
	 * （一括/連続登録込み）をコピーしない」）。
	 *
	 * フォーム項目は `TagInput`（tagRegistryAdmin.ts、
	 * `banto_hub_core::rest::TagPayload` と同型）に1:1対応する。数値項目は
	 * すべて文字列で保持し、空欄 = 未設定（`toInput` で `undefined` に
	 * 変換 - JSON.stringify がフィールド自体を省略し、バックエンドの
	 * `#[serde(default)]` が `None` として受け取る）。`stringLength` は
	 * `dataType === 'string'` のときのみ表示・送信する（`MIN_STRING_LENGTH`/
	 * `MAX_STRING_LENGTH` はヒント表示のみ - 実際の検証はバックエンド）。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listTags,
		createTag,
		updateTag,
		deleteTag,
		listCollectionGroups,
		MIN_STRING_LENGTH,
		MAX_STRING_LENGTH,
		type Tag,
		type TagInput,
		type TagDataType,
		type CollectionGroup
	} from '$lib/banto/tagRegistryAdmin';

	const dataTypeOptions: { value: TagDataType; label: string }[] = [
		{ value: 'bit', label: 'bit（真偽値1点）' },
		{ value: 'i16', label: 'i16（符号あり16bit）' },
		{ value: 'u16', label: 'u16（符号なし16bit）' },
		{ value: 'i32', label: 'i32（符号あり32bit）' },
		{ value: 'u32', label: 'u32（符号なし32bit）' },
		{ value: 'f32', label: 'f32（浮動小数点32bit）' },
		{ value: 'string', label: 'string（文字列）' }
	];

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	interface FormState {
		name: string;
		collectionGroupId: string;
		address: string;
		dataType: TagDataType;
		stringLength: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		unit: string;
		decimals: string;
		thresholdH: string;
		thresholdHh: string;
		thresholdL: string;
		thresholdLl: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			collectionGroupId: '',
			address: '',
			dataType: 'f32',
			stringLength: '',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			unit: '',
			decimals: '0',
			thresholdH: '',
			thresholdHh: '',
			thresholdL: '',
			thresholdLl: '',
			enabled: true
		};
	}

	function numOrEmpty(v: number | null): string {
		return v === null ? '' : String(v);
	}

	function formFromTag(t: Tag): FormState {
		return {
			name: t.name,
			collectionGroupId: String(t.collectionGroupId),
			address: t.address,
			dataType: t.dataType,
			stringLength: numOrEmpty(t.stringLength),
			rawLo: numOrEmpty(t.rawLo),
			rawHi: numOrEmpty(t.rawHi),
			engLo: numOrEmpty(t.engLo),
			engHi: numOrEmpty(t.engHi),
			unit: t.unit ?? '',
			decimals: String(t.decimals),
			thresholdH: numOrEmpty(t.thresholdH),
			thresholdHh: numOrEmpty(t.thresholdHh),
			thresholdL: numOrEmpty(t.thresholdL),
			thresholdLl: numOrEmpty(t.thresholdLl),
			enabled: t.enabled
		};
	}

	/** 空文字列 = 未設定（省略してバックエンドの `#[serde(default)]` に None として扱わせる）。 */
	function optNum(s: string): number | undefined {
		return s === '' ? undefined : Number(s);
	}

	function toInput(form: FormState): TagInput {
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: form.address,
			dataType: form.dataType,
			stringLength: form.dataType === 'string' ? optNum(form.stringLength) : undefined,
			rawLo: optNum(form.rawLo),
			rawHi: optNum(form.rawHi),
			engLo: optNum(form.engLo),
			engHi: optNum(form.engHi),
			unit: form.unit === '' ? undefined : form.unit,
			decimals: Number(form.decimals),
			thresholdH: optNum(form.thresholdH),
			thresholdHh: optNum(form.thresholdHh),
			thresholdL: optNum(form.thresholdL),
			thresholdLl: optNum(form.thresholdLl),
			enabled: form.enabled
		};
	}

	let groups: CollectionGroup[] = $state([]);
	let tags: Tag[] = $state([]);
	let loading = $state(false);

	function groupName(id: number): string {
		return groups.find((g) => g.id === id)?.name ?? `#${id}`;
	}

	async function reload(): Promise<void> {
		loading = true;
		try {
			[groups, tags] = await Promise.all([listCollectionGroups(), listTags()]);
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
			await createTag(toInput(createForm));
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
	let selected: Tag | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectTag(t: Tag): void {
		selected = t;
		editForm = formFromTag(t);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateTag(selected.id, toInput(editForm));
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
			await deleteTag(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	const columns: GridColumn<Tag>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 140,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'collectionGroupId',
			header: '収集グループ',
			accessor: (row) => groupName(row.collectionGroupId),
			width: 140
		},
		{ id: 'address', header: 'アドレス', accessor: 'address', width: 100 },
		{ id: 'dataType', header: '型', accessor: 'dataType', width: 80 },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];
</script>

{#snippet tagFields(form: FormState, errors: Record<string, string>)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			収集グループ
			<select bind:value={form.collectionGroupId}>
				<option value="" disabled>選択してください</option>
				{#each groups as group (group.id)}
					<option value={String(group.id)}>{group.name}</option>
				{/each}
			</select>
			{#if errors.collectionGroupId}<span class="err">{errors.collectionGroupId}</span>{/if}
		</label>
		<label class="field">
			アドレス
			<input type="text" bind:value={form.address} placeholder="D100" />
			{#if errors.address}<span class="err">{errors.address}</span>{/if}
		</label>
		<label class="field">
			データ型
			<select bind:value={form.dataType}>
				{#each dataTypeOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err">{errors.dataType}</span>{/if}
		</label>
		{#if form.dataType === 'string'}
			<label class="field">
				文字列長（word数）
				<input
					type="number"
					min={MIN_STRING_LENGTH}
					max={MAX_STRING_LENGTH}
					bind:value={form.stringLength}
				/>
				<span class="hint">{MIN_STRING_LENGTH}〜{MAX_STRING_LENGTH} word（1 word = 2バイト）。</span
				>
				{#if errors.stringLength}<span class="err">{errors.stringLength}</span>{/if}
			</label>
		{/if}
		<label class="field">
			単位
			<input type="text" bind:value={form.unit} placeholder="℃" />
			{#if errors.unit}<span class="err">{errors.unit}</span>{/if}
		</label>
		<label class="field">
			小数桁数
			<input type="number" min="0" bind:value={form.decimals} />
			{#if errors.decimals}<span class="err">{errors.decimals}</span>{/if}
		</label>
		<label class="field">
			RawLo
			<input type="number" bind:value={form.rawLo} />
			{#if errors.rawLo}<span class="err">{errors.rawLo}</span>{/if}
		</label>
		<label class="field">
			RawHi
			<input type="number" bind:value={form.rawHi} />
			{#if errors.rawHi}<span class="err">{errors.rawHi}</span>{/if}
		</label>
		<label class="field">
			EngLo
			<input type="number" bind:value={form.engLo} />
			{#if errors.engLo}<span class="err">{errors.engLo}</span>{/if}
		</label>
		<label class="field">
			EngHi
			<input type="number" bind:value={form.engHi} />
			{#if errors.engHi}<span class="err">{errors.engHi}</span>{/if}
		</label>
		<label class="field">
			しきい値 H
			<input type="number" bind:value={form.thresholdH} />
			{#if errors.thresholdH}<span class="err">{errors.thresholdH}</span>{/if}
		</label>
		<label class="field">
			しきい値 HH
			<input type="number" bind:value={form.thresholdHh} />
			{#if errors.thresholdHh}<span class="err">{errors.thresholdHh}</span>{/if}
		</label>
		<label class="field">
			しきい値 L
			<input type="number" bind:value={form.thresholdL} />
			{#if errors.thresholdL}<span class="err">{errors.thresholdL}</span>{/if}
		</label>
		<label class="field">
			しきい値 LL
			<input type="number" bind:value={form.thresholdLl} />
			{#if errors.thresholdLl}<span class="err">{errors.thresholdLl}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
{/snippet}

<div class="page">
	<h2>タグ登録</h2>

	{#if canWrite}
		<section class="create">
			<h3>新規作成</h3>
			{@render tagFields(createForm, createErrors)}
			<button type="button" onclick={handleCreate} disabled={creating || groups.length === 0}
				>作成</button
			>
			{#if groups.length === 0}
				<p class="note">先に 収集グループ を1件以上登録してください。</p>
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
		{#if loading && tags.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<div class="grid-wrap">
				<BantoGrid
					rows={tags}
					{columns}
					getRowId={(t) => t.id}
					onRowClick={canWrite ? selectTag : undefined}
				/>
			</div>
		{/if}
	</section>

	{#if selected && canWrite}
		<section class="detail">
			<h3>{selected.name} を編集</h3>
			{@render tagFields(editForm, editErrors)}
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
		max-width: 960px;
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
