<script lang="ts">
	/**
	 * 書き込みルール（write_rules）CRUD 画面（plan W2）。write-targets/+page.svelte
	 * と同じ構造（BantoGrid一覧＋行クリックで下に編集パネル、生input／select、
	 * viewer=閲覧のみ／editor以上=作成・編集・削除、デモモード案内）を踏襲する。
	 *
	 * ルールは 1..N のAND条件（write_rule_conditions）を持つ集約
	 * （`relay_wright_core::write_rules::WriteRuleService` がルール＋条件を
	 * 1トランザクションで読み書きする）なので、この画面固有の追加が2つある。
	 *
	 * 1. 条件は独立CRUDを持たない。作成/編集フォームにインラインの行リストとして
	 *    埋め込み、「+ 条件を追加」「削除」で行を増減し、保存時にルール全体を
	 *    `conditions: WriteRuleConditionInput[]` としてまとめて送る
	 *    （`{#snippet conditionsEditor}` を create/edit 両フォームで共用）。
	 * 2. 書き込み先（writeTargetId）は同じW2で追加された /api/write-targets
	 *    一覧APIがあるのでプルダウンにする。一方 source_tag_id /
	 *    writeSourceTagId が参照する banto-tags の `tags` テーブルには、この
	 *    アプリのフロント/REST/Tauri面にまだ一覧APIが無い（バックエンドの
	 *    write_rules.rs 側は存在チェックのみ行う）。そのため write-targets の
	 *    plcConnectionId と同じ考え方で、数値ID直接入力のフォールバックにして
	 *    いる（存在しないIDはサーバー側の分かりやすいバリデーションエラーで
	 *    弾かれる）。タグ一覧APIが追加され次第プルダウン化するのはW3以降。
	 *
	 * 書き込みループ検出（サービス層のcheck_no_write_cycle）が弾いた場合は
	 * `enabled` フィールドのバリデーションエラーとして返るので、フォーム全体の
	 * 上に目立つエラーとして表示する。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listWriteRules,
		createWriteRule,
		updateWriteRule,
		deleteWriteRule,
		listWriteTargets,
		isWriteRegistryAvailable,
		DEMO_MODE_MESSAGE,
		type WriteRuleDetail,
		type WriteRuleInput,
		type WriteRuleConditionInput,
		type WriteTarget,
		type EdgeMode,
		type WriteValueMode,
		type ConditionOperator
	} from '$lib/banto/writeRegistryAdmin';

	const edgeModeOptions: { value: EdgeMode; label: string }[] = [
		{ value: 'rising', label: '立ち上がり' },
		{ value: 'falling', label: '立ち下がり' },
		{ value: 'change', label: '変化時' }
	];

	const writeValueModeOptions: { value: WriteValueMode; label: string }[] = [
		{ value: 'constant', label: '定数' },
		{ value: 'copy_from_source', label: 'ソースタグをコピー' }
	];

	const operatorOptions: { value: ConditionOperator; label: string }[] = [
		{ value: 'eq', label: '等しい (=)' },
		{ value: 'neq', label: '等しくない (≠)' },
		{ value: 'gt', label: 'より大きい (>)' },
		{ value: 'gte', label: '以上 (≥)' },
		{ value: 'lt', label: 'より小さい (<)' },
		{ value: 'lte', label: '以下 (≤)' },
		{ value: 'between', label: '範囲内 (between)' },
		{ value: 'bit_is', label: 'ビット値 (bit_is)' }
	];

	function edgeModeLabel(v: EdgeMode): string {
		return edgeModeOptions.find((o) => o.value === v)?.label ?? v;
	}

	function writeValueModeLabel(v: WriteValueMode): string {
		return writeValueModeOptions.find((o) => o.value === v)?.label ?? v;
	}

	const available = isWriteRegistryAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	/** One AND-combined condition row, string-backed for empty=unset numeric inputs. */
	interface ConditionFormState {
		sourceTagId: string;
		operator: ConditionOperator;
		thresholdValue: string;
		thresholdValue2: string;
	}

	function blankCondition(): ConditionFormState {
		return { sourceTagId: '', operator: 'gt', thresholdValue: '', thresholdValue2: '' };
	}

	/** Editable form state (shared by create + edit). */
	interface FormState {
		name: string;
		enabled: boolean;
		edgeMode: EdgeMode;
		cooldownMs: string;
		writeTargetId: string;
		writeValueMode: WriteValueMode;
		writeConstantValue: string;
		writeSourceTagId: string;
		conditions: ConditionFormState[];
	}

	function blankForm(): FormState {
		return {
			name: '',
			enabled: true,
			edgeMode: 'rising',
			cooldownMs: '',
			writeTargetId: '',
			writeValueMode: 'constant',
			writeConstantValue: '',
			writeSourceTagId: '',
			conditions: [blankCondition()]
		};
	}

	function formFromRule(detail: WriteRuleDetail): FormState {
		return {
			name: detail.name,
			enabled: detail.enabled,
			edgeMode: detail.edgeMode,
			cooldownMs: detail.cooldownMs === null ? '' : String(detail.cooldownMs),
			writeTargetId: String(detail.writeTargetId),
			writeValueMode: detail.writeValueMode,
			writeConstantValue:
				detail.writeConstantValue === null ? '' : String(detail.writeConstantValue),
			writeSourceTagId: detail.writeSourceTagId === null ? '' : String(detail.writeSourceTagId),
			conditions:
				detail.conditions.length > 0
					? detail.conditions.map((c) => ({
							sourceTagId: String(c.sourceTagId),
							operator: c.operator,
							thresholdValue: String(c.thresholdValue),
							thresholdValue2: c.thresholdValue2 === null ? '' : String(c.thresholdValue2)
						}))
					: [blankCondition()]
		};
	}

	function numOrNull(value: string): number | null {
		const trimmed = value.trim();
		return trimmed === '' ? null : Number(trimmed);
	}

	function toInput(form: FormState): WriteRuleInput {
		const conditions: WriteRuleConditionInput[] = form.conditions.map((c) => ({
			sourceTagId: Number(c.sourceTagId),
			operator: c.operator,
			thresholdValue: Number(c.thresholdValue),
			thresholdValue2: c.operator === 'between' ? numOrNull(c.thresholdValue2) : null
		}));
		return {
			name: form.name,
			enabled: form.enabled,
			edgeMode: form.edgeMode,
			cooldownMs: numOrNull(form.cooldownMs),
			writeTargetId: Number(form.writeTargetId),
			writeValueMode: form.writeValueMode,
			writeConstantValue:
				form.writeValueMode === 'constant' ? numOrNull(form.writeConstantValue) : null,
			writeSourceTagId:
				form.writeValueMode === 'copy_from_source' ? numOrNull(form.writeSourceTagId) : null,
			conditions
		};
	}

	function addCondition(form: FormState): void {
		form.conditions = [...form.conditions, blankCondition()];
	}

	function removeCondition(form: FormState, index: number): void {
		if (form.conditions.length <= 1) return;
		form.conditions = form.conditions.filter((_, i) => i !== index);
	}

	let rules: WriteRuleDetail[] = $state([]);
	let targets: WriteTarget[] = $state([]);
	let loading = $state(false);

	function targetName(id: number): string {
		return targets.find((t) => t.id === id)?.name ?? `#${id}`;
	}

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			const [ruleList, targetList] = await Promise.all([listWriteRules(), listWriteTargets()]);
			rules = ruleList;
			targets = targetList;
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
			await createWriteRule(toInput(createForm));
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
	let selected: WriteRuleDetail | null = $state(null);
	let editForm = $state(blankForm());
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectRule(r: WriteRuleDetail): void {
		selected = r;
		editForm = formFromRule(r);
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateWriteRule(selected.id, toInput(editForm));
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
			await deleteWriteRule(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	const columns: GridColumn<WriteRuleDetail>[] = [
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
			id: 'enabled',
			header: '有効',
			accessor: 'enabled',
			width: 70,
			format: (v) => (v ? 'はい' : 'いいえ')
		},
		{
			id: 'edgeMode',
			header: 'エッジモード',
			accessor: 'edgeMode',
			width: 110,
			format: (v) => edgeModeLabel(v as EdgeMode)
		},
		{
			id: 'writeTarget',
			header: '書き込み先',
			accessor: (r) => targetName(r.writeTargetId),
			width: 150
		},
		{
			id: 'writeValueMode',
			header: '書き込み値',
			accessor: 'writeValueMode',
			width: 140,
			format: (v) => writeValueModeLabel(v as WriteValueMode)
		},
		{
			id: 'conditionsCount',
			header: '条件数',
			accessor: (r) => r.conditions.length,
			width: 70,
			align: 'right'
		}
	];
</script>

{#snippet conditionsEditor(form: FormState, errors: Record<string, string>)}
	<div class="conditions">
		<div class="conditions-header">
			<span>条件（すべてANDで判定されます）</span>
			<button type="button" class="small" onclick={() => addCondition(form)}>+ 条件を追加</button>
		</div>
		{#if errors.conditions}<p class="err">{errors.conditions}</p>{/if}
		{#each form.conditions as condition, i (i)}
			<div class="condition-row">
				<label class="field">
					ソースタグID
					<input type="number" bind:value={condition.sourceTagId} />
					{#if errors[`conditions.${i}.sourceTagId`]}<span class="err"
							>{errors[`conditions.${i}.sourceTagId`]}</span
						>{/if}
				</label>
				<label class="field">
					演算子
					<select bind:value={condition.operator}>
						{#each operatorOptions as op (op.value)}
							<option value={op.value}>{op.label}</option>
						{/each}
					</select>
					{#if errors[`conditions.${i}.operator`]}<span class="err"
							>{errors[`conditions.${i}.operator`]}</span
						>{/if}
				</label>
				<label class="field">
					しきい値
					<input type="number" bind:value={condition.thresholdValue} />
					{#if errors[`conditions.${i}.thresholdValue`]}<span class="err"
							>{errors[`conditions.${i}.thresholdValue`]}</span
						>{/if}
				</label>
				{#if condition.operator === 'between'}
					<label class="field">
						上限値
						<input type="number" bind:value={condition.thresholdValue2} />
						{#if errors[`conditions.${i}.thresholdValue2`]}<span class="err"
								>{errors[`conditions.${i}.thresholdValue2`]}</span
							>{/if}
					</label>
				{/if}
				<button
					type="button"
					class="small danger"
					onclick={() => removeCondition(form, i)}
					disabled={form.conditions.length <= 1}
				>
					削除
				</button>
			</div>
		{/each}
	</div>
{/snippet}

{#snippet ruleFields(form: FormState, errors: Record<string, string>)}
	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			エッジモード
			<select bind:value={form.edgeMode}>
				{#each edgeModeOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.edgeMode}<span class="err">{errors.edgeMode}</span>{/if}
		</label>
		<label class="field">
			クールダウン(ms)
			<input type="number" min="0" bind:value={form.cooldownMs} />
			{#if errors.cooldownMs}<span class="err">{errors.cooldownMs}</span>{/if}
		</label>
		<label class="field">
			書き込み先
			<select bind:value={form.writeTargetId}>
				<option value="">選択してください</option>
				{#each targets as t (t.id)}
					<option value={String(t.id)}>{t.name}</option>
				{/each}
			</select>
			{#if errors.writeTargetId}<span class="err">{errors.writeTargetId}</span>{/if}
		</label>
		<label class="field">
			書き込み値モード
			<select bind:value={form.writeValueMode}>
				{#each writeValueModeOptions as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			{#if errors.writeValueMode}<span class="err">{errors.writeValueMode}</span>{/if}
		</label>
		{#if form.writeValueMode === 'constant'}
			<label class="field">
				定数値
				<input type="number" bind:value={form.writeConstantValue} />
				{#if errors.writeConstantValue}<span class="err">{errors.writeConstantValue}</span>{/if}
			</label>
		{:else}
			<label class="field">
				参照元タグID
				<input type="number" bind:value={form.writeSourceTagId} />
				{#if errors.writeSourceTagId}<span class="err">{errors.writeSourceTagId}</span>{/if}
			</label>
		{/if}
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>
	{#if errors.enabled}<p class="err cycle-err">{errors.enabled}</p>{/if}
	{@render conditionsEditor(form, errors)}
{/snippet}

<div class="page">
	<h2>書き込みルール</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはレジストリDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		{#if canWrite}
			<section class="create">
				<h3>新規作成</h3>
				{@render ruleFields(createForm, createErrors)}
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
			{#if loading && rules.length === 0}
				<p class="loading">読み込み中…</p>
			{:else}
				<div class="grid-wrap">
					<BantoGrid
						rows={rules}
						{columns}
						getRowId={(r) => r.id}
						onRowClick={canWrite ? selectRule : undefined}
					/>
				</div>
			{/if}
		</section>

		{#if selected && canWrite}
			<section class="detail">
				<h3>{selected.name} を編集</h3>
				{@render ruleFields(editForm, editErrors)}
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

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}

	.cycle-err {
		margin: 0 0 0.75rem;
		padding: 0.5rem 0.75rem;
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
	}

	.conditions {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.75rem;
		margin-bottom: 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.conditions-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		font-size: 0.85rem;
		font-weight: 600;
	}

	.condition-row {
		display: flex;
		align-items: flex-end;
		gap: 0.6rem;
		flex-wrap: wrap;
		padding: 0.5rem;
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.condition-row .field {
		min-width: 130px;
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
</style>
