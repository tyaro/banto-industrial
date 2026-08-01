<script lang="ts">
	/**
	 * エンジン制御・監視画面（plan `luminous-discovering-goblet.md` W4）。
	 * 自動書き込みエンジンの状態（アーム/非アーム・ドライラン）を大きく表示し、
	 * アーム/ディスアーム・ドライラン切替・リロード（再構築）を操作する。
	 *
	 * 安全設計（plan W3-B / 安全設計チェックリスト）をUIでも徹底する:
	 * - アームは「実PLCへ自動書き込みが始まる」危険操作なので、必ず確認
	 *   ダイアログの後にのみ実行する。
	 * - 起動直後は必ず非アーム（バックエンドが保証）。前回稼働時がアーム状態
	 *   だった場合は `wasArmedBeforeRestart` バナーで通知する（自動再開は
	 *   しない、手動アームを促すだけ）。
	 * - RBAC（invariant §1 両経路対称・バックエンドが権威）: アーム/ディス
	 *   アーム/リロード=admin、ドライラン=editor。権限の無い操作はボタン自体を
	 *   無効化/非表示にする（バックエンドでも拒否される）。
	 * - リロードは `Engine` オブジェクトの再構築でありデスクトップ版（Tauri）
	 *   専用。LAN/ブラウザ経路にはRESTルートが無いので、その環境ではボタンを
	 *   非表示にし「デスクトップ版のみ」と示す。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources, isAdmin } from '$lib/permissions';
	import {
		getStatus,
		arm,
		disarm,
		setDryRun,
		reload as reloadEngine,
		isEngineAvailable,
		isEngineReloadAvailable,
		DEMO_MODE_MESSAGE,
		type EngineStatus
	} from '$lib/banto/engineAdmin';

	const available = isEngineAvailable();
	const reloadAvailable = isEngineReloadAvailable();
	const canArm = $derived(isAdmin(sessionStore.role));
	const canDryRun = $derived(canWriteResources(sessionStore.role));

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	let status: EngineStatus | null = $state(null);
	let loading = $state(false);
	/** True while any arm/disarm/dry-run/reload action is in flight (disables buttons). */
	let acting = $state(false);

	async function refresh(): Promise<void> {
		if (!available) return;
		loading = true;
		try {
			status = await getStatus();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void refresh();
	});

	const ARM_CONFIRM =
		'アームすると、条件成立時に実PLCへ自動的に値が書き込まれます。本当にアームしますか？';
	const DISARM_CONFIRM =
		'エンジンをディスアームします（以後の物理書き込みを抑止します）。よろしいですか？';

	async function handleArm(): Promise<void> {
		// 危険操作: 必ず明示的な確認ダイアログの後にのみ実行する（安全設計）。
		if (!window.confirm(ARM_CONFIRM)) return;
		acting = true;
		try {
			await arm();
			toastStore.push('success', 'アームしました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			acting = false;
			await refresh();
		}
	}

	async function handleDisarm(): Promise<void> {
		if (!window.confirm(DISARM_CONFIRM)) return;
		acting = true;
		try {
			await disarm();
			toastStore.push('success', 'ディスアームしました');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			acting = false;
			await refresh();
		}
	}

	async function handleToggleDryRun(): Promise<void> {
		if (!status) return;
		const next = !status.dryRun;
		acting = true;
		try {
			await setDryRun(next);
			toastStore.push(
				'success',
				next ? 'ドライランを有効にしました' : 'ドライランを無効にしました'
			);
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			acting = false;
			await refresh();
		}
	}

	async function handleReload(): Promise<void> {
		if (
			!window.confirm(
				'現在のDB内容（有効な接続・ルール）でエンジンを再構築します。再構築後は必ず非アームで開始します。よろしいですか？'
			)
		) {
			return;
		}
		acting = true;
		try {
			status = await reloadEngine();
			toastStore.push('success', 'エンジンを再構築しました（非アームで開始）');
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			await refresh();
		} finally {
			acting = false;
		}
	}
</script>

<div class="page">
	<h2>エンジン制御・監視</h2>

	{#if !available}
		<p class="note">
			{DEMO_MODE_MESSAGE}。単体ブラウザのデモモードには自動書き込みエンジンが無いため、この機能はTauriアプリまたはLANアクセス（組み込みサーバー）でのみ利用できます。
		</p>
	{:else}
		{#if status?.wasArmedBeforeRestart}
			<p class="banner">
				前回の稼働時はアーム状態でした。安全のため再起動後は自動的に非アームで開始しています。必要なら手動でアームしてください。
			</p>
		{/if}

		<section class="status-panel">
			{#if loading && !status}
				<p class="loading">読み込み中…</p>
			{:else if status}
				<div class="badges">
					<div class="badge" class:armed={status.armed} class:disarmed={!status.armed}>
						<span class="badge-label">状態</span>
						<span class="badge-value"
							>{status.armed ? 'ARMED（アーム中）' : 'DISARMED（非アーム）'}</span
						>
						<span class="badge-sub">
							{status.armed
								? '条件成立時に実PLCへ自動書き込みが行われます'
								: '物理書き込みは抑止されています'}
						</span>
					</div>
					<div class="badge" class:dry={status.dryRun}>
						<span class="badge-label">ドライラン</span>
						<span class="badge-value">{status.dryRun ? 'ON' : 'OFF'}</span>
						<span class="badge-sub">
							{status.dryRun
								? '書き込みは行わずログのみ記録します'
								: '実際に書き込みます（アーム時）'}
						</span>
					</div>
				</div>
			{/if}
		</section>

		{#if status}
			<section class="controls">
				<h3>操作</h3>
				<p class="note">
					{canArm
						? 'アーム/ディスアームは管理者のみ操作できます。'
						: 'アーム/ディスアームには管理者権限が必要です（閲覧のみ）。'}
				</p>

				<div class="actions">
					{#if !status.armed}
						<button
							type="button"
							class="arm"
							onclick={handleArm}
							disabled={acting || !canArm}
							title={canArm ? '' : '管理者権限が必要です'}
						>
							アーム
						</button>
					{:else}
						<button
							type="button"
							class="disarm"
							onclick={handleDisarm}
							disabled={acting || !canArm}
							title={canArm ? '' : '管理者権限が必要です'}
						>
							ディスアーム
						</button>
					{/if}

					<button
						type="button"
						class="secondary"
						onclick={handleToggleDryRun}
						disabled={acting || !canDryRun}
						title={canDryRun ? '' : '編集者以上の権限が必要です'}
					>
						{status.dryRun ? 'ドライランを無効化' : 'ドライランを有効化'}
					</button>

					{#if reloadAvailable}
						<button
							type="button"
							class="secondary"
							onclick={handleReload}
							disabled={acting || !canArm}
							title={canArm ? '' : '管理者権限が必要です'}
						>
							エンジン再構築（リロード）
						</button>
					{:else}
						<button type="button" class="secondary" disabled title="デスクトップ版のみ利用できます">
							エンジン再構築（デスクトップ版のみ）
						</button>
					{/if}
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

	h3 {
		margin: 0 0 0.75rem;
		font-size: 0.95rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}

	/* 前回アーム状態の通知バナー（安全設計）。生色は使わず --banto-warning
	   相当が無いため --banto-danger を薄く敷いて注意を促す。 */
	.banner {
		margin: 0;
		padding: 0.6rem 0.85rem;
		font-size: 0.85rem;
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
	}

	.badges {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
		gap: 1rem;
	}

	.badge {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		padding: 1rem 1.25rem;
		border-radius: calc(var(--banto-radius) * 2);
		border: 2px solid var(--banto-border);
		background: var(--banto-bg);
	}

	/* ARMED は最も注意を要する状態なので --banto-danger、DISARMED は安全側
	   なので --banto-success で色分けする（生色禁止・テーマ変数のみ）。 */
	.badge.armed {
		border-color: var(--banto-danger);
		background: color-mix(in srgb, var(--banto-danger) 8%, transparent);
	}

	.badge.disarmed {
		border-color: var(--banto-success);
		background: color-mix(in srgb, var(--banto-success) 8%, transparent);
	}

	.badge.dry {
		border-color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.badge-label {
		font-size: 0.75rem;
		color: var(--banto-text-muted);
	}

	.badge-value {
		font-size: 1.3rem;
		font-weight: 700;
	}

	.badge.armed .badge-value {
		color: var(--banto-danger);
	}

	.badge.disarmed .badge-value {
		color: var(--banto-success);
	}

	.badge-sub {
		font-size: 0.78rem;
		color: var(--banto-text-muted);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
	}

	button {
		padding: 0.55rem 1.1rem;
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

	/* アームは危険操作なので --banto-danger で塗り、確認ダイアログと併せて
	   誤操作を防ぐ。 */
	button.arm {
		background: var(--banto-danger);
	}

	button.arm:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-danger) 85%, black);
	}

	button.disarm {
		background: var(--banto-success);
	}

	button.disarm:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-success) 85%, black);
	}

	button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text);
	}

	button.secondary:hover:not(:disabled) {
		background: var(--banto-bg);
	}
</style>
