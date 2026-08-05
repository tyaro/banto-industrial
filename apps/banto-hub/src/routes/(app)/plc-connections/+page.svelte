<script lang="ts">
	/**
	 * PLC接続（plc_connections）CRUD 画面。relay-wright の
	 * `(app)/plc-connections/+page.svelte`（417行）を反復した実装指示どおりの
	 * シンプル型: BantoGrid一覧＋行クリックで下に編集パネル、viewer=閲覧のみ／
	 * editor以上=作成・編集・削除。relay-wright と異なり banto-hub には
	 * デモモードが無い（`tagRegistryAdmin.ts` 参照）ので、その分岐は持たない。
	 *
	 * プロトコルは "slmp" と "modbus-tcp" の select（banto-tags のレジストリ
	 * としては両方正当）。ただし現時点（T0）で banto-collect の
	 * `build_config`（crates/banto-collect/src/config.rs の `parse_protocol`）
	 * が実際に収集を組み立てられるのは modbus-tcp のみ - slmp を選んで
	 * 登録すると REST 自体は成功するが収集再構築が
	 * 「プロトコル slmp は未対応です」で失敗し、`/status` 画面にエラーが
	 * 出る（レジストリとしては正当な値のため拒否まではしない）。初見の罠に
	 * ならないよう、既定値は modbus-tcp にし、slmp の選択肢ラベルには
	 * 未対応である旨を明記する（監査レビュー指摘、2026-08-05）。
	 *
	 * 削除は、収集グループが参照している場合にサービス層の Validation
	 * エラー（「…収集グループが N 件あるため削除できません」）で拒否されるので、
	 * それをトーストで表示する。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listPlcConnections,
		createPlcConnection,
		updatePlcConnection,
		deletePlcConnection,
		type PlcConnection,
		type PlcConnectionInput,
		type PlcProtocol
	} from '$lib/banto/tagRegistryAdmin';

	const protocolOptions: { value: PlcProtocol; label: string }[] = [
		{ value: 'modbus-tcp', label: 'Modbus TCP' },
		// banto-collect が未対応（上の doc comment 参照）: 選択肢としては残すが
		// 収集は動かないことをラベルで明示する。
		{ value: 'slmp', label: 'SLMP（MELSEC）— 収集は未対応（banto-collect 拡張待ち）' }
	];

	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	/** 編集フォーム状態（作成/編集共通）。数値入力は文字列で保持し、空欄=未設定。 */
	interface FormState {
		name: string;
		protocol: PlcProtocol;
		host: string;
		port: string;
		unitId: string;
		enabled: boolean;
	}

	function blankForm(): FormState {
		return {
			name: '',
			// バックエンドの既定（PlcConnectionPayload の default_plc_protocol）
			// と一致させる。かつ現状収集できるのは modbus-tcp のみ（上の doc
			// comment 参照）。
			protocol: 'modbus-tcp',
			host: '',
			port: '502',
			unitId: '1',
			enabled: true
		};
	}

	function formFromConnection(c: PlcConnection): FormState {
		return {
			name: c.name,
			protocol: c.protocol,
			host: c.host,
			port: String(c.port),
			unitId: String(c.unitId),
			enabled: c.enabled
		};
	}

	function toInput(form: FormState): PlcConnectionInput {
		return {
			name: form.name,
			protocol: form.protocol,
			host: form.host,
			port: Number(form.port),
			unitId: Number(form.unitId),
			enabled: form.enabled
		};
	}

	let connections: PlcConnection[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		loading = true;
		try {
			connections = await listPlcConnections();
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
			await createPlcConnection(toInput(createForm));
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
	let selected: PlcConnection | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectConnection(c: PlcConnection): void {
		selected = c;
		editForm = formFromConnection(c);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updatePlcConnection(selected.id, toInput(editForm));
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
			await deletePlcConnection(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			// 収集グループが参照している場合はサービス層の分かりやすい
			// Validation エラー（件数入り）がここに来る。
			toastStore.push('error', errorMessage(err));
		}
	}

	const columns: GridColumn<PlcConnection>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 60, align: 'right' },
		{
			id: 'name',
			header: '名前',
			accessor: 'name',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{ id: 'protocol', header: 'プロトコル', accessor: 'protocol', width: 110 },
		{ id: 'host', header: 'ホスト', accessor: 'host', width: 140 },
		{ id: 'port', header: 'ポート', accessor: 'port', width: 80, align: 'right' },
		{ id: 'unitId', header: 'ユニットID', accessor: 'unitId', width: 90, align: 'right' },
		{
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		}
	];
</script>

{#snippet connectionFields(form: FormState, errors: Record<string, string>)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			プロトコル
			<select bind:value={form.protocol}>
				{#each protocolOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.protocol}<span class="err">{errors.protocol}</span>{/if}
		</label>
		<label class="field">
			ホスト
			<input type="text" bind:value={form.host} placeholder="192.168.1.10" />
			{#if errors.host}<span class="err">{errors.host}</span>{/if}
		</label>
		<label class="field">
			ポート
			<input type="number" min="1" max="65535" bind:value={form.port} />
			{#if errors.port}<span class="err">{errors.port}</span>{/if}
		</label>
		<label class="field">
			ユニットID
			<input type="number" min="0" max="255" bind:value={form.unitId} />
			<span class="hint">Modbus 用のスレーブID（0〜255）。SLMP では未使用（既定 1 のまま）。</span>
			{#if errors.unitId}<span class="err">{errors.unitId}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
{/snippet}

<div class="page">
	<h2>PLC接続</h2>

	{#if canWrite}
		<section class="create">
			<h3>新規作成</h3>
			{@render connectionFields(createForm, createErrors)}
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
		{#if loading && connections.length === 0}
			<p class="loading">読み込み中…</p>
		{:else}
			<div class="grid-wrap">
				<BantoGrid
					rows={connections}
					{columns}
					getRowId={(c) => c.id}
					onRowClick={canWrite ? selectConnection : undefined}
				/>
			</div>
		{/if}
	</section>

	{#if selected && canWrite}
		<section class="detail">
			<h3>{selected.name} を編集</h3>
			{@render connectionFields(editForm, editErrors)}
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
