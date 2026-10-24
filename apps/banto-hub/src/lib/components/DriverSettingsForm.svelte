<script lang="ts">
	/**
	 * R2b-6c（docs/banto-hub-driver-ui-design.md §6.1）: 機器のドライバの接続設定の欄を
	 * `GET /api/drivers` のスキーマ（`DriverSettingField[]`）から描く汎用部品。ドライバごとの
	 * 分岐は持たない（欄の種類 → 部品だけ）。判断（既定値・検査・送る形・引き継ぎ）は
	 * `#lib/banto/driverSettingsForm.ts` の純関数。
	 *
	 * - Text・Host → `input`、Integer → `input type=number`、Choice → `select`、
	 *   Bool → チェックボックス、Secret → 「設定済み（変えない）・変える・消す」と入力。
	 *   保存済みの値は決して画面に出さない（`$secret` の状態だけ）。
	 * - `group: 'advanced'` の欄は「詳細」に畳む。`help` は欄の下、エラーは欄の下に key で出す。
	 * - `readOnly` なら全部 `disabled`（表示も同じ部品で描く）。
	 */
	import type { DriverSettingField } from '#lib/banto/tagRegistryAdmin.js';
	import {
		isSecretFormValue,
		type DriverFormState,
		type SecretFormValue,
		type SecretMode
	} from '#lib/banto/driverSettingsForm.js';

	interface Props {
		/** `DriverInfo.settingsSchema.fields`。 */
		schema: readonly DriverSettingField[];
		/** 欄の値と、利用者が触った欄の記録。部品が直接書き換える。 */
		value: DriverFormState;
		/** 欄の key → エラーの文言。 */
		errors?: Record<string, string>;
		readOnly?: boolean;
		/** 同じ画面に部品が複数あっても id が衝突しないための接頭辞。 */
		idPrefix?: string;
	}

	let {
		schema,
		value = $bindable(),
		errors = {},
		readOnly = false,
		idPrefix = 'drv'
	}: Props = $props();

	const basicFields = $derived(schema.filter((f) => f.group !== 'advanced'));
	const advancedFields = $derived(schema.filter((f) => f.group === 'advanced'));

	function touch(key: string): void {
		value.touched[key] = true;
	}

	function textOf(key: string): string {
		const v = value.values[key];
		return typeof v === 'string' ? v : '';
	}

	function secretOf(key: string): SecretFormValue {
		const v = value.values[key];
		return isSecretFormValue(v) ? v : { mode: 'change', text: '', state: 'none' };
	}

	function setSecretMode(key: string, mode: SecretMode): void {
		const secret = secretOf(key);
		value.values[key] = { ...secret, mode, text: mode === 'change' ? secret.text : '' };
		touch(key);
	}

	function setSecretText(key: string, text: string): void {
		value.values[key] = { ...secretOf(key), mode: 'change', text };
		touch(key);
	}

	const SECRET_STATE_TEXT = {
		set: '設定済み',
		unreadable: '読めません（この PC で再入力してください）'
	} as const;

	function inputId(key: string): string {
		return `${idPrefix}-${key}`;
	}

	/** 欄の key に対応する `hint`/`err` の id（`aria-describedby` 用）。 */
	function describedBy(field: DriverSettingField): string | undefined {
		const ids: string[] = [];
		if (field.help) ids.push(`${inputId(field.key)}-help`);
		if (errors[field.key]) ids.push(`${inputId(field.key)}-err`);
		return ids.length > 0 ? ids.join(' ') : undefined;
	}
</script>

{#snippet fieldBlock(field: DriverSettingField)}
	{@const id = inputId(field.key)}
	{@const kind = field.kind}
	<div class="driver-field" data-testid={`driver-field-${field.key}`}>
		{#if kind.type === 'bool'}
			<label class="field checkbox" for={id}>
				<input
					{id}
					type="checkbox"
					checked={value.values[field.key] === true}
					onchange={(e) => {
						value.values[field.key] = e.currentTarget.checked;
						touch(field.key);
					}}
					disabled={readOnly}
					aria-describedby={describedBy(field)}
				/>
				{field.label}
			</label>
		{:else if kind.type === 'secret'}
			{@const secret = secretOf(field.key)}
			<div class="field" role="group" aria-labelledby={`${id}-label`}>
				<span id={`${id}-label`}>{field.label}{field.required ? ' *' : ''}</span>
				{#if secret.state !== 'none'}
					<span
						class="secret-state"
						class:unreadable={secret.state === 'unreadable'}
						role={secret.state === 'unreadable' ? 'alert' : undefined}
						data-testid={`driver-secret-state-${field.key}`}
					>
						{SECRET_STATE_TEXT[secret.state]}
					</span>
					{#if !readOnly}
						<div class="secret-modes">
							{#each [['keep', '変えない'], ['change', '変える'], ['clear', '消す']] as const as [mode, text] (mode)}
								<label class="secret-mode">
									<input
										type="radio"
										name={`${id}-mode`}
										checked={secret.mode === mode}
										onchange={() => setSecretMode(field.key, mode)}
									/>
									{text}
								</label>
							{/each}
						</div>
					{/if}
				{/if}
				{#if !readOnly && (secret.state === 'none' || secret.mode === 'change')}
					<input
						{id}
						type="password"
						autocomplete="new-password"
						value={secret.text}
						oninput={(e) => setSecretText(field.key, e.currentTarget.value)}
						placeholder={secret.state === 'none' ? '' : '新しい値'}
						aria-label={field.label}
						aria-describedby={describedBy(field)}
					/>
				{/if}
			</div>
		{:else}
			<label class="field" for={id}>
				{field.label}{field.required ? ' *' : ''}
				{#if kind.type === 'choice'}
					<select
						{id}
						value={textOf(field.key)}
						onchange={(e) => {
							value.values[field.key] = e.currentTarget.value;
							touch(field.key);
						}}
						disabled={readOnly}
						aria-describedby={describedBy(field)}
					>
						{#if !field.required}<option value="">（既定）</option>{/if}
						{#each kind.options as opt (opt.value)}
							<option value={opt.value}>{opt.label}</option>
						{/each}
					</select>
				{:else if kind.type === 'integer'}
					<input
						{id}
						type="number"
						step="1"
						min={kind.min ?? undefined}
						max={kind.max ?? undefined}
						value={textOf(field.key)}
						oninput={(e) => {
							value.values[field.key] = e.currentTarget.value;
							touch(field.key);
						}}
						disabled={readOnly}
						aria-describedby={describedBy(field)}
					/>
				{:else}
					<input
						{id}
						type="text"
						value={textOf(field.key)}
						oninput={(e) => {
							value.values[field.key] = e.currentTarget.value;
							touch(field.key);
						}}
						placeholder={kind.type === 'text'
							? (kind.placeholder ?? undefined)
							: kind.type === 'host'
								? '192.168.1.10'
								: undefined}
						maxlength={kind.type === 'text' ? (kind.max_len ?? undefined) : undefined}
						disabled={readOnly}
						autocomplete="off"
						aria-describedby={describedBy(field)}
					/>
				{/if}
			</label>
		{/if}
		{#if field.help}<span class="hint" id={`${id}-help`}>{field.help}</span>{/if}
		{#if errors[field.key]}<span class="err" id={`${id}-err`}>{errors[field.key]}</span>{/if}
	</div>
{/snippet}

<div class="driver-fields">
	{#each basicFields as field (field.key)}
		{@render fieldBlock(field)}
	{/each}
	{#if advancedFields.length > 0}
		<details
			class="advanced"
			open={advancedFields.some((f) => errors[f.key])}
			data-testid="driver-advanced"
		>
			<summary>詳細</summary>
			<div class="advanced-fields">
				{#each advancedFields as field (field.key)}
					{@render fieldBlock(field)}
				{/each}
			</div>
		</details>
	{/if}
</div>

<style>
	.driver-fields {
		grid-column: 1 / -1;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 0.75rem;
	}

	.advanced {
		grid-column: 1 / -1;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
	}

	.advanced summary {
		cursor: pointer;
		margin-bottom: 0.5rem;
	}

	.advanced-fields {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
		gap: 0.75rem;
	}

	.driver-field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
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

	.field.checkbox input,
	.secret-mode input {
		width: auto;
	}

	.secret-modes {
		display: flex;
		gap: 0.75rem;
	}

	.secret-mode {
		display: flex;
		align-items: center;
		gap: 0.3rem;
	}

	.secret-state.unreadable {
		color: var(--banto-warning);
		font-weight: 600;
	}

	.hint {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}

	.err {
		color: var(--banto-danger);
		font-size: 0.75rem;
	}
</style>
