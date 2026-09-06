<script lang="ts">
	/**
	 * S6（docs/banto-hub-external-db-design.md §5.2・§5.3・§7 row S6）: sink
	 * group（`hub_sink_groups`）の作成・再設定・削除を担う自己完結コンポーネント
	 * - `ConnectionDrawer.svelte`/`CollectionGroupDrawer.svelte`と同じ「`Drawer`
	 * を内包し、呼び出し側は `<SinkGroupDrawer open={...} group={...} .../>` を
	 * 並べるだけでよい」形。
	 *
	 * **作成・再設定とも単一フォーム（ウィザードにしない）**: PLC 接続とは
	 * 違い「接続テスト」に相当する工程が無く、項目数も少ないため、
	 * `ConnectionDrawer`/`CollectionGroupDrawer`の3ステップウィザードは
	 * 過剰と判断した - 常に`Drawer.svelte`（右ペイン）で描画する。
	 *
	 * **稼働中でも即時反映**（設計 §6-13）: sink group の CRUD は PLC 収集
	 * パイプラインに一切影響しないため pending queue には載らない - 他の
	 * Drawer（`ConnectionDrawer`/`CollectionGroupDrawer`）が持つ
	 * `QueuedWhileRunningError`（202応答）の分岐は無い（常に確定応答）。
	 *
	 * 純関数部分（検証・DDL 組み立て・フォーム⇄API入力変換）は
	 * `$lib/banto/sinkGroupForm.ts` へ切り出し済み（そちらでユニットテスト
	 * 済み）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import Drawer from './Drawer.svelte';
	import { toastStore } from '$lib/toast.svelte';
	import {
		createSinkGroup,
		deleteSinkGroup,
		updateSinkGroup,
		type SinkGroup
	} from '$lib/banto/sinkGroupsAdmin';
	import {
		isDbSourceConnection,
		type CollectionGroup,
		type PlcConnection,
		type Tag
	} from '$lib/banto/tagRegistryAdmin';
	import {
		SINK_MODE_OPTIONS,
		blankSinkGroupForm,
		buildRecommendedDdl,
		formToSinkGroupInput,
		intervalMsLabel,
		sinkGroupToForm,
		validateSinkGroupForm,
		type SinkGroupFormState
	} from '$lib/banto/sinkGroupForm';
	import { nextSequentialName } from '$lib/banto/sequentialName';

	interface Props {
		open: boolean;
		/** `null` なら新規作成。非 `null` ならそのグループの再設定。 */
		group: SinkGroup | null;
		/** 新規作成時の連番プリフィルに使う既存グループ名一覧。 */
		existingNames: string[];
		/** dbConnectionId の選択肢（`isDbSourceConnection`で絞り込んだ postgres 接続だけを渡す想定 - 全件渡しても本コンポーネント側で絞る）。 */
		connections: PlcConnection[];
		/** タグ選択肢のグループ化表示（接続/グループ名の解決）に使う。 */
		collectionGroups: CollectionGroup[];
		/** タグ選択肢そのもの（カタログ全件）。 */
		tags: Tag[];
		onClose: () => void;
		onSaved: (group: SinkGroup) => void;
		onDeleted: (id: number) => void;
	}

	let {
		open,
		group,
		existingNames,
		connections,
		collectionGroups,
		tags,
		onClose,
		onSaved,
		onDeleted
	}: Props = $props();

	const isCreate = $derived(group === null);
	const drawerTitle = $derived(isCreate ? 'sink group を作成' : `${group?.name} を編集`);

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

	/** `isDbSourceConnection`で絞った postgres 接続だけを選択肢にする（設計 §7 row S6 実装指示1「postgres 接続のみ」）。 */
	const dbConnectionOptions = $derived(connections.filter(isDbSourceConnection));

	let form: SinkGroupFormState = $state(blankSinkGroupForm());
	let errors: Record<string, string> = $state({});
	let saving = $state(false);
	let deleting = $state(false);
	let tagSearch = $state('');
	let lastOpenKey: string | null = null;

	$effect(() => {
		if (!open) {
			lastOpenKey = null;
			return;
		}
		const key = group ? `edit:${group.id}` : 'create';
		if (key === lastOpenKey) return;
		lastOpenKey = key;

		if (group) {
			form = sinkGroupToForm(group);
		} else {
			const blank = blankSinkGroupForm();
			blank.name = nextSequentialName(existingNames, 'sink-group');
			if (dbConnectionOptions.length > 0) {
				blank.dbConnectionId = String(dbConnectionOptions[0].id);
			}
			form = blank;
		}
		errors = {};
		tagSearch = '';
	});

	/** DDL 表示用 - 未入力・不正な `tableName` でもプレビューはそのまま組み立てる（検証エラーは別途 `errors.tableName` に出す）。 */
	const recommendedDdl = $derived(
		form.tableName.trim() === '' ? null : buildRecommendedDdl(form.tableName.trim())
	);

	let ddlCopied = $state(false);
	async function copyDdl(): Promise<void> {
		if (!recommendedDdl) return;
		try {
			await navigator.clipboard.writeText(recommendedDdl);
			ddlCopied = true;
			toastStore.push('success', 'コピーしました');
		} catch {
			toastStore.push('error', 'コピーに失敗しました。手動で選択してコピーしてください。');
		}
	}
	$effect(() => {
		// tableName が変わったらコピー済み表示をリセットする。
		void form.tableName;
		ddlCopied = false;
	});

	/** タグ選択肢を「接続.グループ」単位でグループ化して表示する（設計 §7 row S6 実装指示1「グループ化」）。 */
	interface TagGroupNode {
		key: string;
		connectionName: string;
		groupName: string;
		tags: Tag[];
	}

	function tagMatchesSearch(tag: Tag, connectionName: string, groupName: string): boolean {
		const q = tagSearch.trim().toLowerCase();
		if (q === '') return true;
		return (
			tag.name.toLowerCase().includes(q) ||
			tag.address.toLowerCase().includes(q) ||
			connectionName.toLowerCase().includes(q) ||
			groupName.toLowerCase().includes(q)
		);
	}

	function computeGroupedTags(): TagGroupNode[] {
		const nodes = new Map<string, TagGroupNode>();
		for (const tag of tags) {
			const collectionGroup = collectionGroups.find((g) => g.id === tag.collectionGroupId);
			const connection = collectionGroup
				? connections.find((c) => c.id === collectionGroup.plcConnectionId)
				: undefined;
			const connectionName = connection?.name ?? '(不明な接続)';
			const groupName = collectionGroup?.name ?? '(不明なグループ)';
			if (!tagMatchesSearch(tag, connectionName, groupName)) continue;
			const key = `${connection?.id ?? 0}:${collectionGroup?.id ?? 0}`;
			let node = nodes.get(key);
			if (!node) {
				node = { key, connectionName, groupName, tags: [] };
				nodes.set(key, node);
			}
			node.tags.push(tag);
		}
		return [...nodes.values()].sort(
			(a, b) =>
				a.connectionName.localeCompare(b.connectionName) || a.groupName.localeCompare(b.groupName)
		);
	}
	const groupedTags = $derived(computeGroupedTags());

	function isTagSelected(id: number): boolean {
		return form.tagIds.includes(id);
	}

	function toggleTag(id: number): void {
		form.tagIds = isTagSelected(id)
			? form.tagIds.filter((existing) => existing !== id)
			: [...form.tagIds, id];
	}

	function tagCountLabel(): string {
		return `${form.tagIds.length}件選択中`;
	}

	function validateBeforeSubmit(): boolean {
		const fieldErrors = validateSinkGroupForm(form);
		if (Object.keys(fieldErrors).length === 0) return true;
		errors = fieldErrors;
		return false;
	}

	async function handleCreate(): Promise<void> {
		if (!validateBeforeSubmit()) return;
		saving = true;
		errors = {};
		try {
			const created = await createSinkGroup(formToSinkGroupInput(form));
			toastStore.push('success', '作成しました');
			onSaved(created);
			onClose();
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) errors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleSave(): Promise<void> {
		if (!group) return;
		if (!validateBeforeSubmit()) return;
		saving = true;
		errors = {};
		try {
			const updated = await updateSinkGroup(group.id, formToSinkGroupInput(form));
			toastStore.push('success', '更新しました');
			form = sinkGroupToForm(updated);
			onSaved(updated);
		} catch (err) {
			const fieldErrors = applyFieldErrors(err);
			if (fieldErrors) errors = fieldErrors;
			else toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!group) return;
		if (!window.confirm(`sink group '${group.name}' を削除しますか？`)) return;
		deleting = true;
		try {
			await deleteSinkGroup(group.id);
			toastStore.push('success', '削除しました');
			onDeleted(group.id);
			onClose();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			deleting = false;
		}
	}

	function isBusy(): boolean {
		return saving || deleting;
	}

	function onRequestClose(): boolean {
		return !isBusy();
	}
</script>

<Drawer {open} title={drawerTitle} {onRequestClose} onclose={onClose} width="560px">
	<p class="note">
		変更は即時に反映されます（保留中の変更キューには載りません）。サイドカーはこの変更を
		<code>config_refresh_secs</code>（既定30秒）以内に取り込みます。
	</p>

	<div class="form-grid">
		<label class="field">
			名前
			<input type="text" bind:value={form.name} />
			{#if errors.name}<span class="err">{errors.name}</span>{/if}
		</label>
		<label class="field">
			DB 接続（postgres のみ）
			<select bind:value={form.dbConnectionId}>
				<option value="">（未選択）</option>
				{#each dbConnectionOptions as conn (conn.id)}
					<option value={String(conn.id)}>{conn.name}</option>
				{/each}
			</select>
			{#if dbConnectionOptions.length === 0}
				<span class="hint">postgres 接続がありません。先にタグ画面から接続を作成してください。</span
				>
			{/if}
			{#if errors.dbConnectionId}<span class="err">{errors.dbConnectionId}</span>{/if}
		</label>
		<label class="field">
			mode
			<select bind:value={form.mode}>
				{#each SINK_MODE_OPTIONS as opt (opt.value)}
					<option value={opt.value}>{opt.label}</option>
				{/each}
			</select>
			<span class="hint">
				{SINK_MODE_OPTIONS.find((o) => o.value === form.mode)?.hint}
			</span>
			{#if errors.mode}<span class="err">{errors.mode}</span>{/if}
		</label>
		<label class="field">
			{intervalMsLabel(form.mode)}
			<input type="number" min="100" max="3600000" bind:value={form.intervalMs} />
			{#if errors.intervalMs}<span class="err">{errors.intervalMs}</span>{/if}
		</label>
		<label class="field wide">
			保存先テーブル（schema.table 可）
			<input type="text" bind:value={form.tableName} placeholder="public.tag_history" />
			<span class="hint">
				英字/アンダースコアで始まる識別子。引用符・スキーマ2階層より深い指定はできません。
			</span>
			{#if errors.tableName}<span class="err">{errors.tableName}</span>{/if}
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.storeBad} />
			Bad / Stale の行も保存する
		</label>
		<label class="field checkbox">
			<input type="checkbox" bind:checked={form.enabled} />
			有効
		</label>
	</div>

	{#if recommendedDdl}
		<div class="ddl-block">
			<div class="ddl-header">
				<span>推奨 DDL</span>
				<button type="button" class="secondary" onclick={copyDdl}>
					{ddlCopied ? 'コピー済み' : 'コピー'}
				</button>
			</div>
			<pre class="ddl-code">{recommendedDdl}</pre>
			<p class="hint">
				Hub とサイドカーは DDL を発行しません。DB 管理者がこのテーブルを用意してください（INSERT
				権限のみで動作）。
			</p>
		</div>
	{/if}

	<div class="tag-picker">
		<div class="tag-picker-header">
			<span>対象タグ（{tagCountLabel()}）</span>
			<input
				type="search"
				placeholder="検索（タグ名・アドレス・接続・グループ）"
				bind:value={tagSearch}
			/>
		</div>
		{#if errors.tagIds}<span class="err">{errors.tagIds}</span>{/if}
		<div class="tag-list">
			{#if groupedTags.length === 0}
				<p class="note">該当するタグがありません。</p>
			{:else}
				{#each groupedTags as node (node.key)}
					<div class="tag-group">
						<h4>{node.connectionName}.{node.groupName}</h4>
						{#each node.tags as tag (tag.id)}
							<label class="tag-row">
								<input
									type="checkbox"
									checked={isTagSelected(tag.id)}
									onchange={() => toggleTag(tag.id)}
								/>
								<span class="tag-name">{node.connectionName}.{node.groupName}.{tag.name}</span>
								<span class="tag-address">{tag.address}</span>
							</label>
						{/each}
					</div>
				{/each}
			{/if}
		</div>
	</div>

	<div class="actions">
		{#if isCreate}
			<button type="button" onclick={handleCreate} disabled={saving}>作成</button>
		{:else}
			<button type="button" onclick={handleSave} disabled={saving || deleting}>保存</button>
			<button type="button" class="danger" onclick={handleDelete} disabled={saving || deleting}>
				削除
			</button>
		{/if}
	</div>
</Drawer>

<style>
	.note {
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		margin: 0 0 0.75rem;
	}

	.form-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
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

	.field.wide {
		grid-column: 1 / -1;
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

	.ddl-block {
		margin-bottom: 0.75rem;
		padding: 0.6rem 0.75rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
	}

	.ddl-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.4rem;
		font-size: 0.8rem;
		font-weight: 600;
	}

	.ddl-code {
		margin: 0 0 0.4rem;
		padding: 0.5rem;
		background: var(--banto-surface);
		border-radius: var(--banto-radius);
		font-size: 0.75rem;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.tag-picker {
		margin-bottom: 0.75rem;
	}

	.tag-picker-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		margin-bottom: 0.4rem;
		font-size: 0.8rem;
		font-weight: 600;
	}

	.tag-picker-header input[type='search'] {
		flex: 1;
		max-width: 260px;
		padding: 0.3rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	.tag-list {
		max-height: 260px;
		overflow-y: auto;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		padding: 0.5rem;
	}

	.tag-group {
		margin-bottom: 0.5rem;
	}

	.tag-group h4 {
		margin: 0 0 0.25rem;
		font-size: 0.78rem;
		color: var(--banto-text-muted);
	}

	.tag-row {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.15rem 0;
		font-size: 0.8rem;
	}

	.tag-row input {
		width: auto;
	}

	.tag-address {
		color: var(--banto-text-muted);
		font-size: 0.72rem;
	}

	.actions {
		display: flex;
		gap: 0.75rem;
		margin-top: 1rem;
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
		font-size: 0.78rem;
	}

	button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
	}
</style>
