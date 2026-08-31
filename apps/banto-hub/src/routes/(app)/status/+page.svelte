<script lang="ts">
	/**
	 * 接続状態モニタ画面（実装指示のスコープ主軸機能、新規作成）。
	 * `GET /api/status`（connections/revision/last_config_error）と
	 * `GET /api/values`（全タグ現在値）を3秒ポーリングで表示する
	 * （2026-08-31 オーナー決定「案A」で `/api/v1/status`・`/api/v1/values`
	 * から管理系エンドポイントへ切替 - 試運転モード中は `/api/v1/*` が
	 * 401 になるため、`hubStatus.ts`冒頭のdoc comment参照）。
	 *
	 * ポーリングでよい理由（設計 §5.1）: 読み取りは
	 * `CollectorManager::current_values` が保持するオンメモリの現在値
	 * スナップショットを読むだけで、PLC への追加ポーリング要求は一切
	 * 発生しない（設計 §4: 「/api/v1/values* は current_values を読むだけで
	 * 完結し、PLC への追加要求を発生させない」- `/api/values`も同じ
	 * `CollectorManager::current_values`を読むだけの`compute_status`/
	 * `build_values_response`を共有しているので同じ理屈が成り立つ）。
	 * つまりこの画面が
	 * リロードする頻度を上げても実機の負荷は増えないので、WebSocket/SSE
	 * 差分配信を新設するより単純な定期ポーリングで十分（WebSocket は
	 * 実装指示でも明示的にスコープ外）。
	 *
	 * T18-2d（docs/banto-hub-t18-design.md「T18-2d 初回導線チェックリスト」、
	 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A）: 「サーバー状態」の上に
	 * 初回チェックリスト（PLC接続作成→接続テスト→収集グループ作成→タグ登録
	 * →SIM値確認）を追加する。着地点が /status（navigation.ts の doc comment
	 * 参照）なので、ここが「サイドバーを探索せず案内だけで完了できる」の
	 * 起点として自然。完了判定・次工程算出はすべて `$lib/banto/tagOnboarding.ts`
	 * の純関数（実データ判定、画面訪問では判定しない）に委ねる。
	 * `listPlcConnections`/`listCollectionGroups`/`listTags` は3秒ポーリング
	 * には乗せず、チェックリストが未完了の間だけ取得する
	 * （`pollRegistry`/`onboardingDone` 参照）- 完了後は10,000タグ規模でも
	 * 無駄な一覧取得を繰り返さない。
	 */
	import { isProviderError } from '@banto/admin-core';
	import {
		applyPendingChange,
		cancelPendingChange,
		isPendingApplyConflictError,
		listPendingChanges,
		requeuePendingChange,
		type PendingChange
	} from '$lib/banto/pendingChangesAdmin';
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
	import {
		listPlcConnections,
		listCollectionGroups,
		listTags,
		type CollectionGroup,
		type PlcConnection,
		type Tag
	} from '$lib/banto/tagRegistryAdmin';
	import { computeOnboardingSteps, isOnboardingComplete } from '$lib/banto/tagOnboarding';
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
		if (isPendingApplyConflictError(err)) return err.failureReason ?? err.message;
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

	function formatDateTime(value: string): string {
		return new Date(value).toLocaleString('ja-JP');
	}

	function formatValue(entry: ValueEntry): string {
		return entry.v === null ? '-' : String(entry.v);
	}

	let status: StatusResponse | null = $state(null);
	let values: ValuesResponse | null = $state(null);
	let pendingChanges = $state<PendingChange[]>([]);
	let loading = $state(true);
	let lastErrorShownAt = 0;
	let pendingActionId = $state<number | null>(null);

	// 連続失敗（サーバー停止中など）でトーストが3秒毎に積み上がらないよう、
	// 直近のエラー表示から一定時間は再表示を抑制する。
	const ERROR_TOAST_THROTTLE_MS = 15000;

	async function poll(): Promise<void> {
		try {
			const [nextStatus, nextValues, nextPending] = await Promise.all([
				getHubStatus(),
				getHubValues(),
				hubAdmin ? listPendingChanges() : Promise.resolve<PendingChange[]>([])
			]);
			status = nextStatus;
			values = nextValues;
			pendingChanges = nextPending;
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

	// --- T18-2d 初回チェックリスト（TAG-UX-A） -------------------------------
	//
	// `status`/`values` は上の poll() が3秒毎に更新するが、一覧系
	// （plc-connections/collection-groups/tags）はチェックリスト専用の
	// 追加取得で、チェックリストが未完了の間だけ回す（`onboardingDone` が
	// 真になったら以後は取得しない - 冒頭 doc comment 参照）。
	let registrySnapshot: {
		connections: PlcConnection[];
		groups: CollectionGroup[];
		tags: Tag[];
	} | null = $state(null);
	let onboardingDone = $state(false);

	async function pollRegistry(): Promise<void> {
		if (onboardingDone) return;
		try {
			const [connections, groups, tags] = await Promise.all([
				listPlcConnections(),
				listCollectionGroups(),
				listTags()
			]);
			registrySnapshot = { connections, groups, tags };
		} catch {
			// ベストエフォート - チェックリストがこの周期だけ更新されないだけで、
			// 主機能（サーバー状態・書き込み受付等）の表示は poll() 側が独立して
			// 継続する。エラートーストは poll() 側と重複するので出さない。
		}
	}

	const onboardingSteps = $derived.by(() => {
		if (!registrySnapshot || !status || !values) return [];
		return computeOnboardingSteps({
			connections: registrySnapshot.connections,
			groups: registrySnapshot.groups,
			tags: registrySnapshot.tags,
			connectionStatuses: status.connections,
			values: values.values
		});
	});

	$effect(() => {
		if (onboardingSteps.length > 0 && isOnboardingComplete(onboardingSteps)) {
			onboardingDone = true;
		}
	});

	$effect(() => {
		void poll();
		void pollRegistry();
		const timer = setInterval(() => {
			void poll();
			void pollRegistry();
		}, POLL_INTERVAL_MS);
		return () => clearInterval(timer);
	});

	function connectionRowClass(conn: ConnectionStatusEntry): string {
		return `status-${statusClass(conn.status)}`;
	}

	function pendingStateLabel(state: PendingChange['state']): string {
		if (state === 'pending') return '保留中';
		if (state === 'applying') return '適用中';
		if (state === 'applied') return '適用済み';
		if (state === 'canceled') return 'キャンセル済み';
		if (state === 'failed') return '失敗';
		return state;
	}

	function pendingStateClass(state: PendingChange['state']): string {
		if (state === 'applied') return 'good';
		if (state === 'failed' || state === 'canceled') return 'bad';
		if (state === 'applying') return 'warn';
		return 'stale';
	}

	async function handleApplyPending(change: PendingChange): Promise<void> {
		pendingActionId = change.id;
		try {
			await applyPendingChange(change.id);
			toastStore.push('success', `pending change #${change.id} を適用しました`);
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			if (isPendingApplyConflictError(err)) {
				await poll();
			}
		} finally {
			pendingActionId = null;
		}
	}

	async function handleCancelPending(change: PendingChange): Promise<void> {
		pendingActionId = change.id;
		try {
			await cancelPendingChange(change.id);
			toastStore.push('success', `pending change #${change.id} をキャンセルしました`);
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			pendingActionId = null;
		}
	}

	async function handleRequeuePending(change: PendingChange): Promise<void> {
		pendingActionId = change.id;
		try {
			await requeuePendingChange(change.id);
			toastStore.push('success', `pending change #${change.id} を再試行キューへ戻しました`);
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			pendingActionId = null;
		}
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
	{#if onboardingSteps.length > 0 && !isOnboardingComplete(onboardingSteps)}
		<section class="onboarding">
			<h2>初回セットアップ</h2>
			<p class="note">PLC接続の作成からSIM値の確認まで、この画面の案内だけで完了できます。</p>
			<ol class="onboarding-list">
				{#each onboardingSteps as step (step.id)}
					<li class="onboarding-step" class:done={step.done}>
						<span class="onboarding-mark" aria-hidden="true">{step.done ? '✓' : '○'}</span>
						<span class="onboarding-label">{step.label}</span>
						{#if !step.done}
							<a class="onboarding-cta" href={step.href}>{step.ctaLabel}</a>
						{/if}
					</li>
				{/each}
			</ol>
		</section>
	{/if}

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

	{#if hubAdmin}
		<section>
			<h2>Pending changes</h2>
			<p class="note">{pendingChanges.length}件の変更提案があります。</p>
			{#if pendingChanges.length === 0}
				<p class="note">保留中の変更はありません。</p>
			{:else}
				<div class="table-wrap">
					<table class="pending-table">
						<thead>
							<tr>
								<th>ID</th>
								<th>source</th>
								<th>state</th>
								<th>requestedByUsername</th>
								<th>createdAt</th>
								<th>failureReason</th>
								<th>操作</th>
							</tr>
						</thead>
						<tbody>
							{#each pendingChanges as change (change.id)}
								<tr>
									<td>#{change.id}</td>
									<td class="pending-source">{change.source}</td>
									<td>
										<span class={`state-chip state-${pendingStateClass(change.state)}`}>
											{pendingStateLabel(change.state)}
										</span>
									</td>
									<td>{change.requestedByUsername ?? '-'}</td>
									<td>{formatDateTime(change.createdAt)}</td>
									<td>
										{#if change.failureReason}
											<div class:config-error={change.state === 'failed'} class="pending-failure">
												{change.failureReason}
											</div>
										{:else}
											-
										{/if}
									</td>
									<td>
										{#if change.state === 'pending'}
											<div class="pending-actions">
												<button
													type="button"
													onclick={() => void handleApplyPending(change)}
													disabled={pendingActionId !== null}
												>
													適用
												</button>
												<button
													type="button"
													class="danger"
													onclick={() => void handleCancelPending(change)}
													disabled={pendingActionId !== null}
												>
													キャンセル
												</button>
											</div>
										{:else if change.state === 'failed'}
											<div class="pending-actions">
												<button
													type="button"
													onclick={() => void handleRequeuePending(change)}
													disabled={pendingActionId !== null}
												>
													再試行
												</button>
												<button
													type="button"
													class="danger"
													onclick={() => void handleCancelPending(change)}
													disabled={pendingActionId !== null}
												>
													キャンセル
												</button>
											</div>
										{:else}
											-
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{/if}
		</section>
	{/if}

	<section>
		<h2>Windows サービス</h2>
		{#if !localShell}
			<p class="note">ローカルシェルが必要です（ブラウザ遠隔からは操作できません）。</p>
		{:else if !hostSwitch}
			{#if hostSwitchError}
				<p class="config-error">シェル状態の取得に失敗しました: {hostSwitchError}</p>
			{:else}
				<p class="note">シェル状態を読み込み中…</p>
			{/if}
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

	/* T18-2d（TAG-UX-A）: 初回チェックリスト。 */
	.onboarding {
		border-color: var(--banto-primary);
	}

	.onboarding-list {
		list-style: none;
		margin: 0.5rem 0 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 0.4rem;
	}

	.onboarding-step {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.875rem;
		padding: 0.35rem 0.5rem;
		border-radius: var(--banto-radius);
	}

	.onboarding-step.done {
		color: var(--banto-text-muted);
	}

	.onboarding-mark {
		display: inline-flex;
		width: 1.2rem;
		justify-content: center;
		font-weight: 700;
	}

	.onboarding-step.done .onboarding-mark {
		color: var(--banto-success, #1a7f37);
	}

	.onboarding-label {
		flex: 1;
	}

	.onboarding-cta {
		padding: 0.3rem 0.75rem;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		font-size: 0.8rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.onboarding-cta:hover {
		background: var(--banto-primary-hover);
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

	.pending-source {
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

	.state-chip {
		display: inline-flex;
		align-items: center;
		padding: 0.15rem 0.45rem;
		border-radius: 999px;
		font-size: 0.78rem;
		font-weight: 600;
		border: 1px solid var(--banto-border);
	}

	.state-good {
		color: var(--banto-text);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.state-warn {
		color: var(--banto-danger);
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}

	.state-bad {
		color: var(--banto-danger);
		border-color: color-mix(in srgb, var(--banto-danger) 40%, var(--banto-border));
		background: color-mix(in srgb, var(--banto-danger) 12%, transparent);
	}

	.state-stale {
		color: var(--banto-text-muted);
	}

	.pending-failure {
		margin: 0;
		font-size: 0.8rem;
		white-space: pre-wrap;
	}

	.pending-actions {
		display: flex;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.pending-actions button {
		padding: 0.35rem 0.75rem;
		border: none;
		border-radius: var(--banto-radius);
		background: var(--banto-primary);
		color: var(--banto-text-inverse);
		font-weight: 600;
		cursor: pointer;
	}

	.pending-actions button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.pending-actions button.danger {
		background: transparent;
		border: 1px solid var(--banto-danger);
		color: var(--banto-danger);
		font-weight: 400;
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
