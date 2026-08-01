<script lang="ts">
	/**
	 * タグ登録（tags）CRUD 画面（R1-B）。書き込みルールの条件・コピー元が参照
	 * するソースタグの登録。write-targets/+page.svelte と同じ構造（BantoGrid
	 * 一覧＋行クリックで下に編集パネル、viewer=閲覧のみ／editor以上=作成・
	 * 編集・削除、デモモード案内、両経路対称のバックエンド）。
	 *
	 * 収集グループ（collection_groups）の管理はこの画面に内包する（画面は
	 * PLC接続／タグ登録の2枚構成 — グループはタグの実装詳細であり
	 * 独立画面にしない）。ページ上部の「収集グループ」セクションで一覧＋
	 * 作成・編集・削除でき、タグのフォームはそのグループを select で参照する
	 * （グループ名＋接続名を表示）。
	 *
	 * period_ms について: relay-wright のエンジンは自前の固定間隔でポーリング
	 * するため、収集周期は共有レジストリ上のメタデータ（ChronoGazer 等の収集
	 * エンジンが使用）であり、本アプリでは情報表示にとどまる — その旨を
	 * ヘルプテキストに明記する。
	 *
	 * スケーリング（raw/eng の上下限）は banto-tags の検証どおり「4つ全て
	 * 入力するか、全て空にするか」の all-or-nothing。しきい値は
	 * LL ≤ L ≤ H ≤ HH（設定されたもの同士のみ比較）。
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
		createCollectionGroup,
		updateCollectionGroup,
		deleteCollectionGroup,
		listPlcConnections,
		isTagRegistryAvailable,
		ALLOWED_PERIOD_MS,
		DEMO_MODE_MESSAGE,
		type Tag,
		type TagInput,
		type TagDataType,
		type CollectionGroup,
		type CollectionGroupInput,
		type PlcConnection
	} from '$lib/banto/tagRegistryAdmin';

	const dataTypeOptions: TagDataType[] = ['bit', 'i16', 'u16', 'i32', 'u32', 'f32'];

	function periodLabel(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60000) return `${ms / 1000}s`;
		return `${ms / 60000}min`;
	}

	const available = isTagRegistryAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	function applyFieldErrors(err: unknown): Record<string, string> | null {
		if (isProviderError(err) && err.body.kind === 'validation') {
			const map: Record<string, string> = {};
			for (const fe of err.body.field_errors) map[fe.field] = fe.message;
			return map;
		}
		return null;
	}

	// --- shared data ---
	let tags: Tag[] = $state([]);
	let groups: CollectionGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let loading = $state(false);

	function connectionName(id: number): string {
		return connections.find((c) => c.id === id)?.name ?? `#${id}`;
	}

	function groupLabel(g: CollectionGroup): string {
		return `${g.name}（${connectionName(g.plcConnectionId)}）`;
	}

	function groupName(id: number): string {
		const g = groups.find((entry) => entry.id === id);
		return g ? g.name : `#${id}`;
	}

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			const [tagList, groupList, connectionList] = await Promise.all([
				listTags(),
				listCollectionGroups(),
				listPlcConnections()
			]);
			tags = tagList;
			groups = groupList;
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

	// --- collection group management (embedded; groups are an implementation
	// detail of tags, so no separate screen) ---
	interface GroupFormState {
		name: string;
		plcConnectionId: string;
		periodMs: string;
		enabled: boolean;
	}

	function blankGroupForm(): GroupFormState {
		return { name: '', plcConnectionId: '', periodMs: '1000', enabled: true };
	}

	function groupToInput(form: GroupFormState): CollectionGroupInput {
		return {
			name: form.name,
			plcConnectionId: Number(form.plcConnectionId),
			periodMs: Number(form.periodMs),
			enabled: form.enabled
		};
	}

	let groupForm = $state(blankGroupForm());
	let groupErrors: Record<string, string> = $state({});
	let groupSaving = $state(false);
	/** null = creating a new group; otherwise the group being edited. */
	let editingGroup: CollectionGroup | null = $state(null);

	function startEditGroup(g: CollectionGroup): void {
		editingGroup = g;
		groupForm = {
			name: g.name,
			plcConnectionId: String(g.plcConnectionId),
			periodMs: String(g.periodMs),
			enabled: g.enabled
		};
		groupErrors = {};
	}

	function cancelEditGroup(): void {
		editingGroup = null;
		groupForm = blankGroupForm();
		groupErrors = {};
	}

	async function saveGroup(): Promise<void> {
		groupSaving = true;
		groupErrors = {};
		try {
			if (editingGroup) {
				await updateCollectionGroup(editingGroup.id, groupToInput(groupForm));
				toastStore.push('success', '収集グループを更新しました');
			} else {
				await createCollectionGroup(groupToInput(groupForm));
				toastStore.push('success', '収集グループを作成しました');
			}
			cancelEditGroup();
			await reload();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) groupErrors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			groupSaving = false;
		}
	}

	async function handleDeleteGroup(g: CollectionGroup): Promise<void> {
		if (!window.confirm(`収集グループ ${g.name} を削除しますか？`)) return;
		try {
			await deleteCollectionGroup(g.id);
			toastStore.push('success', '収集グループを削除しました');
			if (editingGroup?.id === g.id) cancelEditGroup();
			await reload();
		} catch (err) {
			// タグが所属している場合はサービス層の件数入り Validation エラー。
			toastStore.push('error', errorMessage(err));
		}
	}

	// --- tag create/edit ---
	/** Editable form state (shared by create + edit). Strings for numeric inputs so empty = unset. */
	interface FormState {
		name: string;
		collectionGroupId: string;
		address: string;
		dataType: TagDataType;
		unit: string;
		decimals: string;
		rawLo: string;
		rawHi: string;
		engLo: string;
		engHi: string;
		thresholdLl: string;
		thresholdL: string;
		thresholdH: string;
		thresholdHh: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			collectionGroupId: '',
			address: '',
			dataType: 'i16',
			unit: '',
			decimals: '0',
			rawLo: '',
			rawHi: '',
			engLo: '',
			engHi: '',
			thresholdLl: '',
			thresholdL: '',
			thresholdH: '',
			thresholdHh: '',
			enabled: true
		};
	}

	function formFromTag(t: Tag): FormState {
		return {
			name: t.name,
			collectionGroupId: String(t.collectionGroupId),
			address: t.address,
			dataType: t.dataType,
			unit: t.unit ?? '',
			decimals: String(t.decimals),
			rawLo: t.rawLo === null ? '' : String(t.rawLo),
			rawHi: t.rawHi === null ? '' : String(t.rawHi),
			engLo: t.engLo === null ? '' : String(t.engLo),
			engHi: t.engHi === null ? '' : String(t.engHi),
			thresholdLl: t.thresholdLl === null ? '' : String(t.thresholdLl),
			thresholdL: t.thresholdL === null ? '' : String(t.thresholdL),
			thresholdH: t.thresholdH === null ? '' : String(t.thresholdH),
			thresholdHh: t.thresholdHh === null ? '' : String(t.thresholdHh),
			enabled: t.enabled
		};
	}

	/**
	 * 空欄 = 未設定（null）。フォーム状態は初期値こそ文字列だが、Svelte 5 の
	 * `bind:value` は `type="number"` の入力後に number（空欄は null）を書き
	 * 戻すため、実行時には string | number | null が混在する — string 前提で
	 * `.trim()` すると入力後の保存が TypeError で落ちる（write-targets/
	 * write-rules に元からあった実バグと同型。3画面とも同修正）。
	 */
	function numOrNull(value: string | number | null): number | null {
		if (typeof value === 'number') return Number.isNaN(value) ? null : value;
		if (value === null) return null;
		const trimmed = value.trim();
		return trimmed === '' ? null : Number(trimmed);
	}

	function toInput(form: FormState): TagInput {
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: form.address,
			dataType: form.dataType,
			unit: form.unit.trim() === '' ? null : form.unit,
			decimals: Number(form.decimals),
			rawLo: numOrNull(form.rawLo),
			rawHi: numOrNull(form.rawHi),
			engLo: numOrNull(form.engLo),
			engHi: numOrNull(form.engHi),
			thresholdLl: numOrNull(form.thresholdLl),
			thresholdL: numOrNull(form.thresholdL),
			thresholdH: numOrNull(form.thresholdH),
			thresholdHh: numOrNull(form.thresholdHh),
			enabled: form.enabled
		};
	}

	// --- create ---
	let createForm = $state(blankForm());
	let createErrors: Record<string, string> = $state({});
	let creating = $state(false);

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
			width: 150,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'collectionGroup',
			header: '収集グループ',
			accessor: (t) => groupName(t.collectionGroupId),
			width: 130
		},
		{ id: 'address', header: 'アドレス', accessor: 'address', width: 100 },
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
				<option value="">選択してください</option>
				{#each groups as g (g.id)}
					<option value={String(g.id)}>{groupLabel(g)}</option>
				{/each}
			</select>
			{#if errors.collectionGroupId}<span class="err">{errors.collectionGroupId}</span>{/if}
		</label>
		<label class="field">
			アドレス
			<input type="text" bind:value={form.address} placeholder="D100" />
			<span class="hint">SLMP 記法（例: D100 / M10 / X1A）</span>
			{#if errors.address}<span class="err">{errors.address}</span>{/if}
		</label>
		<label class="field">
			データ型
			<select bind:value={form.dataType}>
				{#each dataTypeOptions as dt (dt)}
					<option value={dt}>{dt}</option>
				{/each}
			</select>
			{#if errors.dataType}<span class="err">{errors.dataType}</span>{/if}
		</label>
		<label class="field">
			単位
			<input type="text" bind:value={form.unit} />
		</label>
		<label class="field">
			小数桁
			<input type="number" min="0" max="6" bind:value={form.decimals} />
			{#if errors.decimals}<span class="err">{errors.decimals}</span>{/if}
		</label>
		<label class="field">
			raw下限
			<input type="number" bind:value={form.rawLo} />
		</label>
		<label class="field">
			raw上限
			<input type="number" bind:value={form.rawHi} />
		</label>
		<label class="field">
			eng下限
			<input type="number" bind:value={form.engLo} />
		</label>
		<label class="field">
			eng上限
			<input type="number" bind:value={form.engHi} />
		</label>
		<label class="field">
			しきい値LL
			<input type="number" bind:value={form.thresholdLl} />
			{#if errors.thresholdLl}<span class="err">{errors.thresholdLl}</span>{/if}
		</label>
		<label class="field">
			しきい値L
			<input type="number" bind:value={form.thresholdL} />
			{#if errors.thresholdL}<span class="err">{errors.thresholdL}</span>{/if}
		</label>
		<label class="field">
			しきい値H
			<input type="number" bind:value={form.thresholdH} />
			{#if errors.thresholdH}<span class="err">{errors.thresholdH}</span>{/if}
		</label>
		<label class="field">
			しきい値HH
			<input type="number" bind:value={form.thresholdHh} />
			{#if errors.thresholdHh}<span class="err">{errors.thresholdHh}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
	<p class="note">
		スケーリング（raw/eng の上下限）は 4 つすべて入力するか、すべて空にしてください （空 =
		スケーリングなし）。しきい値は LL ≤ L ≤ H ≤ HH の順（設定した項目のみ比較）。
	</p>
	{#if errors.scaling}<p class="err">{errors.scaling}</p>{/if}
{/snippet}

<div class="page">
	<h2>タグ登録</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはレジストリDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<section class="groups">
			<h3>収集グループ</h3>
			<p class="note">
				タグは必ずいずれかの収集グループ（PLC接続への所属単位）に属します。
				収集周期（period）は共有レジストリのメタデータで、本アプリのエンジンは自前の
				固定間隔でポーリングするため、ここでは参考情報です。
			</p>
			{#if groups.length === 0}
				<p class="note">
					収集グループがまだありません。{canWrite
						? 'タグを登録する前に、下のフォームから作成してください。'
						: '編集者以上の権限で作成できます。'}
				</p>
			{:else}
				<table class="group-table">
					<thead>
						<tr>
							<th>ID</th>
							<th>名前</th>
							<th>PLC接続</th>
							<th>収集周期</th>
							<th>有効</th>
							{#if canWrite}<th></th>{/if}
						</tr>
					</thead>
					<tbody>
						{#each groups as g (g.id)}
							<tr class:editing={editingGroup?.id === g.id}>
								<td class="num">{g.id}</td>
								<td>{g.name}</td>
								<td>{connectionName(g.plcConnectionId)}</td>
								<td>{periodLabel(g.periodMs)}</td>
								<td>{g.enabled ? 'はい' : 'いいえ'}</td>
								{#if canWrite}
									<td class="row-actions">
										<button type="button" class="small" onclick={() => startEditGroup(g)}>
											編集
										</button>
										<button type="button" class="small danger" onclick={() => handleDeleteGroup(g)}>
											削除
										</button>
									</td>
								{/if}
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
			{#if canWrite}
				<div class="group-form">
					<span class="group-form-title">
						{editingGroup ? `${editingGroup.name} を編集` : '新規グループ'}
					</span>
					<div class="form-grid">
						<label class="field">
							名前
							<input type="text" bind:value={groupForm.name} />
							{#if groupErrors.name}<span class="err">{groupErrors.name}</span>{/if}
						</label>
						<label class="field">
							PLC接続
							<select bind:value={groupForm.plcConnectionId}>
								<option value="">選択してください</option>
								{#each connections as c (c.id)}
									<option value={String(c.id)}>{c.name}（{c.protocol}）</option>
								{/each}
							</select>
							{#if groupErrors.plcConnectionId}<span class="err">{groupErrors.plcConnectionId}</span
								>{/if}
						</label>
						<label class="field">
							収集周期
							<select bind:value={groupForm.periodMs}>
								{#each ALLOWED_PERIOD_MS as ms (ms)}
									<option value={String(ms)}>{periodLabel(ms)}</option>
								{/each}
							</select>
							{#if groupErrors.periodMs}<span class="err">{groupErrors.periodMs}</span>{/if}
						</label>
						<label class="field checkbox">
							<input type="checkbox" bind:checked={groupForm.enabled} />
							有効
						</label>
					</div>
					<div class="actions">
						<button type="button" onclick={saveGroup} disabled={groupSaving}>
							{editingGroup ? '保存' : '作成'}
						</button>
						{#if editingGroup}
							<button type="button" class="ghost" onclick={cancelEditGroup}>キャンセル</button>
						{/if}
					</div>
				</div>
			{/if}
		</section>

		{#if canWrite}
			<section class="create">
				<h3>新規作成</h3>
				{@render tagFields(createForm, createErrors)}
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

	.group-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.8rem;
		margin-bottom: 0.75rem;
	}

	.group-table th,
	.group-table td {
		text-align: left;
		padding: 0.35rem 0.5rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.group-table th {
		color: var(--banto-text-muted);
		font-weight: 600;
	}

	.group-table td.num {
		text-align: right;
	}

	.group-table tr.editing td {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.row-actions {
		display: flex;
		gap: 0.4rem;
		justify-content: flex-end;
	}

	.group-form {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.group-form-title {
		font-size: 0.85rem;
		font-weight: 600;
	}

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
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

	button.small {
		padding: 0.3rem 0.6rem;
		font-size: 0.75rem;
	}

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}

	button.danger:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	button.ghost {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	button.ghost:hover {
		background: color-mix(in srgb, var(--banto-text) 8%, transparent);
	}
</style>
