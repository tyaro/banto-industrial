<script lang="ts">
	/**
	 * データカテゴリ（監査ログの保持ポリシー/バックアップ・リストア）
	 * （#359 chronogazer 分）。元 `+page.svelte` の「監査ログの保持
	 * ポリシー」「バックアップ/リストア」セクションから markup・state・
	 * 関数を無改変で移した。いずれも admin 限定 かつ 各機能の可用性
	 * （`auditAvailable`/`backupsAvailable`）ガード付き。
	 */
	import { isTauri } from '$lib/banto/setup';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { toastStore } from '$lib/toast.svelte';
	import {
		getAuditConfig,
		isAuditLogAvailable,
		setAuditConfig,
		type AuditSettings
	} from '$lib/banto/auditLogAdmin';
	import {
		cancelPendingRestore,
		createBackup,
		downloadBackup,
		getPendingRestore,
		isBackupsAvailable,
		listBackups,
		openBackupsFolder,
		stageRestoreFromBackup,
		uploadAndStageRestore,
		type BackupInfo,
		type PendingRestoreInfo
	} from '$lib/banto/backupsAdmin';
	import { errorMessage } from './shared';

	const tauri = isTauri();

	// --- M14: audit-log retention policy (Tauri + LAN browser) --------------
	// Unlike server/auth-mode settings, this section is not Tauri-only:
	// `auditLogAdmin.ts` has a REST fallback (`GET`/`PUT /api/audit-log/config`,
	// spec M14 Phase B) so a LAN browser admin can also see/change the
	// retention policy, not just the desktop app - so this section is gated
	// on `auditAvailable` (real backend, not the plain-browser demo) rather
	// than `tauri`.
	const auditAvailable = isAuditLogAvailable();

	let auditConfig = $state<AuditSettings | null>(null);
	// 0 is the wire sentinel for "unlimited" on both fields (spec M14,
	// `SettingsService::set_audit_config`/`normalize_retention`) - shown to
	// the admin as a plain 0 with an explanatory note below, rather than a
	// separate checkbox, mirroring the Rust-side convention exactly.
	let retentionDaysDraft = $state(90);
	let retentionRowsDraft = $state(100_000);
	let applyingAudit = $state(false);
	let auditError: string | null = $state(null);

	function applyAuditConfigToDrafts(config: AuditSettings): void {
		auditConfig = config;
		retentionDaysDraft = config.retentionDays ?? 0;
		retentionRowsDraft = config.retentionRows ?? 0;
	}

	$effect(() => {
		if (!auditAvailable || !isAdmin(sessionStore.role)) return;
		void (async () => {
			try {
				applyAuditConfigToDrafts(await getAuditConfig());
			} catch (err) {
				auditError = errorMessage(err);
			}
		})();
	});

	async function saveAuditConfig(): Promise<void> {
		applyingAudit = true;
		auditError = null;
		try {
			applyAuditConfigToDrafts(
				await setAuditConfig({
					retentionDays: retentionDaysDraft > 0 ? retentionDaysDraft : null,
					retentionRows: retentionRowsDraft > 0 ? retentionRowsDraft : null
				})
			);
			toastStore.push('success', '監査ログの保持ポリシーを更新しました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			applyingAudit = false;
		}
	}

	// --- M17: SQLite backup/restore (Tauri + LAN browser, admin only) -------
	// Same availability gate as the audit-log section above (real backend,
	// not the plain-browser demo) - `backupsAdmin.ts`'s REST fallback means a
	// LAN browser admin gets this section too, not just the desktop app.
	const backupsAvailable = isBackupsAvailable();

	let backups = $state<BackupInfo[]>([]);
	let pendingRestore = $state<PendingRestoreInfo | null>(null);
	let loadingBackups = $state(false);
	let creatingBackup = $state(false);
	let stagingRestore = $state(false);
	let cancellingRestore = $state(false);
	let backupsError: string | null = $state(null);
	let restoreFileInput: HTMLInputElement | undefined = $state();

	function formatBytes(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let value = bytes;
		let unitIndex = -1;
		do {
			value /= 1024;
			unitIndex++;
		} while (value >= 1024 && unitIndex < units.length - 1);
		return `${value.toFixed(1)} ${units[unitIndex]}`;
	}

	async function reloadBackups(): Promise<void> {
		backups = await listBackups();
	}

	async function reloadPendingRestore(): Promise<void> {
		pendingRestore = await getPendingRestore();
	}

	$effect(() => {
		if (!backupsAvailable || !isAdmin(sessionStore.role)) return;
		void (async () => {
			loadingBackups = true;
			backupsError = null;
			try {
				await Promise.all([reloadBackups(), reloadPendingRestore()]);
			} catch (err) {
				backupsError = errorMessage(err);
			} finally {
				loadingBackups = false;
			}
		})();
	});

	async function handleCreateBackup(): Promise<void> {
		creatingBackup = true;
		backupsError = null;
		try {
			await createBackup();
			toastStore.push('success', 'バックアップを作成しました');
			await reloadBackups();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			creatingBackup = false;
		}
	}

	async function handleDownloadBackup(fileName: string): Promise<void> {
		try {
			await downloadBackup(fileName);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	async function handleOpenBackupsFolder(): Promise<void> {
		try {
			const result = await openBackupsFolder();
			if (!result.opened) {
				toastStore.push('info', `このOSでは非対応です。手動で開いてください: ${result.path}`);
			}
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		}
	}

	// Confirmation copy is fixed per spec M17 ("現在のデータは適用時に自動
	// バックアップされます。適用には再起動が必要です" must be explicit) -
	// only the leading line describing the source (existing file vs upload)
	// varies between the two callers below.
	function confirmRestore(sourceDescription: string): boolean {
		return window.confirm(
			`${sourceDescription}\n\n現在のデータは適用時に自動バックアップされます。適用には再起動が必要です。よろしいですか？`
		);
	}

	async function handleRestoreFromExisting(fileName: string): Promise<void> {
		if (!confirmRestore(`このバックアップからリストアします: ${fileName}`)) return;
		stagingRestore = true;
		try {
			await stageRestoreFromBackup(fileName);
			toastStore.push('success', 'リストアをステージしました（再起動後に適用されます）');
			await reloadPendingRestore();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			stagingRestore = false;
		}
	}

	function handleRestoreFileButtonClick(): void {
		restoreFileInput?.click();
	}

	async function handleRestoreFileChange(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = ''; // allow re-selecting the same file (e.g. after fixing it) later
		if (!file) return;
		if (!confirmRestore(`アップロードしたファイルからリストアします: ${file.name}`)) return;

		stagingRestore = true;
		try {
			await uploadAndStageRestore(file);
			toastStore.push('success', 'リストアをステージしました（再起動後に適用されます）');
			await reloadPendingRestore();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			stagingRestore = false;
		}
	}

	async function handleCancelRestore(): Promise<void> {
		cancellingRestore = true;
		try {
			await cancelPendingRestore();
			toastStore.push('success', 'リストアの予約を取り消しました');
			pendingRestore = null;
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			cancellingRestore = false;
		}
	}
</script>

{#if auditAvailable && isAdmin(sessionStore.role)}
	<section>
		<h2>監査ログの保持ポリシー</h2>

		<div class="server-fields">
			<label class="field">
				保持日数
				<input type="number" min="0" bind:value={retentionDaysDraft} />
			</label>
			<label class="field">
				上限行数
				<input type="number" min="0" bind:value={retentionRowsDraft} />
			</label>
		</div>

		<button type="button" onclick={saveAuditConfig} disabled={applyingAudit}>保存</button>

		{#if auditError}
			<p class="error">{auditError}</p>
		{/if}

		{#if auditConfig}
			<p class="status">
				現在の設定:
				<strong>
					{auditConfig.retentionDays !== null ? `${auditConfig.retentionDays}日` : '無期限'}
					/ {auditConfig.retentionRows !== null
						? `${auditConfig.retentionRows.toLocaleString()}件`
						: '無制限'}
				</strong>
			</p>
		{/if}

		<p class="note">
			0を入力すると、その項目は無制限になります（既定は90日 /
			10万件）。古い記録は一覧の表示時に自動的に整理されます。記録の一覧は「監査ログ」画面から確認できます。
		</p>
	</section>
{/if}

{#if backupsAvailable && isAdmin(sessionStore.role)}
	<section>
		<h2>バックアップ/リストア</h2>

		<div class="backup-toolbar">
			<button type="button" onclick={handleCreateBackup} disabled={creatingBackup}>
				{creatingBackup ? '作成中…' : '今すぐバックアップ'}
			</button>
			{#if tauri}
				<button type="button" class="secondary" onclick={handleOpenBackupsFolder}
					>フォルダを開く</button
				>
			{/if}
		</div>

		{#if backupsError}
			<p class="error">{backupsError}</p>
		{/if}

		{#if pendingRestore}
			<p class="pending-restore">
				再起動後に適用されます: <strong>{pendingRestore.stagedAt}</strong>（{formatBytes(
					pendingRestore.sizeBytes
				)}）
				<button
					type="button"
					class="secondary"
					onclick={handleCancelRestore}
					disabled={cancellingRestore}
				>
					取消
				</button>
			</p>
		{/if}

		{#if loadingBackups}
			<p class="note">読み込み中…</p>
		{:else if backups.length === 0}
			<p class="note">バックアップはまだありません。</p>
		{:else}
			<ul class="backup-list">
				{#each backups as backup (backup.fileName)}
					<li>
						<div class="backup-info">
							<span class="file-name">{backup.fileName}</span>
							<span class="meta">{formatBytes(backup.sizeBytes)} ・ {backup.createdAt}</span>
						</div>
						<div class="backup-actions">
							{#if !tauri}
								<button
									type="button"
									class="secondary"
									onclick={() => handleDownloadBackup(backup.fileName)}
								>
									ダウンロード
								</button>
							{/if}
							<button
								type="button"
								class="secondary"
								onclick={() => handleRestoreFromExisting(backup.fileName)}
								disabled={stagingRestore}
							>
								このバックアップからリストア
							</button>
						</div>
					</li>
				{/each}
			</ul>
		{/if}

		{#if !tauri}
			<div class="restore-upload">
				<button type="button" onclick={handleRestoreFileButtonClick} disabled={stagingRestore}>
					ファイルからリストア
				</button>
				<input
					class="file-input"
					type="file"
					accept=".sqlite3"
					bind:this={restoreFileInput}
					onchange={handleRestoreFileChange}
				/>
			</div>
		{/if}

		<p class="note">
			DBファイル横の backups/ ディレクトリにオンラインバックアップを作成します（VACUUM
			INTO、稼働中でも安全）。リストアはアップロード/選択したファイルを検証（整合性チェック+スキーマ確認）した上でステージし、次回起動時に自動適用します（稼働中のDB差し替えは行いません）。適用直前の現DBは自動的にバックアップされます（仕様
			M17）。
		</p>
	</section>
{/if}

<style>
	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
		font-weight: 400;
	}

	button.secondary:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-text) 8%, transparent);
	}

	.backup-toolbar {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.pending-restore {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		flex-wrap: wrap;
		margin: 0.75rem 0 0;
		padding: 0.6rem 0.8rem;
		border: 1px solid var(--banto-primary);
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
		font-size: 0.85rem;
	}

	.backup-list {
		list-style: none;
		margin: 0.75rem 0 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.backup-list li {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		flex-wrap: wrap;
		padding: 0.5rem 0.7rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
	}

	.backup-info {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.backup-info .file-name {
		font-size: 0.85rem;
		font-weight: 600;
		word-break: break-all;
	}

	.backup-info .meta {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.backup-actions {
		display: flex;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.backup-actions button,
	.pending-restore button {
		padding: 0.35rem 0.7rem;
		font-size: 0.8rem;
	}

	.restore-upload {
		margin-top: 0.75rem;
	}

	/* Visually hidden but still focusable/clickable via
	   restoreFileInput?.click() - same approach as banto-hub's CSV file
	   input. */
	.file-input {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
