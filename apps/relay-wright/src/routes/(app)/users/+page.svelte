<script lang="ts">
	/**
	 * ユーザー管理画面（spec M10 RBAC）。`admin` のみ到達（+page.ts が非adminを
	 * ダッシュボードへリダイレクト）。デモモード（プレーンな vite dev/preview、
	 * バックエンドなし）ではアカウントDBそのものが存在しないため、案内文のみ表示
	 * する（isUsersAdminAvailable()）。
	 *
	 * 一覧はBantoGrid（クライアントモード、全件を一度に取得）。BantoGridの
	 * セル編集は「1カラム=1値」のインライン編集向けで、行ごとの複数ボタン（削除
	 * ＋パスワードリセット）を埋め込む手段がないため、行クリックで選択→下に
	 * 詳細/編集/リセット/削除パネルを表示する構成にしている（items.[id]ページの
	 * 「クリックで編集ページへ」と同じ発想を、別ページ遷移ではなくページ内表示
	 * にしたもの）。
	 */
	import { BantoGrid, type GridColumn } from '@banto/grid-svelte';
	import { BantoForm, createFormStore } from '@banto/forms';
	import type { FormSchema } from '@banto/forms';
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import {
		createUser,
		deleteUser,
		isUsersAdminAvailable,
		listUsers,
		resetUserPassword,
		updateUser,
		DEMO_MODE_MESSAGE,
		type Role as UserRole,
		type UserSummary
	} from '$lib/banto/usersAdmin';

	const roleOptions: { value: UserRole; label: string }[] = [
		{ value: 'admin', label: '管理者' },
		{ value: 'editor', label: '編集者' },
		{ value: 'viewer', label: '閲覧者' }
	];

	function roleLabel(role: UserRole): string {
		return roleOptions.find((option) => option.value === role)?.label ?? role;
	}

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const available = isUsersAdminAvailable();

	const createSchema: FormSchema = {
		fields: [
			{ name: 'username', label: 'ユーザー名', type: 'text', required: true, min: 1, max: 32 },
			{
				name: 'password',
				label: 'パスワード（8文字以上）',
				type: 'password',
				required: true,
				min: 8
			},
			{ name: 'displayName', label: '表示名', type: 'text', required: true, min: 1 },
			{
				name: 'role',
				label: 'ロール',
				type: 'select',
				required: true,
				default: 'viewer',
				options: roleOptions
			}
		]
	};

	let createStore = $state(createFormStore(createSchema));
	let creating = $state(false);

	let users: UserSummary[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			users = await listUsers();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	async function handleCreate(values: Record<string, unknown>): Promise<void> {
		creating = true;
		try {
			await createUser({
				username: String(values.username),
				password: String(values.password),
				displayName: String(values.displayName),
				role: values.role as UserRole
			});
			toastStore.push('success', '作成しました');
			createStore = createFormStore(createSchema);
			await reload();
		} catch (err) {
			if (isProviderError(err) && err.body.kind === 'validation') {
				createStore.setServerErrors(err.body.field_errors);
			} else {
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			creating = false;
		}
	}

	const columns: GridColumn<UserSummary>[] = [
		{ id: 'id', header: 'ID', accessor: 'id', width: 70, align: 'right' },
		{
			id: 'username',
			header: 'ユーザー名',
			accessor: 'username',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'displayName',
			header: '表示名',
			accessor: 'displayName',
			width: 160,
			filterable: true,
			filterType: 'text'
		},
		{
			id: 'role',
			header: 'ロール',
			accessor: 'role',
			width: 110,
			format: (value) => roleLabel(value as UserRole)
		},
		{ id: 'createdAt', header: '作成日時', accessor: 'createdAt', width: 180 }
	];

	let selected: UserSummary | null = $state(null);
	let editDisplayName = $state('');
	let editRole: UserRole = $state('viewer');
	let saving = $state(false);

	function selectUser(user: UserSummary): void {
		selected = user;
		editDisplayName = user.displayName;
		editRole = user.role;
		resetPassword = '';
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		try {
			const updated = await updateUser(selected.id, {
				displayName: editDisplayName,
				role: editRole
			});
			toastStore.push('success', '更新しました');
			selected = updated;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			saving = false;
		}
	}

	let resetPassword = $state('');
	let resetting = $state(false);

	async function saveReset(): Promise<void> {
		if (!selected) return;
		if (resetPassword.length < 8) {
			toastStore.push('error', 'パスワードは8文字以上で入力してください');
			return;
		}
		resetting = true;
		try {
			await resetUserPassword(selected.id, resetPassword);
			toastStore.push('success', 'パスワードをリセットしました');
			resetPassword = '';
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			resetting = false;
		}
	}

	async function handleDelete(): Promise<void> {
		if (!selected) return;
		if (!window.confirm(`${selected.username} を削除しますか？`)) return;
		try {
			await deleteUser(selected.id);
			toastStore.push('success', '削除しました');
			selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}
</script>

<div class="page">
	<h2>ユーザー管理</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはアカウントDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<section class="create">
			<h3>新規作成</h3>
			<BantoForm
				schema={createSchema}
				store={createStore}
				onSubmit={handleCreate}
				submitting={creating}
				submitLabel="作成"
			/>
		</section>

		<section class="list">
			<h3>アカウント一覧</h3>
			<p class="note">行をクリックすると下に編集パネルが表示されます。</p>
			{#if loading && users.length === 0}
				<p class="loading">読み込み中…</p>
			{:else}
				<div class="grid-wrap">
					<BantoGrid rows={users} {columns} getRowId={(user) => user.id} onRowClick={selectUser} />
				</div>
			{/if}
		</section>

		{#if selected}
			<section class="detail">
				<h3>{selected.username} を編集</h3>
				<label class="field">
					表示名
					<input type="text" bind:value={editDisplayName} />
				</label>
				<label class="field">
					ロール
					<select bind:value={editRole}>
						{#each roleOptions as option (option.value)}
							<option value={option.value}>{option.label}</option>
						{/each}
					</select>
				</label>
				<div class="actions">
					<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
					<button type="button" class="danger" onclick={handleDelete}>削除</button>
				</div>

				<div class="reset">
					<label class="field">
						新しいパスワード（8文字以上）
						<input type="password" bind:value={resetPassword} autocomplete="new-password" />
					</label>
					<button type="button" onclick={saveReset} disabled={resetting}
						>パスワードをリセット</button
					>
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
		max-width: 720px;
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

	.field {
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		margin-bottom: 0.75rem;
	}

	.field input,
	.field select {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
	}

	.actions {
		display: flex;
		gap: 0.75rem;
		margin-bottom: 1rem;
	}

	.reset {
		border-top: 1px solid var(--banto-border);
		padding-top: 0.75rem;
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
