<script lang="ts">
	/**
	 * プロジェクト画面（設定の保存・読み込み, feature/project-file）。
	 *
	 * アプリの設定レジストリ全体（PLC接続 / 収集グループ / タグ / 書き込み先 /
	 * 書き込みルール / QR文字列）を単一のバージョン付きJSONプロジェクトファイル
	 * として保存（エクスポート）し、別の環境や別の時点の設定を読み込む
	 * （インポート）。ユーザー / UI設定 / 監査ログ / 書き込み履歴 / アーム状態は
	 * 「設定」ではないため対象外。
	 *
	 * エクスポートは editor 以上（設定内容に host/port は含むが秘密情報は含まない
	 * ため閲覧＝読み取り相当）。インポートは admin のみ・エンジンがアーム中は
	 * 不可・現在の全設定を置き換える破壊的操作なので、確認ダイアログで明示し、
	 * バックエンドも同じ権限/ガードで二経路対称に強制する（REST/Tauri）。
	 * インポート後はルールが反映されるようエンジンの再読込が必要
	 * （Tauri は自動で再読込するが、UI からも /engine で明示的に再読込できる）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources, isAdmin } from '$lib/permissions';
	import {
		exportProject,
		importProject,
		isProjectAvailable,
		PROJECT_FORMAT,
		PROJECT_VERSION,
		DEMO_MODE_MESSAGE,
		type ProjectFile,
		type ImportSummary
	} from '$lib/banto/projectAdmin';

	const available = isProjectAvailable();
	const canExport = $derived(canWriteResources(sessionStore.role));
	const canImport = $derived(isAdmin(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	// --- エクスポート ---------------------------------------------------------

	let exporting = $state(false);

	/** `exportedAt`（"YYYY-MM-DD HH:MM:SS"）から YYYYMMDD を取り出す。無ければ日付なし。 */
	function fileDateSuffix(exportedAt: string | null | undefined): string {
		if (typeof exportedAt === 'string' && exportedAt.length >= 10) {
			return '_' + exportedAt.slice(0, 10).replace(/-/g, '');
		}
		return '';
	}

	function triggerDownload(project: ProjectFile): void {
		const json = JSON.stringify(project, null, 2);
		const blob = new Blob([json], { type: 'application/json' });
		const url = URL.createObjectURL(blob);
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = `relay-wright-project${fileDateSuffix(project.exportedAt)}.json`;
		document.body.appendChild(anchor);
		anchor.click();
		anchor.remove();
		URL.revokeObjectURL(url);
	}

	async function handleExport(): Promise<void> {
		exporting = true;
		try {
			const project = await exportProject();
			triggerDownload(project);
			toastStore.push('success', 'プロジェクトファイルをエクスポートしました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			exporting = false;
		}
	}

	// --- インポート -----------------------------------------------------------

	let fileInput: HTMLInputElement | null = $state(null);
	let selectedName = $state('');
	let parsed: ProjectFile | null = $state(null);
	let parseError = $state('');
	let importing = $state(false);
	let lastSummary: ImportSummary | null = $state(null);

	interface CountRow {
		label: string;
		count: number;
	}

	function counts(project: ProjectFile): CountRow[] {
		return [
			{ label: 'PLC接続', count: project.plcConnections?.length ?? 0 },
			{ label: '収集グループ', count: project.collectionGroups?.length ?? 0 },
			{ label: 'タグ', count: project.tags?.length ?? 0 },
			{ label: '書き込み先', count: project.writeTargets?.length ?? 0 },
			{ label: '書き込みルール', count: project.writeRules?.length ?? 0 },
			{ label: 'QR文字列', count: project.qrStrings?.length ?? 0 }
		];
	}

	function summaryRows(summary: ImportSummary): CountRow[] {
		return [
			{ label: 'PLC接続', count: summary.plcConnections },
			{ label: '収集グループ', count: summary.collectionGroups },
			{ label: 'タグ', count: summary.tags },
			{ label: '書き込み先', count: summary.writeTargets },
			{ label: '書き込みルール', count: summary.writeRules },
			{ label: '書き込み条件', count: summary.writeRuleConditions },
			{ label: 'QR文字列', count: summary.qrStrings }
		];
	}

	/** 最低限の形状チェック（詳細な検証はバックエンドが行う）。 */
	function looksLikeProject(value: unknown): value is ProjectFile {
		if (typeof value !== 'object' || value === null) return false;
		const v = value as Record<string, unknown>;
		return v.format === PROJECT_FORMAT && Array.isArray(v.plcConnections);
	}

	async function handleFileChange(event: Event): Promise<void> {
		parsed = null;
		parseError = '';
		lastSummary = null;
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) {
			selectedName = '';
			return;
		}
		selectedName = file.name;
		try {
			const text = await file.text();
			const value = JSON.parse(text) as unknown;
			if (!looksLikeProject(value)) {
				parseError = `このファイルは relay-wright のプロジェクトファイルではないようです（format が "${PROJECT_FORMAT}" ではありません）。`;
				return;
			}
			if (value.version !== PROJECT_VERSION) {
				parseError = `このバージョンのプロジェクトファイル（version ${value.version}）は読み込めません（対応 version ${PROJECT_VERSION}）。`;
				return;
			}
			parsed = value;
		} catch {
			parseError = 'JSON として読み取れませんでした。ファイルが壊れていないか確認してください。';
		}
	}

	function resetImport(): void {
		parsed = null;
		parseError = '';
		selectedName = '';
		if (fileInput) fileInput.value = '';
	}

	async function handleImport(): Promise<void> {
		if (!parsed) return;
		const confirmed = window.confirm(
			'現在の全設定（接続・タグ・書き込み先・ルール・QR）が置き換えられます。' +
				'この操作は元に戻せません。エンジンがアーム中はインポートできません。\n\n続行しますか？'
		);
		if (!confirmed) return;

		importing = true;
		try {
			const summary = await importProject(parsed);
			lastSummary = summary;
			resetImport();
			toastStore.push('success', 'プロジェクトをインポートしました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			importing = false;
		}
	}
</script>

<section class="page">
	<header class="page-head">
		<h1>プロジェクト</h1>
		<p class="lead">
			設定レジストリ全体（PLC接続・収集グループ・タグ・書き込み先・書き込みルール・QR文字列）を
			1つのプロジェクトファイルとして保存・読み込みします。ユーザーやUI設定・監査ログ・書き込み履歴は含まれません。
		</p>
	</header>

	{#if !available}
		<p class="notice">{DEMO_MODE_MESSAGE}</p>
	{:else}
		<!-- エクスポート -->
		<div class="card">
			<h2>エクスポート</h2>
			<p>現在の全設定をJSONプロジェクトファイルとしてダウンロードします。</p>
			{#if canExport}
				<button class="primary" onclick={handleExport} disabled={exporting}>
					{exporting ? 'エクスポート中…' : 'エクスポート'}
				</button>
			{:else}
				<p class="notice">エクスポートには編集者以上の権限が必要です。</p>
			{/if}
		</div>

		<!-- インポート -->
		<div class="card">
			<h2>インポート</h2>
			{#if canImport}
				<p class="warn">
					⚠ インポートは<strong
						>現在の全設定（接続・タグ・書き込み先・ルール・QR）を置き換えます</strong
					>。 元に戻せません。エンジンが<strong>アーム中はインポートできません</strong>。
				</p>
				<label class="file-field">
					<span>プロジェクトファイルを選択</span>
					<input
						bind:this={fileInput}
						type="file"
						accept=".json,application/json"
						onchange={handleFileChange}
					/>
				</label>

				{#if selectedName}
					<p class="selected">選択中: {selectedName}</p>
				{/if}

				{#if parseError}
					<p class="notice error">{parseError}</p>
				{/if}

				{#if parsed}
					<div class="preview">
						<h3>読み込む内容</h3>
						<ul class="counts">
							{#each counts(parsed) as row (row.label)}
								<li><span class="count">{row.count}</span> {row.label}</li>
							{/each}
						</ul>
						<div class="actions">
							<button class="danger" onclick={handleImport} disabled={importing}>
								{importing ? 'インポート中…' : 'この内容で全設定を置き換える'}
							</button>
							<button class="ghost" onclick={resetImport} disabled={importing}>キャンセル</button>
						</div>
					</div>
				{/if}

				{#if lastSummary}
					<div class="result">
						<h3>インポートが完了しました</h3>
						<ul class="counts">
							{#each summaryRows(lastSummary) as row (row.label)}
								<li><span class="count">{row.count}</span> {row.label}</li>
							{/each}
						</ul>
						<p class="warn">
							インポートしたルールを有効にするには、エンジンの<strong>再読込</strong>が必要です。
							<a href="/engine">エンジン制御・監視</a
							>から再読込してください（デスクトップ版は自動で再読込を試みます）。
						</p>
					</div>
				{/if}
			{:else}
				<p class="notice">インポートには管理者権限が必要です。</p>
			{/if}
		</div>
	{/if}
</section>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
		max-width: 48rem;
	}
	.page-head h1 {
		margin: 0 0 0.25rem;
	}
	.lead {
		margin: 0;
		color: var(--banto-text-muted, #64748b);
	}
	.card {
		border: 1px solid var(--banto-border, #e2e8f0);
		border-radius: 0.5rem;
		padding: 1.25rem;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		background: var(--banto-surface, transparent);
	}
	.card h2 {
		margin: 0;
		font-size: 1.1rem;
	}
	.card p {
		margin: 0;
	}
	.warn {
		color: var(--banto-warning-text, #b45309);
	}
	.notice {
		color: var(--banto-text-muted, #64748b);
	}
	.notice.error {
		color: var(--banto-danger-text, #b91c1c);
	}
	.file-field {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.selected {
		font-size: 0.9rem;
		color: var(--banto-text-muted, #64748b);
	}
	.preview,
	.result {
		border-top: 1px solid var(--banto-border, #e2e8f0);
		padding-top: 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}
	.preview h3,
	.result h3 {
		margin: 0;
		font-size: 1rem;
	}
	.counts {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem 1rem;
	}
	.counts li {
		display: inline-flex;
		align-items: baseline;
		gap: 0.35rem;
	}
	.count {
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}
	.actions {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
	}
	button {
		cursor: pointer;
		border-radius: 0.375rem;
		border: 1px solid var(--banto-border, #cbd5e1);
		padding: 0.5rem 1rem;
		font: inherit;
		background: var(--banto-surface, #fff);
	}
	button:disabled {
		opacity: 0.6;
		cursor: default;
	}
	button.primary {
		background: var(--banto-accent, #2563eb);
		color: #fff;
		border-color: transparent;
		align-self: flex-start;
	}
	button.danger {
		background: var(--banto-danger, #dc2626);
		color: #fff;
		border-color: transparent;
	}
	button.ghost {
		background: transparent;
	}
</style>
