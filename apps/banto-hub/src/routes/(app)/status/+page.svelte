<script lang="ts">
	/**
	 * 接続状態モニタ画面（実装指示のスコープ主軸機能、新規作成）。
	 * `GET /api/v1/status`（connections/revision/last_config_error）と
	 * `GET /api/v1/values`（全タグ現在値）を3秒ポーリングで表示する。
	 *
	 * ポーリングでよい理由（設計 §5.1）: 読み取りは
	 * `CollectorManager::current_values` が保持するオンメモリの現在値
	 * スナップショットを読むだけで、PLC への追加ポーリング要求は一切
	 * 発生しない（設計 §4: 「/api/v1/values* は current_values を読むだけで
	 * 完結し、PLC への追加要求を発生させない」）。つまりこの画面が
	 * リロードする頻度を上げても実機の負荷は増えないので、WebSocket/SSE
	 * 差分配信を新設するより単純な定期ポーリングで十分（WebSocket は
	 * 実装指示でも明示的にスコープ外）。
	 */
	import { isProviderError } from '@banto/admin-core';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import {
		getHubStatus,
		getHubValues,
		type ConnectionStatusEntry,
		type StatusResponse,
		type ValueEntry,
		type ValuesResponse
	} from '$lib/banto/hubStatus';
	import { enableWriteControl, disableWriteControl } from '$lib/banto/writeControlAdmin';
	import {
		canSwitchToDesktop,
		canSwitchToService,
		canToggleAutostart,
		hostSwitchDisabledReason,
		type HostSwitchGateInput
	} from '$lib/banto/hostSwitchGate';
	import {
		getHostSwitchStatus,
		isLocalShell,
		listenHostSwitchProgress,
		setServiceAutostart,
		switchToDesktop,
		switchToService,
		type HostSwitchProgress,
		type HostSwitchStatus
	} from '$lib/banto/hostSwitchShell';

	const canManageWriteControl = $derived(isAdmin(sessionStore.role));
	const localShell = isLocalShell();
	const hubAdmin = $derived(isAdmin(sessionStore.role));

	const POLL_INTERVAL_MS = 3000;

	function errorMessage(err: unknown): string {
		return isProviderError(err) ? err.message : String(err);
	}

	const statusLabels: Record<string, string> = {
		connected: '接続中',
		reconnecting: '再接続中',
		stopped: '停止中'
	};

	function statusLabel(status: string): string {
		return statusLabels[status] ?? status;
	}

	function statusClass(status: string): string {
		if (status === 'connected') return 'ok';
		if (status === 'reconnecting') return 'warn';
		return 'bad';
	}

	const qualityLabels: Record<string, string> = {
		good: '良好',
		bad: '不良',
		stale: '陳腐化'
	};

	function qualityLabel(q: string): string {
		return qualityLabels[q] ?? q;
	}

	/** 品質での色分け（実装指示: good=通常, bad=danger, stale=muted）。 */
	function qualityClass(q: string): string {
		if (q === 'bad') return 'bad';
		if (q === 'stale') return 'stale';
		return 'good';
	}

	function formatTime(epochMs: number): string {
		return new Date(epochMs).toLocaleString('ja-JP');
	}

	function formatValue(entry: ValueEntry): string {
		return entry.v === null ? '-' : String(entry.v);
	}

	let status: StatusResponse | null = $state(null);
	let values: ValuesResponse | null = $state(null);
	let loading = $state(true);
	let lastErrorShownAt = 0;

	// 連続失敗（サーバー停止中など）でトーストが3秒毎に積み上がらないよう、
	// 直近のエラー表示から一定時間は再表示を抑制する。
	const ERROR_TOAST_THROTTLE_MS = 15000;

	async function poll(): Promise<void> {
		try {
			const [nextStatus, nextValues] = await Promise.all([getHubStatus(), getHubValues()]);
			status = nextStatus;
			values = nextValues;
		} catch (err) {
			const now = Date.now();
			if (now - lastErrorShownAt > ERROR_TOAST_THROTTLE_MS) {
				lastErrorShownAt = now;
				toastStore.push('error', errorMessage(err));
			}
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void poll();
		const timer = setInterval(() => void poll(), POLL_INTERVAL_MS);
		return () => clearInterval(timer);
	});

	function connectionRowClass(conn: ConnectionStatusEntry): string {
		return `status-${statusClass(conn.status)}`;
	}

	// --- 書き込み受付トグル (T2-4、設計 §6-6、admin 限定) --------------------
	let writeControlBusy = $state(false);

	async function handleEnableWrites(): Promise<void> {
		writeControlBusy = true;
		try {
			await enableWriteControl();
			toastStore.push('success', '書き込み受付を有効化しました');
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			writeControlBusy = false;
		}
	}

	async function handleDisableWrites(): Promise<void> {
		writeControlBusy = true;
		try {
			await disableWriteControl();
			toastStore.push('success', '書き込み受付を無効化しました');
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			writeControlBusy = false;
		}
	}

	// --- Windows サービスカード（desktop-plan §9.7、ローカルシェル限定） -----
	let hostSwitch: HostSwitchStatus | null = $state(null);
	let hostSwitchBusy = $state(false);
	let hostProgress: HostSwitchProgress | null = $state(null);
	let hostSwitchError: string | null = $state(null);
	let autostartBusy = $state(false);

	const scmStateLabels: Record<string, string> = {
		NotInstalled: '未インストール',
		Stopped: '停止',
		StartPending: '開始中',
		StopPending: '停止中',
		Running: '実行中',
		Other: 'その他'
	};

	function scmStateLabel(raw: string | null | undefined): string {
		if (!raw) return '不明';
		if (raw.startsWith('Other(')) return `その他 (${raw.slice(6, -1)})`;
		return scmStateLabels[raw] ?? raw;
	}

	function gateInput(): HostSwitchGateInput {
		const viewRaw = hostSwitch?.view ?? 'fallback';
		const view =
			viewRaw === 'desktop' || viewRaw === 'service' || viewRaw === 'fallback'
				? viewRaw
				: 'fallback';
		return {
			isLocalShell: localShell,
			isAdmin: hubAdmin,
			canOperate: hostSwitch?.canOperate ?? false,
			view,
			switching: hostSwitchBusy || hostSwitch?.switching === true,
			lastConfigError: status?.last_config_error ?? null,
			hasRevision: status != null && typeof status.revision === 'number'
		};
	}

	const gate = $derived(gateInput());
	const disabledReason = $derived(hostSwitchDisabledReason(gate));
	const allowSwitchToService = $derived(canSwitchToService(gate));
	const allowSwitchToDesktop = $derived(canSwitchToDesktop(gate));
	const allowAutostart = $derived(canToggleAutostart(gate));

	async function refreshHostSwitch(): Promise<void> {
		if (!localShell) {
			hostSwitch = null;
			return;
		}
		try {
			hostSwitch = await getHostSwitchStatus();
		} catch (err) {
			hostSwitchError = errorMessage(err);
		}
	}

	$effect(() => {
		if (!localShell) return;
		void refreshHostSwitch();
		const timer = setInterval(() => {
			if (!hostSwitchBusy) void refreshHostSwitch();
		}, POLL_INTERVAL_MS);
		let unlisten: (() => void) | undefined;
		void listenHostSwitchProgress((ev) => {
			hostProgress = ev;
			if (ev.error) hostSwitchError = ev.error;
			if (ev.done) {
				hostSwitchBusy = false;
				void refreshHostSwitch();
				void poll();
			} else {
				hostSwitchBusy = true;
			}
		}).then((fn) => {
			unlisten = fn;
		});
		return () => {
			clearInterval(timer);
			unlisten?.();
		};
	});

	async function handleSwitchToService(): Promise<void> {
		hostSwitchBusy = true;
		hostSwitchError = null;
		hostProgress = {
			phase: 'starting',
			message: 'サービスへの切替を開始しています…',
			done: false,
			error: null
		};
		try {
			await switchToService();
		} catch (err) {
			hostSwitchBusy = false;
			hostSwitchError = errorMessage(err);
			toastStore.push('error', hostSwitchError);
		}
	}

	async function handleSwitchToDesktop(): Promise<void> {
		hostSwitchBusy = true;
		hostSwitchError = null;
		hostProgress = {
			phase: 'starting',
			message: 'アプリへの切替を開始しています…',
			done: false,
			error: null
		};
		try {
			await switchToDesktop();
		} catch (err) {
			hostSwitchBusy = false;
			hostSwitchError = errorMessage(err);
			toastStore.push('error', hostSwitchError);
		}
	}

	async function handleAutostartChange(enabled: boolean): Promise<void> {
		const nextLabel = enabled ? '自動起動（AutoStart）' : '手動起動（OnDemand）';
		const ok = window.confirm(
			`Banto Hub サービスの次回 Windows 起動時の設定を変更します。\n\n` +
				`変更後の起動種別: ${nextLabel}\n\n` +
				`この設定を変更しても、現在のサービスは開始・停止しません。\n` +
				`続行すると Windows の管理者昇格（UAC）が求められます。`
		);
		if (!ok) {
			await refreshHostSwitch();
			return;
		}
		autostartBusy = true;
		hostSwitchError = null;
		try {
			await setServiceAutostart(enabled);
			toastStore.push('success', enabled ? '自動起動を有効にしました' : '自動起動を無効にしました');
			await refreshHostSwitch();
		} catch (err) {
			hostSwitchError = errorMessage(err);
			toastStore.push('error', hostSwitchError);
			await refreshHostSwitch();
		} finally {
			autostartBusy = false;
		}
	}
</script>

<div class="page">
	<section>
		<h2>サーバー状態</h2>
		{#if loading && !status}
			<p class="note">読み込み中…</p>
		{:else if status}
			<dl class="summary">
				<dt>バージョン</dt>
				<dd>{status.version}</dd>
				<dt>リビジョン</dt>
				<dd>{status.revision}</dd>
			</dl>
			{#if status.last_config_error}
				<p class="config-error">設定エラー: {status.last_config_error}</p>
			{/if}

			<h3>接続一覧</h3>
			{#if status.connections.length === 0}
				<p class="note">登録されているPLC接続がありません。</p>
			{:else}
				<table class="conn-table">
					<thead>
						<tr>
							<th>名前</th>
							<th>状態</th>
							<th>再試行回数</th>
						</tr>
					</thead>
					<tbody>
						{#each status.connections as conn (conn.id)}
							<tr class={connectionRowClass(conn)}>
								<td>{conn.name}</td>
								<td>{statusLabel(conn.status)}</td>
								<td>{conn.attempt ?? '-'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			{/if}
		{/if}
	</section>

	{#if status && canManageWriteControl}
		<section>
			<h2>書き込み受付（管理者限定）</h2>
			<p class="note">
				プロセス再起動時は必ず無効化されます（安全側の既定 - 明示的に有効化するまで
				<code>POST /api/v1/values/&#123;tag&#125;</code> は 503 を返します）。
			</p>
			<dl class="summary">
				<dt>現在の状態</dt>
				<dd class={status.write_enabled ? 'write-on' : 'write-off'}>
					{status.write_enabled
						? '有効（書き込みを受け付けています）'
						: '無効（書き込みを拒否しています）'}
				</dd>
				<dt>再起動前の状態</dt>
				<dd>{status.write_was_enabled_before_restart ? '有効だった' : '無効だった'}</dd>
			</dl>
			<div class="write-control-actions">
				<button
					type="button"
					onclick={handleEnableWrites}
					disabled={writeControlBusy || status.write_enabled}
				>
					有効化する
				</button>
				<button
					type="button"
					class="danger"
					onclick={handleDisableWrites}
					disabled={writeControlBusy || !status.write_enabled}
				>
					無効化する
				</button>
			</div>
		</section>
	{/if}

	<section>
		<h2>Windows サービス</h2>
		{#if !localShell}
			<p class="note">ローカルシェルが必要です（ブラウザ遠隔からは操作できません）。</p>
		{:else if !hostSwitch}
			<p class="note">シェル状態を読み込み中…</p>
		{:else}
			<dl class="summary">
				<dt>現在の状態</dt>
				<dd>{scmStateLabel(hostSwitch.scmState)}</dd>
				<dt>シェル表示</dt>
				<dd>
					{hostSwitch.view === 'desktop'
						? 'アプリ'
						: hostSwitch.view === 'service'
							? 'サービス接続'
							: 'フォールバック'}
				</dd>
			</dl>

			{#if hostSwitchBusy || (hostProgress && !hostProgress.done)}
				<p class="switch-progress" role="status">
					{hostProgress?.message ?? '切替処理が進行中です…'}
				</p>
			{/if}

			{#if hostSwitchError}
				<p class="config-error">切替エラー: {hostSwitchError}</p>
			{/if}

			{#if disabledReason && !allowSwitchToService && !allowSwitchToDesktop}
				<p class="note">{disabledReason}</p>
			{/if}

			<div class="write-control-actions">
				<button type="button" onclick={handleSwitchToService} disabled={!allowSwitchToService}>
					サービスへ切り替えて開始
				</button>
				<button type="button" onclick={handleSwitchToDesktop} disabled={!allowSwitchToDesktop}>
					サービスを停止してアプリで開く
				</button>
			</div>

			<div class="autostart-block">
				<p class="autostart-heading">次回 Windows 起動</p>
				<label class="autostart-label">
					<input
						type="checkbox"
						checked={hostSwitch.autoStart}
						disabled={!allowAutostart || autostartBusy}
						onchange={(e) => void handleAutostartChange(e.currentTarget.checked)}
					/>
					Banto Hub サービスを自動起動する
				</label>
				<p class="note">
					この設定を変更しても、現在のサービスは開始・停止しません。変更時は管理者昇格（UAC）が必要です。
				</p>
			</div>
		{/if}
	</section>

	<section>
		<h2>タグ現在値</h2>
		{#if loading && !values}
			<p class="note">読み込み中…</p>
		{:else if values}
			<p class="note">
				{values.values.length}件のタグ ・ 更新時刻: {formatTime(values.t)}
			</p>
			{#if values.values.length === 0}
				<p class="note">登録されているタグがありません。</p>
			{:else}
				<div class="table-wrap">
					<table class="values-table">
						<thead>
							<tr>
								<th>外部名</th>
								<th>値</th>
								<th>品質</th>
								<th>時刻</th>
							</tr>
						</thead>
						<tbody>
							{#each values.values as entry (entry.tag)}
								<tr>
									<td class="tag-name">{entry.tag}</td>
									<td class="value quality-{qualityClass(entry.q)}">{formatValue(entry)}</td>
									<td class="quality quality-{qualityClass(entry.q)}">{qualityLabel(entry.q)}</td>
									<td>{formatTime(entry.t)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		{/if}
	</section>
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	h3 {
		margin: 1rem 0 0.5rem;
		font-size: 0.95rem;
	}

	.note {
		margin: 0 0 0.5rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.summary {
		display: grid;
		grid-template-columns: max-content 1fr;
		gap: 0.35rem 1rem;
		margin: 0;
		font-size: 0.875rem;
	}

	.summary dt {
		color: var(--banto-text-muted);
	}

	.summary dd {
		margin: 0;
	}

	.config-error {
		margin: 0.75rem 0 0;
		padding: 0.5rem 0.7rem;
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 0.85rem;
	}

	th {
		text-align: left;
		padding: 0.4rem 0.6rem;
		color: var(--banto-text-muted);
		font-weight: 600;
		border-bottom: 1px solid var(--banto-border);
	}

	td {
		padding: 0.4rem 0.6rem;
		border-bottom: 1px solid var(--banto-border);
	}

	tr.status-ok td {
		color: var(--banto-text);
	}

	tr.status-warn td {
		color: var(--banto-danger);
	}

	tr.status-bad td {
		color: var(--banto-text-muted);
	}

	.table-wrap {
		max-height: 480px;
		overflow-y: auto;
	}

	.tag-name {
		font-family: var(--banto-font-mono, monospace);
	}

	.quality-good {
		color: var(--banto-text);
	}

	.quality-bad {
		color: var(--banto-danger);
		font-weight: 600;
	}

	.quality-stale {
		color: var(--banto-text-muted);
	}

	.write-on {
		color: var(--banto-text);
		font-weight: 600;
	}

	.write-off {
		color: var(--banto-text-muted);
	}

	.write-control-actions {
		display: flex;
		gap: 0.5rem;
		margin-top: 0.75rem;
	}

	.write-control-actions button {
		padding: 0.5rem 1rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	.write-control-actions button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.write-control-actions button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
		font-weight: 400;
	}

	.switch-progress {
		margin: 0.75rem 0 0;
		padding: 0.5rem 0.7rem;
		border-radius: var(--banto-radius);
		background: color-mix(in srgb, var(--banto-primary) 12%, transparent);
		font-size: 0.85rem;
	}

	.autostart-block {
		margin-top: 1rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--banto-border);
	}

	.autostart-heading {
		margin: 0 0 0.35rem;
		font-size: 0.9rem;
		font-weight: 600;
	}

	.autostart-label {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.875rem;
		margin-bottom: 0.35rem;
	}
</style>
