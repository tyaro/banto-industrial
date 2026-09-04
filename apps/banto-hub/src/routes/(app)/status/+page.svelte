<script lang="ts">
	/**
	 * サーバー状態モニタ画面（実装指示のスコープ主軸機能、新規作成）。
	 * `GET /api/status`（connections/revision/last_config_error/
	 * サービス状態/CPU・メモリ）を3秒ポーリングで表示する（2026-08-31
	 * オーナー決定「案A」で `/api/v1/status` から管理系エンドポイントへ
	 * 切替 - 試運転モード中は `/api/v1/*` が 401 になるため、
	 * `hubStatus.ts`冒頭のdoc comment参照）。
	 *
	 * **T19 S3-b（UX-47、docs/banto-hub-t19-design.md §3.9、2026-09-04）**:
	 * 以前はここに `GET /api/values`（全タグ現在値）も3秒ポーリングして
	 * 表示していたが撤去した - 現在値はタグモニタ画面が担う（同じ情報を
	 * 複数画面が持つと表示規約の二重管理になる、という UX-30 と同じ理屈）。
	 * この画面に残すのは「サーバーの状態」に属する情報 -
	 * 接続状態・pending 変更・各種サービス状態・CPU/メモリ（UX-46、下の
	 * 「サーバーリソース」セクション）。
	 *
	 * ポーリングでよい理由（設計 §5.1）: 読み取りは
	 * `CollectorManager::current_values` が保持するオンメモリの現在値
	 * スナップショットや、既に確保済みの `SystemInfoSampler` を読むだけで、
	 * PLC への追加ポーリング要求は一切発生しない。つまりこの画面が
	 * リロードする頻度を上げても実機の負荷は増えないので、WebSocket/SSE
	 * 差分配信を新設するより単純な定期ポーリングで十分（WebSocket は
	 * 実装指示でも明示的にスコープ外）。
	 *
	 * T19 S1-d（docs/banto-hub-t19-design.md UX-44、2026-09-03）: T18-2d で
	 * 「サーバー状態」の上に置いていた初回チェックリスト（PLC接続作成→収集
	 * グループ作成→タグ登録→収集開始→モニタで値確認、完了判定は
	 * `$lib/banto/tagOnboarding.ts::computeOnboardingSteps` の純関数）を撤去
	 * した（2026-09-02 オーナー決定「起動直後は何も出さない」）。設定操作の
	 * 入口はタグ画面（`/tags`）へ既に一本化済み（S1-a〜S1-c）で、案内が無くても
	 * ツリーの右クリックから接続・グループ・タグを作成できる。管理アカウント
	 * 作成の経路は撤去していない -「ロックダウンしたい」「ユーザーを分けたい」
	 * ときだけユーザー管理画面（`/users`、admin限定ナビ）から作る形にした
	 * （ロックダウンには admin アカウントが必須 - 設定画面のロックダウン
	 * セクション・`commissioning_lock_down_fails_without_any_admin_account`
	 * 参照）。
	 *
	 * **2026-08-31 オーナー指摘（収集の開始/停止 UI の追加）**: `rest.rs` の
	 * `commit_catalog_and_notify` の doc comment のとおり、本番経路では
	 * PLC接続/収集グループ/タグの登録・変更は configured revision を
	 * 進めるだけで、動いている（あるいはまだ一度も開始していない）収集機
	 * には反映されない。`POST /api/collection/start|stop` 自体は元々 API
	 * にしか無く、UI から叩く導線が1つも無かった - 実機での試運転の最後の
	 * 一歩「PLC に接続開始し、タグにアクセスできているか確認する」を画面
	 * から行えなかった。「収集の開始・停止」セクション（`#collection-control`、
	 * `collectionControlAdmin.ts` 使用）はその導線。接続単位のシミュレーション
	 * （`PlcConnection.simulation`、T9-2、接続 Drawer のチェックボックス）は
	 * 今回のオーナー指摘とは無関係で
	 * 一切変更していない。
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
		type ConnectionStatusEntry,
		type StatusResponse
	} from '$lib/banto/hubStatus';
	import { formatBytes, formatPercent } from '$lib/banto/systemInfoFormat';
	import { listPlcConnections, isVirtualConnection } from '$lib/banto/tagRegistryAdmin';
	import { enableWriteControl, disableWriteControl } from '$lib/banto/writeControlAdmin';
	import {
		startCollection,
		startAllSimulationCollection,
		stopCollection
	} from '$lib/banto/collectionControlAdmin';
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
		stopped: '停止中',
		// T19 S2-a（UX-48）: タグの無い接続はそもそもセッションを張らない
		// ので「停止中」（本来動くはずが止まっている）とは区別する - 見た目
		// が同じだと壊れていると誤解される（docs/banto-hub-t19-design.md
		// §3.8）。
		unused: '未使用（タグ未登録）'
	};

	function statusLabel(status: string): string {
		return statusLabels[status] ?? status;
	}

	function statusClass(status: string): string {
		if (status === 'connected') return 'ok';
		if (status === 'reconnecting') return 'warn';
		if (status === 'unused') return 'muted';
		return 'bad';
	}

	function formatDateTime(value: string): string {
		return new Date(value).toLocaleString('ja-JP');
	}

	let status: StatusResponse | null = $state(null);
	let pendingChanges = $state<PendingChange[]>([]);
	let loading = $state(true);
	let lastErrorShownAt = 0;
	let pendingActionId = $state<number | null>(null);

	// 連続失敗（サーバー停止中など）でトーストが3秒毎に積み上がらないよう、
	// 直近のエラー表示から一定時間は再表示を抑制する。
	const ERROR_TOAST_THROTTLE_MS = 15000;

	async function poll(): Promise<void> {
		try {
			const [nextStatus, nextPending] = await Promise.all([
				getHubStatus(),
				hubAdmin ? listPendingChanges() : Promise.resolve<PendingChange[]>([])
			]);
			status = nextStatus;
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

	// T19 S1-d（UX-44、docs/banto-hub-t19-design.md、2026-09-03）: T18-2d の
	// 初回チェックリスト（`registrySnapshot`/`onboardingDone`/`pollRegistry`/
	// `onboardingSteps` とそれを完了させる `$effect`）を撤去した（2026-09-02
	// オーナー決定「起動直後は何も出さない」）。`listPlcConnections` は
	// `handleStartCollection` が確認ダイアログ用に個別で呼ぶため引き続き
	// import している。

	$effect(() => {
		void poll();
		const timer = setInterval(() => void poll(), POLL_INTERVAL_MS);
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

	// --- 収集の開始・停止 (2026-08-31 オーナー指摘、admin 限定) --------------
	//
	// `POST /api/collection/start|start-all-simulation|stop` は元々 admin
	// ロール必須（`collection_control_router` の `RoleGuard{min: Role::Admin}`）
	// なので、書き込み受付トグルと同じく操作ボタンは `canManageWriteControl`
	// （= admin）でだけ出す。状態表示自体（現在の運転状態）は「サーバー状態」
	// と同じ「読み取り専用・ロール不問」の扱いにする。
	let collectionActionBusy = $state(false);

	// 関数越しに読む理由: `status?.collection_state` を `$derived(...)` へ直接
	// 書くと svelte-check（TS の制御フロー解析）が誤って `status` を `never`
	// と推論するケースがある - このファイルの他の `$derived`
	// （`gate`/`disabledReason`等）もすべて関数呼び出し越しに読む形にして
	// 回避している、それに倣う。
	function computeIsCollectionTransitioning(): boolean {
		return status?.collection_state === 'starting' || status?.collection_state === 'stopping';
	}
	const isCollectionTransitioning = $derived(computeIsCollectionTransitioning());

	// faulted も「開始」で再試行できるようにする（専用の再試行 API は無く、
	// `start` を再度叩くのが唯一の回復手段 - `controller.rs` 参照）。
	function computeCanStartCollection(): boolean {
		return (
			!isCollectionTransitioning &&
			(status?.collection_state === 'stopped' || status?.collection_state === 'faulted')
		);
	}
	const canStartCollection = $derived(computeCanStartCollection());

	function computeCanStopCollection(): boolean {
		return !isCollectionTransitioning && status?.collection_state === 'running';
	}
	const canStopCollection = $derived(computeCanStopCollection());

	const collectionStateLabels: Record<string, string> = {
		stopped: '収集停止',
		starting: '開始中',
		stopping: '停止処理中',
		faulted: '異常停止'
	};

	/** desktop-plan §9.7 の状態表示（`running` は mode で「設定どおり運転」/
	 * 「全PLCシミュレーション」に分ける）。 */
	function collectionStateLabel(): string {
		if (!status) return '-';
		if (status.collection_state === 'running') {
			return status.collection_mode === 'all_simulation'
				? '全PLCシミュレーション運転中'
				: '設定どおり運転中';
		}
		return collectionStateLabels[status.collection_state] ?? status.collection_state;
	}

	function collectionStateClass(): string {
		if (!status) return '';
		if (status.collection_state === 'running') {
			return status.collection_mode === 'all_simulation' ? 'warn' : 'ok';
		}
		if (status.collection_state === 'faulted') return 'bad';
		return '';
	}

	/**
	 * desktop-plan §9.7「確認・エラー文言の基準」: 「開始」は実機/SIM接続の
	 * 内訳をボタン直前に示す。ここでの内訳は `registrySnapshot`（チェック
	 * リスト完了後は更新が止まる、冒頭 doc comment参照）に頼らず、確認直前に
	 * 毎回 `listPlcConnections` を取り直して正確な件数にする - 収集開始は
	 * 頻繁に押す操作ではないので追加の一覧取得コストは無視できる。
	 */
	async function handleStartCollection(): Promise<void> {
		collectionActionBusy = true;
		try {
			const connections = await listPlcConnections();
			const active = connections.filter((c) => c.enabled && !isVirtualConnection(c));
			const realCount = active.filter((c) => !c.simulation).length;
			const simCount = active.filter((c) => c.simulation).length;
			const writeLine = status?.write_enabled
				? 'PLC書き込み: 有効（書き込みを受け付けています）'
				: 'PLC書き込み: OFF';
			const ok = window.confirm(
				'設定どおり収集を開始しますか。\n\n' +
					`実PLC: ${realCount}接続 / 接続別SIM: ${simCount}接続\n` +
					'履歴: 実機由来値を記録 / 通常外部出力: 設定どおり\n' +
					writeLine
			);
			if (!ok) return;
			await startCollection();
			toastStore.push('success', '収集を開始しました');
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			collectionActionBusy = false;
		}
	}

	/**
	 * 全PLCシミュレーション開始 - 主動線ではなく副次的な選択肢
	 * （2026-08-31 オーナー指摘、`collectionControlAdmin.ts` 冒頭のdoc
	 * comment参照）。desktop-plan §9.7 の確認文言基準どおり「実PLCへ接続
	 * しない」「実機履歴・通常外部出力へ記録しない」を確認する。
	 */
	async function handleStartAllSimulation(): Promise<void> {
		collectionActionBusy = true;
		try {
			const ok = window.confirm(
				'全PLCシミュレーションを開始しますか。\n\n' +
					'実PLCには接続しません。実機履歴と通常外部出力へは記録しません。'
			);
			if (!ok) return;
			await startAllSimulationCollection();
			toastStore.push('success', '全PLCシミュレーションを開始しました');
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			collectionActionBusy = false;
		}
	}

	/** desktop-plan §9.7 の確認文言基準どおりの文言。 */
	async function handleStopCollection(): Promise<void> {
		collectionActionBusy = true;
		try {
			const ok = window.confirm(
				'収集を停止します。履歴を flush し、PLC接続と通常の外部出力を停止します。\n\nよろしいですか？'
			);
			if (!ok) return;
			await stopCollection();
			toastStore.push('success', '収集を停止しました');
			await poll();
		} catch (err) {
			toastStore.push('error', errorMessage(err));
		} finally {
			collectionActionBusy = false;
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
				<!--
					T19 S1-d（UX-45、docs/banto-hub-t19-design.md §3.6、2026-09-03）:
					`CommissioningBanner.svelte` の常時表示（全画面共通の警告バナー）を
					やめた代わりに、事実として状態を確認できる場所をここに残す -
					警告ではなく「サーバー状態」の一項目として並べる。安全性は
					損なわれない: 試運転モード中は非 loopback バインドが構造的に
					拒否される（`enforce_loopback_when_commissioning`）ため、無認証の
					まま外部ネットワークへ露出することはない（設計 §3.6）。
				-->
				<dt>試運転モード</dt>
				<dd>
					{#if sessionStore.commissioningMode}
						有効（未ロックダウン・認証なしでアクセス可能）
					{:else}
						無効（認証必須）
					{/if}
				</dd>
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

	<section>
		<h2>サーバーリソース</h2>
		{#if loading && !status}
			<p class="note">読み込み中…</p>
		{:else if status}
			<dl class="summary">
				<dt>CPU 使用率</dt>
				<dd>{formatPercent(status.system.cpu_percent)}</dd>
				<dt>プロセスメモリ（RSS）</dt>
				<dd>{formatBytes(status.system.process_memory_bytes)}</dd>
				<dt>ホストメモリ</dt>
				<dd>
					{formatBytes(status.system.host_memory_used_bytes)} / {formatBytes(
						status.system.host_memory_total_bytes
					)}
				</dd>
			</dl>
		{/if}
	</section>

	<section id="collection-control">
		<h2>収集の開始・停止</h2>
		{#if !status}
			<p class="note">読み込み中…</p>
		{:else}
			<dl class="summary">
				<dt>現在の状態</dt>
				<dd class={collectionStateClass()}>{collectionStateLabel()}</dd>
			</dl>
			{#if status.collection_state === 'running' && status.collection_mode === 'all_simulation'}
				<p class="config-error">
					⚠ 全PLCシミュレーション運転中 -
					実PLCへの接続、実機履歴、通常の外部出力への記録は行っていません。
				</p>
			{/if}
			{#if status.last_runtime_error}
				<p class="config-error">実行時エラー: {status.last_runtime_error}</p>
			{/if}
			{#if canManageWriteControl}
				<div class="write-control-actions">
					{#if canStartCollection}
						<button
							type="button"
							onclick={() => void handleStartCollection()}
							disabled={collectionActionBusy}
						>
							開始
						</button>
						<button
							type="button"
							class="secondary"
							onclick={() => void handleStartAllSimulation()}
							disabled={collectionActionBusy}
						>
							全PLC シミュレーション
						</button>
					{/if}
					{#if canStopCollection}
						<button
							type="button"
							class="danger"
							onclick={() => void handleStopCollection()}
							disabled={collectionActionBusy}
						>
							停止
						</button>
					{/if}
				</div>
				{#if isCollectionTransitioning}
					<p class="note">処理中です。しばらくお待ちください…</p>
				{/if}
			{:else}
				<p class="note">開始・停止は管理者限定です。</p>
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

	tr.status-muted td {
		/* T19 S2-a（UX-48）: 「停止中」（status-bad）と同じ落ち着いた色調だが、
		   壊れているのではなく元から使っていないことを示すため斜体で
		   区別する。 */
		color: var(--banto-text-muted);
		font-style: italic;
	}

	.table-wrap {
		max-height: 480px;
		overflow-y: auto;
	}

	.pending-source {
		font-family: var(--banto-font-mono, monospace);
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

	/* 「収集の開始・停止」セクションの現在状態表示（`collectionStateClass()`）。 */
	.summary dd.ok {
		color: var(--banto-text);
		font-weight: 600;
	}

	.summary dd.warn {
		color: var(--banto-danger);
		font-weight: 600;
	}

	.summary dd.bad {
		color: var(--banto-danger);
		font-weight: 700;
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

	/* 「全PLCシミュレーション」等の副次的な操作 - 主操作より目立たせない
	   （2026-08-31 オーナー指摘: 全PLC SIM は主動線ではなく副次的な選択肢）。 */
	.write-control-actions button.secondary {
		background: transparent;
		border: 1px solid var(--banto-border);
		color: var(--banto-text-muted);
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
