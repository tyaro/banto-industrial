<script lang="ts">
	/**
	 * QRコード画面（デバッグ支援）。タッチパネル（HMI）のQRリーダーに読ませる
	 * 文字列をリストで管理し、そのQRコードを画面に並べて表示する。運用の
	 * 想定はデバッグ作業: 画面に表示したQRをタッチパネルのリーダーで直接
	 * スキャンする。viewer は閲覧（スキャン）のみ、editor 以上が作成/編集/
	 * 削除/並び替えできる（backend も同じ権限で二経路対称 — REST/Tauri）。
	 *
	 * QRのSVGはサーバー側（Rust `qrcode` クレート）で text から機械生成された
	 * ものだけが返る。ユーザー入力のマークアップは一切通らないため、下の
	 * {@html} 描画は docs/conventions.md §セキュリティの「自前生成・完全
	 * エスケープ/機械生成の出力のみ {@html} 可」を満たす（設定画面の
	 * LANアクセスQRと同じ扱い）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';
	import {
		listQrStrings,
		createQrString,
		updateQrString,
		deleteQrString,
		reorderQrStrings,
		isQrStringsAvailable,
		DEMO_MODE_MESSAGE,
		type QrString,
		type QrStringInput
	} from '$lib/banto/qrAdmin';

	const available = isQrStringsAvailable();
	const canWrite = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let items: QrString[] = $state([]);
	let loading = $state(false);

	async function reload(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			items = await listQrStrings();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void reload();
	});

	// --- QR表示（サイズ切替・スキャン用表示） ---
	const sizeOptions = [
		{ value: 128, label: '小' },
		{ value: 192, label: '中' },
		{ value: 256, label: '大' }
	] as const;
	let tileSize = $state(192);
	/** スキャン用表示: 管理セクションを隠してQRタイルだけを大きく見せる。 */
	let scanOnly = $state(false);

	function displayName(item: QrString): string {
		return item.label !== '' ? item.label : item.text;
	}

	// --- create ---
	let createForm = $state({ label: '', text: '' });
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

	function toInput(form: { label: string; text: string }): QrStringInput {
		return { label: form.label, text: form.text };
	}

	async function handleCreate(): Promise<void> {
		creating = true;
		createErrors = {};
		try {
			await createQrString(toInput(createForm));
			toastStore.push('success', '追加しました');
			createForm = { label: '', text: '' };
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
	let selected: QrString | null = $state(null);
	let editForm = $state({ label: '', text: '' });
	let editErrors: Record<string, string> = $state({});
	let saving = $state(false);

	function selectItem(item: QrString): void {
		selected = item;
		editForm = { label: item.label, text: item.text };
		editErrors = {};
	}

	async function saveEdit(): Promise<void> {
		if (!selected) return;
		saving = true;
		editErrors = {};
		try {
			const updated = await updateQrString(selected.id, toInput(editForm));
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

	async function handleDelete(item: QrString): Promise<void> {
		if (!window.confirm(`「${displayName(item)}」を削除しますか？`)) return;
		try {
			await deleteQrString(item.id);
			toastStore.push('success', '削除しました');
			if (selected?.id === item.id) selected = null;
			await reload();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	// --- reorder（↑/↓: ローカルで入れ替えた全件の id 配列を送る） ---
	let reordering = $state(false);

	async function move(index: number, delta: -1 | 1): Promise<void> {
		const target = index + delta;
		if (target < 0 || target >= items.length || reordering) return;
		const ids = items.map((item) => item.id);
		[ids[index], ids[target]] = [ids[target], ids[index]];
		reordering = true;
		try {
			items = await reorderQrStrings(ids);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			await reload();
		} finally {
			reordering = false;
		}
	}
</script>

<div class="page">
	<h2>QRコード</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードにはDBがないため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		<p class="note">
			タッチパネルのQRリーダーで読み取るデバッグ用のQRコードを表示します。
			登録した文字列が上から順に（並び順どおりに）タイルで並びます。
		</p>

		{#if !scanOnly}
			{#if canWrite}
				<section class="create">
					<h3>QR文字列を追加</h3>
					<div class="form-grid">
						<label class="field">
							ラベル（任意）
							<input type="text" bind:value={createForm.label} placeholder="開始コマンド" />
							{#if createErrors.label}<span class="err">{createErrors.label}</span>{/if}
						</label>
						<label class="field wide">
							QR文字列（必須・1000文字以内）
							<input type="text" bind:value={createForm.text} placeholder="START" />
							{#if createErrors.text}<span class="err">{createErrors.text}</span>{/if}
						</label>
					</div>
					<button type="button" onclick={handleCreate} disabled={creating}>追加</button>
				</section>
			{/if}

			<section class="list">
				<h3>登録済みリスト</h3>
				<p class="note">
					{canWrite
						? '↑/↓で表示順を入れ替えられます。「編集」で下に編集パネルが開きます。'
						: '閲覧のみ（編集には編集者以上の権限が必要です）。'}
				</p>
				{#if loading && items.length === 0}
					<p class="loading">読み込み中…</p>
				{:else if items.length === 0}
					<p class="loading">まだ登録がありません。</p>
				{:else}
					<div class="table-wrap">
						<table>
							<thead>
								<tr>
									<th class="num">並び順</th>
									<th>ラベル</th>
									<th>QR文字列</th>
									{#if canWrite}<th class="ops">操作</th>{/if}
								</tr>
							</thead>
							<tbody>
								{#each items as item, index (item.id)}
									<tr class:selected={selected?.id === item.id}>
										<td class="num">{index + 1}</td>
										<td>{item.label}</td>
										<td class="mono">{item.text}</td>
										{#if canWrite}
											<td class="ops">
												<button
													type="button"
													class="small"
													onclick={() => move(index, -1)}
													disabled={index === 0 || reordering}
													aria-label="上へ"
												>
													↑
												</button>
												<button
													type="button"
													class="small"
													onclick={() => move(index, 1)}
													disabled={index === items.length - 1 || reordering}
													aria-label="下へ"
												>
													↓
												</button>
												<button type="button" class="small" onclick={() => selectItem(item)}>
													編集
												</button>
												<button
													type="button"
													class="small danger"
													onclick={() => handleDelete(item)}
												>
													削除
												</button>
											</td>
										{/if}
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</section>

			{#if selected && canWrite}
				<section class="detail">
					<h3>「{displayName(selected)}」を編集</h3>
					<div class="form-grid">
						<label class="field">
							ラベル（任意）
							<input type="text" bind:value={editForm.label} />
							{#if editErrors.label}<span class="err">{editErrors.label}</span>{/if}
						</label>
						<label class="field wide">
							QR文字列（必須・1000文字以内）
							<input type="text" bind:value={editForm.text} />
							{#if editErrors.text}<span class="err">{editErrors.text}</span>{/if}
						</label>
					</div>
					<div class="actions">
						<button type="button" onclick={saveEdit} disabled={saving}>保存</button>
						<button type="button" class="ghost" onclick={() => (selected = null)}>閉じる</button>
					</div>
				</section>
			{/if}
		{/if}

		<section class="qr-display">
			<div class="qr-toolbar">
				<h3>QR表示</h3>
				<div class="controls">
					<span class="controls-label">サイズ:</span>
					{#each sizeOptions as option (option.value)}
						<button
							type="button"
							class="small"
							class:active={tileSize === option.value}
							onclick={() => (tileSize = option.value)}
						>
							{option.label}
						</button>
					{/each}
					<label class="scan-toggle">
						<input type="checkbox" bind:checked={scanOnly} />
						QR表示のみ（スキャン用）
					</label>
				</div>
			</div>
			{#if items.length === 0}
				<p class="loading">表示するQRコードがありません。上のフォームから追加してください。</p>
			{:else}
				<div class="qr-grid" style:--qr-tile-size={`${tileSize}px`}>
					{#each items as item (item.id)}
						<figure class="qr-tile">
							{#if item.svg !== ''}
								<!-- サーバー生成のQR SVG（Rust `qrcode` クレートによる機械生成のみ、
								     ユーザー入力のマークアップは通らない — docs/conventions.md
								     §セキュリティ。設定画面のLANアクセスQRと同じ扱い）。 -->
								<!-- eslint-disable-next-line svelte/no-at-html-tags -->
								<div class="qr-svg">{@html item.svg}</div>
							{:else}
								<div class="qr-svg qr-missing">QR生成不可</div>
							{/if}
							<figcaption>
								<span class="qr-label">{displayName(item)}</span>
								{#if item.label !== ''}
									<span class="qr-text mono">{item.text}</span>
								{/if}
							</figcaption>
						</figure>
					{/each}
				</div>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 1080px;
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
		font-size: 0.85rem;
	}

	.form-grid {
		display: grid;
		grid-template-columns: minmax(150px, 220px) 1fr;
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

	.field input {
		padding: 0.4rem 0.5rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: var(--banto-bg);
		color: var(--banto-text);
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
		padding: 0.25rem 0.55rem;
		font-size: 0.75rem;
		font-weight: 500;
		background: var(--banto-bg);
		color: var(--banto-text);
		border: 1px solid var(--banto-border);
	}

	button.small:hover:not(:disabled) {
		background: var(--banto-surface);
		border-color: var(--banto-primary);
	}

	button.small.active {
		background: var(--banto-primary);
		border-color: var(--banto-primary);
		color: var(--banto-text-inverse);
	}

	button.small.danger {
		color: var(--banto-danger);
		border-color: var(--banto-danger);
	}

	button.small.danger:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	button.ghost {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
		font-weight: 500;
	}

	button.ghost:hover:not(:disabled) {
		background: var(--banto-bg);
	}

	.table-wrap {
		overflow-x: auto;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th,
	td {
		padding: 0.45rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
		text-align: left;
		vertical-align: middle;
	}

	th {
		color: var(--banto-text-muted);
		font-weight: 600;
		font-size: 0.75rem;
	}

	tr.selected td {
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.num {
		width: 4rem;
		text-align: right;
	}

	.ops {
		width: 15rem;
		white-space: nowrap;
	}

	.ops button + button {
		margin-left: 0.3rem;
	}

	.mono {
		font-family: var(--banto-font-mono, ui-monospace, monospace);
		word-break: break-all;
	}

	/* --- QR表示 --- */

	.qr-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: 0.5rem;
		margin-bottom: 0.75rem;
	}

	.qr-toolbar h3 {
		margin: 0;
	}

	.controls {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.controls-label {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.scan-toggle {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		margin-left: 0.75rem;
		font-size: 0.8rem;
		color: var(--banto-text-muted);
		cursor: pointer;
	}

	.qr-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(calc(var(--qr-tile-size) + 2rem), 1fr));
		gap: 1rem;
	}

	.qr-tile {
		margin: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
	}

	.qr-svg {
		/* 固定の白背景（--banto-* サーフェス変数にしない）: QRコードは
		   ダークモードでも黒白のままでないとリーダーが読めない — 設定画面の
		   .qr と同じ理由。 */
		background: #fff;
		padding: 0.5rem;
		border-radius: var(--banto-radius);
		border: 1px solid var(--banto-border);
		width: var(--qr-tile-size);
		height: var(--qr-tile-size);
		display: flex;
		align-items: center;
		justify-content: center;
	}

	/* qrcode クレートのSVGは固有の width/height 属性を持つため、タイルの
	   サイズ切替（小/中/大）に追従するようここで上書きする。 */
	.qr-svg :global(svg) {
		width: 100%;
		height: 100%;
	}

	.qr-missing {
		color: var(--banto-danger);
		font-size: 0.75rem;
		text-align: center;
	}

	figcaption {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.15rem;
		max-width: calc(var(--qr-tile-size) + 2rem);
		text-align: center;
	}

	.qr-label {
		font-size: 0.85rem;
		font-weight: 600;
	}

	.qr-text {
		font-size: 0.7rem;
		color: var(--banto-text-muted);
	}
</style>
