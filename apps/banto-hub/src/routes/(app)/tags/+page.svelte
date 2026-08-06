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
		listPlcConnections,
		MIN_STRING_LENGTH,
		MAX_STRING_LENGTH,
		TAG_KIND_OPTIONS,
		CALC_CONNECTION_NAME,
		MEM_CONNECTION_NAME,
		type Tag,
		type TagInput,
		type TagDataType,
		type TagKind,
		type CollectionGroup,
		type PlcConnection
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
		/**
		 * 書き込み可（T2-3、docs/tag-server-design.md §6 item 1）。既定 off -
		 * 明示的に opt-in させたタグだけを書き込み対象にする設計（per-tag
		 * opt-in）に合わせる。`computed` タグではこのチェックボックス自体を
		 * 隠す（送信時に強制 false - サーバー側も writable=true を拒否する）。
		 */
		writable: boolean;
		/** T6-2: タグ種別。既定は既存どおり `plc`。 */
		tagKind: TagKind;
		/** T6-2: 演算タグの式ソース（`tagKind === 'computed'` のときのみ表示・送信）。 */
		expression: string;
		/** T6-2: 内部タグの再起動時復元フラグ（`tagKind === 'internal'` のときのみ表示）。 */
		retain: boolean;
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
			enabled: true,
			writable: false,
			tagKind: 'plc',
			expression: '',
			retain: false
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
			enabled: t.enabled,
			writable: t.writable,
			tagKind: t.tagKind,
			expression: t.expression ?? '',
			retain: t.retain
		};
	}

	/** 空文字列 = 未設定（省略してバックエンドの `#[serde(default)]` に None として扱わせる）。 */
	function optNum(s: string): number | undefined {
		return s === '' ? undefined : Number(s);
	}

	/**
	 * T6-2 (docs/tag-server-design.md §4.2's table): `computed`/`internal`
	 * tags carry no PLC address at all — send an empty string regardless of
	 * whatever the (hidden) address field still holds from a prior `plc`
	 * selection, so switching `tagKind` in the form can never leak a stale
	 * address into a payload the backend would reject.
	 */
	function toInput(form: FormState): TagInput {
		const isPlc = form.tagKind === 'plc';
		const isComputed = form.tagKind === 'computed';
		return {
			name: form.name,
			collectionGroupId: Number(form.collectionGroupId),
			address: isPlc ? form.address : '',
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
			enabled: form.enabled,
			// computed タグは常に writable=false（値は式が決める、§4.2表）-
			// フォーム自体もこのチェックボックスを隠すが、送信直前にも強制する。
			writable: isComputed ? false : form.writable,
			tagKind: form.tagKind,
			expression: isComputed ? form.expression : undefined,
			retain: form.tagKind === 'internal' ? form.retain : false
		};
	}

	let groups: CollectionGroup[] = $state([]);
	let connections: PlcConnection[] = $state([]);
	let tags: Tag[] = $state([]);
	let loading = $state(false);

	function groupName(id: number): string {
		return groups.find((g) => g.id === id)?.name ?? `#${id}`;
	}

	function connectionName(id: number): string | undefined {
		return connections.find((c) => c.id === id)?.name;
	}

	/**
	 * T6-2: groups の候補をタグ種別で絞り込む — `computed` は `calc` 接続
	 * 配下、`internal` は `mem` 接続配下、`plc` はそのどちらでもない接続配下
	 * のみ（`banto_tags::tag::validate_tag_kind_placement` と同じ規則）。
	 * サーバー側検証の先取りであって、これ自体が正の唯一の判定源ではない。
	 */
	function groupsFor(kind: TagKind): CollectionGroup[] {
		return groups.filter((g) => {
			const name = connectionName(g.plcConnectionId);
			if (kind === 'computed') return name === CALC_CONNECTION_NAME;
			if (kind === 'internal') return name === MEM_CONNECTION_NAME;
			return name !== CALC_CONNECTION_NAME && name !== MEM_CONNECTION_NAME;
		});
	}

	async function reload(): Promise<void> {
		loading = true;
		try {
			[groups, connections, tags] = await Promise.all([
				listCollectionGroups(),
				listPlcConnections(),
				listTags()
			]);
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
			id: 'tagKind',
			header: '種別',
			accessor: 'tagKind',
			width: 90
		},
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		},
		{
			id: 'writable',
			header: '書き込み可',
			accessor: 'writable',
			width: 90,
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
				{#each groupsFor(form.tagKind) as group (group.id)}
					<option value={String(group.id)}>{group.name}</option>
				{/each}
			</select>
			{#if form.tagKind === 'computed'}
				<span class="hint">{CALC_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span>
			{:else if form.tagKind === 'internal'}
				<span class="hint">{MEM_CONNECTION_NAME} 接続配下のグループのみ選択できます。</span>
			{/if}
			{#if errors.collectionGroupId}<span class="err">{errors.collectionGroupId}</span>{/if}
		</label>
		<label class="field">
			タグ種別
			<select
				bind:value={form.tagKind}
				onchange={() => {
					// タグ種別を切り替えたら、もう選択できないグループ ID は
					// クリアする（`groupsFor` の絞り込みと矛盾する選択を残さない）。
					if (!groupsFor(form.tagKind).some((g) => String(g.id) === form.collectionGroupId)) {
						form.collectionGroupId = '';
					}
				}}
			>
				{#each TAG_KIND_OPTIONS as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.tagKind}<span class="err">{errors.tagKind}</span>{/if}
		</label>
		{#if form.tagKind === 'plc'}
			<label class="field">
				アドレス
				<input type="text" bind:value={form.address} placeholder="D100（ビット: D100.5）" />
				<span class="hint"
					>ワードデバイスの特定ビットを読み書きするときは「D100.5」のように「.」+ビット位置（0〜15、Modbus
					は「40001.3」）を付けます。「D100.5」でワードの5ビット目。ビット指定アドレスは data_type =
					bit のタグでのみ使えます。</span
				>
				{#if errors.address}<span class="err">{errors.address}</span>{/if}
			</label>
		{/if}
		{#if form.tagKind === 'computed'}
			<label class="field wide">
				式（expression）
				<textarea
					bind:value={form.expression}
					rows="2"
					placeholder="(line1.fast.a + line1.fast.b) / 2"></textarea>
				<span class="hint"
					>四則・比較・論理・if(c,a,b)・min/max/abs/round/clamp/bit(tag,n)。参照する外部名は他タグ
					（plc/computed/internal）の完全名。</span
				>
				{#if errors.expression}<span class="err">{errors.expression}</span>{/if}
			</label>
		{/if}
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
		{#if form.tagKind !== 'computed'}
			<label class="field checkbox">
				<input type="checkbox" bind:checked={form.writable} />
				書き込み可（writable）
			</label>
		{/if}
		{#if form.tagKind === 'internal'}
			<label class="field checkbox">
				<input type="checkbox" bind:checked={form.retain} />
				retain（再起動時に最終値を復元）
			</label>
		{/if}
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

	.field.wide {
		grid-column: 1 / -1;
	}

	.field input,
	.field select,
	.field textarea {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
		font-family: inherit;
	}

	.field textarea {
		resize: vertical;
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
