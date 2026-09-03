<script lang="ts">
	/**
	 * 設定画面。relay-wright の同名ファイル（1126行）から「テーマ」
	 * 「プリセット」「アカウント」の3セクションのみを残した（実装指示:
	 * 「settings は 1126 行版から『テーマ』『プリセット』『アカウント』
	 * だけに削減」）。削除したセクション（ウィンドウ効果/LANアクセス/認証
	 * （ログイン不要モード）/自動ログイン/監査ログ保持ポリシー/バックアップ・
	 * リストア）はいずれも Tauri 専用機能か、実装指示の明示スコープ外
	 * （バックアップ、LAN/認証/自動ログイン設定セクション）に該当する。
	 */
	import type { ThemeMode, ThemePreset } from '@banto/theme';
	import { goto } from '$app/navigation';
	import { getAuthProvider, isProviderError } from '@banto/admin-core';
	import { settings } from '$lib/settings.svelte';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { getHubStatus } from '$lib/banto/hubStatus';
	import { lockDown } from '$lib/banto/commissioning';
	import { listUsers } from '$lib/banto/usersAdmin';
	import {
		getMqttSettings,
		saveMqttSettings,
		type MqttSettings
	} from '$lib/banto/mqttSettingsAdmin';
	import {
		getGrpcSettings,
		saveGrpcSettings,
		type GrpcSettings
	} from '$lib/banto/grpcSettingsAdmin';
	import {
		getStoreSettings,
		setStoreSettings,
		previewPrune,
		pruneNow
	} from '$lib/banto/storeSettingsAdmin';
	import {
		formToRetentionDays,
		formatPruneConfirmMessage,
		formatPruneDoneMessage,
		formatRetentionSavedMessage,
		hasUnsavedRetentionChange,
		pruneDisabledReason,
		retentionDaysToForm,
		validateRetentionForm,
		type RetentionFormState
	} from '$lib/banto/storeRetentionForm';
	import {
		applyConfigPackage,
		inspectConfigPackage,
		loadConfigPackage,
		isConfigPackageImportAbortedError
	} from '$lib/banto/configPackageAdmin';
	import {
		parseConfigPackage,
		serializeConfigPackage,
		type ConfigPackage,
		type ConfigPackageInspection,
		type ConfigPackageImportSummary
	} from '$lib/banto/configPackage';

	const IMPORT_BLOCKED_MESSAGE = '構成パッケージの取り込みは収集を停止してから実行してください';

	function errorMessage(err: unknown): string {
		if (isConfigPackageImportAbortedError(err)) {
			return err.message;
		}
		if (isProviderError(err)) {
			if (err.body.kind === 'validation' && err.body.field_errors.length > 0) {
				return err.body.field_errors.map((fe) => fe.message).join(' / ');
			}
			return err.message;
		}
		return String(err);
	}

	const modes: { value: ThemeMode; label: string }[] = [
		{ value: 'light', label: 'ライト' },
		{ value: 'dark', label: 'ダーク' },
		{ value: 'system', label: 'システムに従う' }
	];

	const presets: { value: ThemePreset; label: string }[] = [
		{ value: 'standard', label: 'スタンダード' },
		{ value: 'glass', label: 'ガラス' }
	];

	const changePassword = getAuthProvider().changePassword;

	let currentPassword = $state('');
	let newPassword = $state('');
	let newPasswordConfirm = $state('');
	let passwordError: string | null = $state(null);
	let changingPassword = $state(false);

	async function submitChangePassword(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		passwordError = null;

		if (newPassword.length < 8) {
			passwordError = 'パスワードは8文字以上で入力してください';
			return;
		}
		if (newPassword !== newPasswordConfirm) {
			passwordError = 'パスワードが一致しません';
			return;
		}
		if (!changePassword) return;

		changingPassword = true;
		try {
			const result = await changePassword(currentPassword, newPassword);
			if (result.success) {
				currentPassword = '';
				newPassword = '';
				newPasswordConfirm = '';
				toastStore.push('success', 'パスワードを変更しました');
			} else {
				passwordError = result.error ?? 'パスワードの変更に失敗しました';
			}
		} catch (err) {
			passwordError = errorMessage(err);
		} finally {
			changingPassword = false;
		}
	}

	// --- 試運転モードのロックダウン（設計 §5.6・2026-08-30 オーナー決定） -----
	//
	// 表示条件は `sessionStore.commissioningMode` の1つ（ロックダウン済みなら
	// 元々このフラグが false なので、セクションごと非表示になる - 実装指示
	// 「ロックダウン済みのときはこの操作を表示しないこと」）。
	//
	// admin アカウントが1件も無いとサーバーが拒否する（詰み防止、
	// `apps/banto-hub/core/src/commissioning.rs` の `no_admin_account_error`）。
	// 実行してから失敗を見せるより「そもそも押せない」方が親切なので、
	// `listUsers()`（既存の管理 API、バックエンド変更なしで流用できる）で
	// 事前に admin の有無を確認し、無ければボタンを無効化して理由を出す。
	// この事前チェックはあくまで UX 用のヒントで、権威ある判定は最終的に
	// サーバー側の `lock_down()` が行う（`handleLockDown` の catch で validation
	// エラーを表示するのはそのため - 事前チェックと実行の間に他クライアントが
	// 最後の admin を消す、というレースも理論上あり得る）。
	const NO_ADMIN_MESSAGE =
		'管理者（adminロール）アカウントが1件も存在しないため、ロックダウンできません。' +
		'この状態で施錠すると誰もログインできなくなり、管理操作が一切できなくなります。' +
		'先にユーザー管理から管理者アカウントを作成してください。';

	/** null = 未確認（読み込み中 or 取得失敗）。安全側でボタンは無効のまま。 */
	let hasAdminAccount: boolean | null = $state(null);
	let adminCheckError: string | null = $state(null);
	let lockingDown = $state(false);
	let lockDownError: string | null = $state(null);

	$effect(() => {
		if (!sessionStore.commissioningMode) return;
		let cancelled = false;
		(async () => {
			try {
				const users = await listUsers();
				if (!cancelled) hasAdminAccount = users.some((u) => u.role === 'admin');
			} catch (err) {
				if (!cancelled) adminCheckError = errorMessage(err);
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	const lockDownDisabledReason = $derived(
		lockingDown
			? null // ボタン自体は disabled になるが、理由表示は「読み込み中/不可」時のみでよい
			: hasAdminAccount === null
				? (adminCheckError ?? '管理者アカウントの有無を確認しています…')
				: hasAdminAccount === false
					? NO_ADMIN_MESSAGE
					: null
	);

	async function handleLockDown(): Promise<void> {
		if (
			!window.confirm(
				'ロックダウンを実行すると元に戻せません。' +
					'以後、管理操作にはログインが必須になります（試運転モードへは UI から戻せません）。' +
					'実行しますか？'
			)
		) {
			return;
		}

		lockingDown = true;
		lockDownError = null;
		try {
			await lockDown();
			toastStore.push('success', 'ロックダウンしました。ログイン画面へ移動します。');
			// ロックダウン後は以後の全リクエストで認証が必須になる - この画面に
			// 留まらせると後続の管理 API 呼び出しが軒並み 401 になって壊れて
			// 見えるため、即座にログイン画面へ誘導する（実装指示のとおり）。
			// ここまで来た時点で試運転モード中の合成セッションしか無い可能性が
			// 高いが、万一実トークンを保持していた場合に備えて `logout()` で
			// 確実に破棄してから遷移する（`Header.svelte` の logout と同じ手順）。
			await getAuthProvider().logout();
			goto('/login');
		} catch (err) {
			lockDownError = errorMessage(err);
		} finally {
			lockingDown = false;
		}
	}

	// --- MQTT 発行（T3、設計 §5.3、admin 限定） -------------------------------
	//
	// 保存は `PUT /api/mqtt-settings` を叩くだけで即時適用される(実装指示
	// 「保存で即時適用」- サーバー側が保存直後に `MqttPublisher::apply` を
	// 呼ぶので、ここでは保存 API を呼んで結果を反映するだけでよい)。
	// `password` は空欄のまま保存すると「変更なし」として扱われる
	// (`mqttSettingsAdmin.ts` の doc comment参照) - フォームには常に空欄で
	// 表示し、入力があった場合だけ送る。

	const canManageMqtt = $derived(isAdmin(sessionStore.role));

	const mqttQosOptions: { value: 0 | 1; label: string }[] = [
		{ value: 0, label: '0（At most once）' },
		{ value: 1, label: '1（At least once）' }
	];

	let mqttEnabled = $state(false);
	let mqttHost = $state('');
	let mqttPort = $state(1883);
	let mqttClientId = $state('banto-hub');
	let mqttUsername = $state('');
	let mqttPassword = $state('');
	let mqttPrefix = $state('banto');
	let mqttQos: 0 | 1 = $state(1);
	let mqttMinIntervalMs = $state(1000);

	let mqttLoaded = $state(false);
	let mqttSaving = $state(false);
	let mqttError: string | null = $state(null);
	let mqttConnected = $state(false);
	/**
	 * 監査③（2026-08-12）是正: 収集が停止中でない間は構成パッケージの
	 * インポートを実行させない UX ヒント用。5秒毎に `loadMqttStatus`
	 * 経由で更新される best-effort な値であり、権威ある判定ではない
	 * （権威ある判定は `handleApplyConfigPackage` 冒頭の再フェッチ）。
	 */
	let collectionState: string | null = $state(null);

	function applyMqttSettings(loaded: MqttSettings): void {
		mqttEnabled = loaded.enabled;
		mqttHost = loaded.host;
		mqttPort = loaded.port;
		mqttClientId = loaded.clientId;
		mqttUsername = loaded.username ?? '';
		mqttPrefix = loaded.prefix;
		mqttQos = loaded.qos === 0 ? 0 : 1;
		mqttMinIntervalMs = loaded.minIntervalMs;
		// password は常に空欄のまま(サーバーは返さない - このセクションの
		// doc comment参照)。
	}

	async function loadMqttStatus(): Promise<void> {
		try {
			const status = await getHubStatus();
			mqttConnected = status.mqtt.connected;
			collectionState = status.collection_state;
		} catch {
			// 状態表示だけの補助情報 - 取得失敗はエラー表示せず黙って保持する。
		}
	}

	$effect(() => {
		if (!canManageMqtt) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getMqttSettings();
				if (!cancelled) applyMqttSettings(loaded);
			} catch (err) {
				if (!cancelled) mqttError = errorMessage(err);
			} finally {
				if (!cancelled) mqttLoaded = true;
			}
			await loadMqttStatus();
		})();
		const interval = setInterval(() => void loadMqttStatus(), 5000);
		return () => {
			cancelled = true;
			clearInterval(interval);
		};
	});

	async function submitMqttSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		mqttError = null;
		mqttSaving = true;
		try {
			const saved = await saveMqttSettings({
				enabled: mqttEnabled,
				host: mqttHost,
				port: mqttPort,
				clientId: mqttClientId,
				username: mqttUsername.trim() === '' ? null : mqttUsername,
				password: mqttPassword,
				prefix: mqttPrefix,
				qos: mqttQos,
				minIntervalMs: mqttMinIntervalMs
			});
			applyMqttSettings(saved);
			mqttPassword = '';
			toastStore.push('success', 'MQTT 設定を保存しました(即時適用されます)');
			await loadMqttStatus();
		} catch (err) {
			mqttError = errorMessage(err);
		} finally {
			mqttSaving = false;
		}
	}

	// --- gRPC（T4、設計 §5.4、admin 限定） -----------------------------------
	//
	// 保存は `PUT /api/grpc-settings` を叩くだけで即時適用される(実装指示
	// 「保存で即時適用」- サーバー側が保存直後に `GrpcServer::apply` を
	// 呼ぶので、ここでは保存 API を呼んで結果を反映するだけでよい)。MQTT と
	// 違いパスワード等の秘匿情報を持たないため、フォームは常に現在値を
	// そのまま表示する。
	//
	// `bind`(2026-08-08 オーナー決定、docs/improvement-plan.md H3)は既定
	// `127.0.0.1` - `port` と同じく常にフォームの現在値をそのまま送る
	// (`grpcSettingsAdmin.ts`のdoc comment参照)。

	const canManageGrpc = $derived(isAdmin(sessionStore.role));

	let grpcEnabled = $state(false);
	let grpcBind = $state('127.0.0.1');
	let grpcPort = $state(50051);

	let grpcLoaded = $state(false);
	let grpcSaving = $state(false);
	let grpcError: string | null = $state(null);

	let configPackageFileEl: HTMLInputElement | undefined = $state();
	let configPackageLoaded = $state(false);
	let configPackageData: ConfigPackage | null = $state(null);
	let configPackageInspection: ConfigPackageInspection | null = $state(null);
	let configPackageLoadError: string | null = $state(null);
	let configPackageWorking = $state(false);
	let configPackageApplying = $state(false);
	let configPackageApplyError: string | null = $state(null);
	let configPackageImportSummary: ConfigPackageImportSummary | null = $state(null);
	let mqttImportUsername = $state('');
	let mqttImportPassword = $state('');

	/**
	 * 監査③（2026-08-12）是正: 収集稼働中の構成パッケージ import はサイレント
	 * に壊れる（`configPackageAdmin.ts` の doc comment参照）ため、
	 * `collectionState` が `'stopped'` 以外の間はボタンを無効化し警告文を
	 * 出す。best-effort な UX ヒントであり、権威ある判定は
	 * `handleApplyConfigPackage` 冒頭の再フェッチで行う。
	 */
	const importGuardActive = $derived(collectionState !== null && collectionState !== 'stopped');

	function applyGrpcSettings(loaded: GrpcSettings): void {
		grpcEnabled = loaded.enabled;
		grpcBind = loaded.bind;
		grpcPort = loaded.port;
	}

	$effect(() => {
		if (!canManageGrpc) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getGrpcSettings();
				if (!cancelled) applyGrpcSettings(loaded);
			} catch (err) {
				if (!cancelled) grpcError = errorMessage(err);
			} finally {
				if (!cancelled) grpcLoaded = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	async function submitGrpcSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		grpcError = null;
		grpcSaving = true;
		try {
			const saved = await saveGrpcSettings({
				enabled: grpcEnabled,
				bind: grpcBind,
				port: grpcPort
			});
			applyGrpcSettings(saved);
			toastStore.push('success', 'gRPC 設定を保存しました(即時適用されます)');
		} catch (err) {
			grpcError = errorMessage(err);
		} finally {
			grpcSaving = false;
		}
	}

	// --- 履歴の保持期間（T19 S2-d、docs/banto-hub-t19-design.md §5.1、UX-39、
	// admin 限定） -------------------------------------------------------------
	//
	// 2026-09-03 オーナー決定1: 「保存」ボタンは保持方針を**保存するだけ**
	// （次回の24時間周期タスク/再起動から自然に反映される・非破壊）。
	// 「今すぐ古い履歴を削除」は**別の**破壊的操作で、保存済みの方針で即時
	// 剪定する - `previewPrune`で件数を確認してから `window.confirm` で
	// 不可逆であることを明示し、OK のときだけ `pruneNow`を呼ぶ。
	// オーナー決定2: 「無制限（削除しない）」を選択肢として持つ。

	const canManageStore = $derived(isAdmin(sessionStore.role));

	const DEFAULT_RETENTION_DAYS_FALLBACK = 7;

	let storeRetentionForm: RetentionFormState = $state({
		unlimited: false,
		days: DEFAULT_RETENTION_DAYS_FALLBACK
	});
	/** 直近に `getStoreSettings`/保存成功で確定した`retentionDays` - `hasUnsavedRetentionChange`の基準。 */
	let savedRetentionDays: number | null = $state(null);

	let storeLoaded = $state(false);
	let storeSaving = $state(false);
	let storeError: string | null = $state(null);
	let pruning = $state(false);
	let pruneError: string | null = $state(null);

	const storeValidationError = $derived(validateRetentionForm(storeRetentionForm));
	const storeHasUnsavedChange = $derived(
		hasUnsavedRetentionChange(savedRetentionDays, storeRetentionForm)
	);
	const storePruneDisabledReason = $derived(pruneDisabledReason(storeHasUnsavedChange));

	$effect(() => {
		if (!canManageStore) return;
		let cancelled = false;
		(async () => {
			try {
				const loaded = await getStoreSettings();
				if (!cancelled) {
					savedRetentionDays = loaded.retentionDays;
					storeRetentionForm = retentionDaysToForm(
						loaded.retentionDays,
						DEFAULT_RETENTION_DAYS_FALLBACK
					);
				}
			} catch (err) {
				if (!cancelled) storeError = errorMessage(err);
			} finally {
				if (!cancelled) storeLoaded = true;
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	async function submitStoreSettings(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		storeError = null;
		if (storeValidationError) {
			storeError = storeValidationError;
			return;
		}
		storeSaving = true;
		try {
			const saved = await setStoreSettings({
				retentionDays: formToRetentionDays(storeRetentionForm)
			});
			savedRetentionDays = saved.retentionDays;
			storeRetentionForm = retentionDaysToForm(
				saved.retentionDays,
				DEFAULT_RETENTION_DAYS_FALLBACK
			);
			toastStore.push('success', formatRetentionSavedMessage());
		} catch (err) {
			storeError = errorMessage(err);
		} finally {
			storeSaving = false;
		}
	}

	async function handlePruneNow(): Promise<void> {
		pruneError = null;
		pruning = true;
		try {
			const preview = await previewPrune();
			if (preview.wouldDeleteCount === 0) {
				toastStore.push('info', '削除対象はありません');
				return;
			}
			if (!window.confirm(formatPruneConfirmMessage(preview.wouldDeleteCount))) {
				return;
			}
			const result = await pruneNow();
			toastStore.push('success', formatPruneDoneMessage(result.deletedCount));
		} catch (err) {
			pruneError = errorMessage(err);
		} finally {
			pruning = false;
		}
	}

	function resetConfigPackageImport(): void {
		configPackageData = null;
		configPackageInspection = null;
		configPackageLoadError = null;
		configPackageApplyError = null;
		configPackageImportSummary = null;
		mqttImportUsername = '';
		mqttImportPassword = '';
		if (configPackageFileEl) configPackageFileEl.value = '';
	}

	function configPackageExportFilename(): string {
		const now = new Date();
		const y = now.getFullYear();
		const m = String(now.getMonth() + 1).padStart(2, '0');
		const d = String(now.getDate()).padStart(2, '0');
		return `banto-hub-config-${y}-${m}-${d}.json`;
	}

	async function handleExportConfigPackage(): Promise<void> {
		configPackageWorking = true;
		try {
			const pkg = await loadConfigPackage();
			const blob = new Blob([serializeConfigPackage(pkg)], {
				type: 'application/json;charset=utf-8'
			});
			const url = URL.createObjectURL(blob);
			const a = document.createElement('a');
			a.href = url;
			a.download = configPackageExportFilename();
			a.click();
			URL.revokeObjectURL(url);
			toastStore.push('success', '構成パッケージをダウンロードしました');
		} catch (err) {
			configPackageLoadError = errorMessage(err);
		} finally {
			configPackageWorking = false;
		}
	}

	async function handleConfigPackageFileChange(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement | null;
		const file = input?.files?.[0];
		resetConfigPackageImport();
		if (!file) return;
		configPackageWorking = true;
		try {
			const text = await file.text();
			const pkg = parseConfigPackage(text);
			const inspection = await inspectConfigPackage(pkg);
			configPackageData = pkg;
			configPackageInspection = inspection;
			configPackageLoaded = true;
			mqttImportUsername = '';
			mqttImportPassword = '';
		} catch (err) {
			configPackageLoadError = errorMessage(err);
			configPackageLoaded = false;
		} finally {
			configPackageWorking = false;
		}
	}

	async function handleApplyConfigPackage(): Promise<void> {
		if (!configPackageData || !configPackageInspection) return;
		configPackageApplying = true;
		configPackageApplyError = null;
		configPackageImportSummary = null;
		try {
			// 監査③（2026-08-12）是正: 収集稼働中の import はサイレントに
			// 壊れる（configPackageAdmin.ts の doc comment参照）ため、実行
			// 直前に必ず最新の収集状態を取り直して確認する（`importGuardActive`
			// は5秒ポーリングの古い値かもしれず、権威ある判定には使えない）。
			const freshStatus = await getHubStatus();
			if (freshStatus.collection_state !== 'stopped') {
				configPackageApplyError = IMPORT_BLOCKED_MESSAGE;
				return;
			}
			const summary = await applyConfigPackage(configPackageData, {
				mqttUsername: mqttImportUsername.trim() === '' ? undefined : mqttImportUsername,
				mqttPassword: mqttImportPassword
			});
			configPackageImportSummary = summary;
			const [loadedMqtt, loadedGrpc] = await Promise.all([getMqttSettings(), getGrpcSettings()]);
			applyMqttSettings(loadedMqtt);
			applyGrpcSettings(loadedGrpc);
			await loadMqttStatus();
			toastStore.push('success', '構成パッケージを適用しました');
		} catch (err) {
			configPackageApplyError = errorMessage(err);
		} finally {
			configPackageApplying = false;
		}
	}
</script>

<div class="sections">
	<section>
		<h2>テーマ</h2>
		<div class="options" role="radiogroup" aria-label="テーマ">
			{#each modes as mode (mode.value)}
				<label class:selected={settings.themeMode === mode.value}>
					<input
						type="radio"
						name="theme"
						value={mode.value}
						checked={settings.themeMode === mode.value}
						onchange={() => settings.setThemeMode(mode.value)}
					/>
					{mode.label}
				</label>
			{/each}
		</div>

		<h3>プリセット</h3>
		<div class="options" role="radiogroup" aria-label="テーマプリセット">
			{#each presets as preset (preset.value)}
				<label class:selected={settings.themePreset === preset.value}>
					<input
						type="radio"
						name="theme-preset"
						value={preset.value}
						checked={settings.themePreset === preset.value}
						onchange={() => settings.setThemePreset(preset.value)}
					/>
					{preset.label}
				</label>
			{/each}
		</div>
		<p class="note">設定はこの端末に即時保存されます。</p>
	</section>

	<section>
		<h2>アカウント</h2>
		{#if changePassword}
			<form onsubmit={submitChangePassword}>
				<label class="field">
					現在のパスワード
					<input type="password" bind:value={currentPassword} autocomplete="current-password" />
				</label>
				<label class="field">
					新しいパスワード（8文字以上）
					<input type="password" bind:value={newPassword} autocomplete="new-password" />
				</label>
				<label class="field">
					新しいパスワード（確認）
					<input type="password" bind:value={newPasswordConfirm} autocomplete="new-password" />
				</label>

				{#if passwordError}
					<p class="error">{passwordError}</p>
				{/if}

				<button type="submit" disabled={changingPassword}>パスワードを変更</button>
			</form>
		{:else}
			<p class="note">この環境ではパスワード変更に対応していません。</p>
		{/if}
	</section>

	{#if sessionStore.commissioningMode}
		<section class="commissioning">
			<h2>試運転モードのロックダウン</h2>
			<p class="note">
				現在この環境は試運転モードです。認証なしで管理操作ができる状態のため、現場での試運転が
				終わったら運用開始前に必ずロックダウンしてください。<strong
					>ロックダウンは元に戻せません</strong
				>（UI からは試運転モードへ戻せません）。
			</p>

			{#if hasAdminAccount === false}
				<p class="error">{NO_ADMIN_MESSAGE}</p>
			{:else if adminCheckError}
				<p class="error">管理者アカウントの確認に失敗しました: {adminCheckError}</p>
			{/if}

			{#if lockDownError}
				<p class="error">{lockDownError}</p>
			{/if}

			<button
				type="button"
				class="danger"
				onclick={handleLockDown}
				disabled={lockingDown || hasAdminAccount !== true}
				title={lockDownDisabledReason ?? undefined}
			>
				{lockingDown ? 'ロックダウン中…' : 'ロックダウンを実行'}
			</button>
		</section>
	{/if}

	{#if canManageMqtt}
		<section>
			<h2>
				MQTT 発行
				<span class="status-pill" class:ok={mqttConnected} class:bad={!mqttConnected}>
					{mqttConnected ? '接続中' : '未接続'}
				</span>
			</h2>
			{#if mqttLoaded}
				<form onsubmit={submitMqttSettings}>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={mqttEnabled} />
						MQTT 発行を有効にする
					</label>

					<label class="field">
						ブローカーホスト
						<input type="text" bind:value={mqttHost} placeholder="例: 192.168.1.10" />
					</label>
					<label class="field">
						ポート
						<input type="number" min="1" max="65535" bind:value={mqttPort} />
					</label>
					<label class="field">
						クライアント ID
						<input type="text" bind:value={mqttClientId} />
					</label>
					<label class="field">
						ユーザー名（任意）
						<input type="text" bind:value={mqttUsername} autocomplete="off" />
					</label>
					<label class="field">
						パスワード（変更する場合のみ入力。空欄なら現在の値を維持）
						<input
							type="password"
							bind:value={mqttPassword}
							autocomplete="new-password"
							placeholder="変更しない場合は空欄のまま"
						/>
					</label>
					<label class="field">
						トピック prefix
						<input type="text" bind:value={mqttPrefix} />
					</label>

					<h3>QoS</h3>
					<div class="options" role="radiogroup" aria-label="MQTT QoS">
						{#each mqttQosOptions as option (option.value)}
							<label class:selected={mqttQos === option.value}>
								<input
									type="radio"
									name="mqtt-qos"
									checked={mqttQos === option.value}
									onchange={() => (mqttQos = option.value)}
								/>
								{option.label}
							</label>
						{/each}
					</div>

					<label class="field">
						最短発行間隔（ミリ秒）
						<input type="number" min="0" bind:value={mqttMinIntervalMs} />
					</label>

					{#if mqttError}
						<p class="error">{mqttError}</p>
					{/if}

					<button type="submit" disabled={mqttSaving}>保存(即時適用)</button>
					<p class="note">
						トピックは <code>{'{prefix}/{connection}/{group}/{tag}'}</code> の形式で発行されます。パスワードは
						サーバーに平文で保存されます(閉域 LAN 前提)。
					</p>
				</form>
			{:else}
				<p class="note">読み込み中...</p>
			{/if}
		</section>
	{/if}

	{#if canManageGrpc}
		<section>
			<h2>
				gRPC
				<span class="status-pill" class:ok={grpcEnabled} class:bad={!grpcEnabled}>
					{grpcEnabled ? '有効' : '無効'}
				</span>
			</h2>
			{#if grpcLoaded}
				<form onsubmit={submitGrpcSettings}>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={grpcEnabled} />
						gRPC サーバーを有効にする
					</label>

					<label class="field">
						Bind アドレス
						<input type="text" bind:value={grpcBind} placeholder="127.0.0.1" />
					</label>
					<p class="note">
						127.0.0.1 = このPCのみ(既定・推奨) / 0.0.0.0 = 全インターフェース(非推奨: TLS が
						無いため API キーが平文で LAN に流れます)。
					</p>

					<label class="field">
						ポート
						<input type="number" min="1" max="65535" bind:value={grpcPort} />
					</label>

					{#if grpcError}
						<p class="error">{grpcError}</p>
					{/if}

					<button type="submit" disabled={grpcSaving}>保存(即時適用)</button>
					<p class="note">
						REST/WebSocket とは別ポートで listen します(既定 50051)。無効化中はこのポートで一切
						listen しません。
					</p>
				</form>
			{:else}
				<p class="note">読み込み中...</p>
			{/if}
		</section>
	{/if}

	{#if canManageStore}
		<section>
			<h2>データ保持</h2>
			{#if storeLoaded}
				<form onsubmit={submitStoreSettings}>
					<label class="field checkbox">
						<input
							type="checkbox"
							checked={storeRetentionForm.unlimited}
							onchange={(e) =>
								(storeRetentionForm = {
									...storeRetentionForm,
									unlimited: (e.currentTarget as HTMLInputElement).checked
								})}
						/>
						無制限（削除しない）
					</label>

					<label class="field">
						保持日数
						<input
							type="number"
							min="1"
							max="3650"
							bind:value={storeRetentionForm.days}
							disabled={storeRetentionForm.unlimited}
						/>
					</label>

					{#if storeError}
						<p class="error">{storeError}</p>
					{/if}

					<button type="submit" disabled={storeSaving || storeValidationError !== null}>
						保存
					</button>
					<p class="note">
						保存すると次回の自動剪定（起動時 + 24時間ごと）から反映されます。保存だけでは既存の
						履歴は削除されません。
					</p>

					{#if pruneError}
						<p class="error">{pruneError}</p>
					{/if}

					<button
						type="button"
						class="danger"
						onclick={handlePruneNow}
						disabled={pruning || storeHasUnsavedChange}
						title={storePruneDisabledReason ?? undefined}
					>
						{pruning ? '削除中…' : '今すぐ古い履歴を削除'}
					</button>
					<p class="note">
						保存済みの保持方針に従って、保持期間を過ぎた履歴ファイルを今すぐ削除します。
						<strong>削除すると元に戻せません。</strong>
					</p>
				</form>
			{:else}
				<p class="note">読み込み中...</p>
			{/if}
		</section>
	{/if}

	{#if canManageMqtt || canManageGrpc}
		<section>
			<h2>構成パッケージ</h2>
			<p class="note">
				PLC 接続・収集グループ・タグ・MQTT / gRPC の非秘密設定を JSON で移送します。 MQTT
				の認証情報は含めないため、必要ならインポート時に再入力してください。
			</p>
			<div class="package-actions">
				<button type="button" onclick={handleExportConfigPackage} disabled={configPackageWorking}>
					JSON をダウンロード
				</button>
				<label class="field file-field">
					<span>JSON ファイル</span>
					<input
						type="file"
						accept=".json,application/json"
						bind:this={configPackageFileEl}
						onchange={handleConfigPackageFileChange}
						disabled={configPackageWorking}
					/>
				</label>
			</div>

			{#if configPackageLoadError}
				<p class="error">{configPackageLoadError}</p>
			{/if}

			{#if configPackageData && configPackageInspection}
				<div class="package-summary">
					<p class="note">
						schemaVersion={configPackageData.schemaVersion} / product={configPackageData.product} / exportedAt={configPackageData.exportedAt}
					</p>
					<p class="note">
						PLC 接続 {configPackageInspection.counts.plcConnections.create} 追加 /
						{configPackageInspection.counts.plcConnections.update} 更新、収集グループ
						{configPackageInspection.counts.collectionGroups.create} 追加 /
						{configPackageInspection.counts.collectionGroups.update} 更新、タグ
						{configPackageInspection.counts.tags.create} 追加 /
						{configPackageInspection.counts.tags.update} 更新。
					</p>
					<p class="note">
						MQTT は
						{configPackageInspection.mqttSettings.enabled ? '有効' : '無効'} / gRPC は {configPackageInspection
							.grpcSettings.enabled
							? '有効'
							: '無効'} です。
					</p>
					{#if configPackageInspection.mqttCredentialsRequired}
						<p class="note warning">
							MQTT
							のユーザー名・パスワードはパッケージに含まれません。必要なら下で再入力してください。
						</p>
					{/if}
					{#if configPackageInspection.warnings.length > 0}
						<ul class="warnings">
							{#each configPackageInspection.warnings as warning (warning)}
								<li>{warning}</li>
							{/each}
						</ul>
					{/if}

					<label class="field">
						MQTT ユーザー名（必要な場合のみ再入力）
						<input type="text" bind:value={mqttImportUsername} autocomplete="off" />
					</label>
					<label class="field">
						MQTT パスワード（必要な場合のみ再入力）
						<input type="password" bind:value={mqttImportPassword} autocomplete="new-password" />
					</label>

					{#if configPackageApplyError}
						<p class="error">{configPackageApplyError}</p>
					{/if}

					{#if configPackageImportSummary}
						<p class="note success">
							インポート完了: PLC 接続 {configPackageImportSummary.counts.plcConnections.create} 追加
							/
							{configPackageImportSummary.counts.plcConnections.update} 更新、収集グループ
							{configPackageImportSummary.counts.collectionGroups.create} 追加 /
							{configPackageImportSummary.counts.collectionGroups.update} 更新、タグ
							{configPackageImportSummary.counts.tags.create} 追加 /
							{configPackageImportSummary.counts.tags.update} 更新。
						</p>
					{/if}

					{#if importGuardActive}
						<p class="note warning">{IMPORT_BLOCKED_MESSAGE}</p>
					{/if}

					<button
						type="button"
						onclick={handleApplyConfigPackage}
						disabled={configPackageApplying || configPackageWorking || importGuardActive}
					>
						インポートを実行
					</button>
				</div>
			{:else if configPackageLoaded}
				<p class="note">JSON を読み込み中です...</p>
			{:else}
				<p class="note">JSON を選択すると、追加/更新件数と再入力項目を確認できます。</p>
			{/if}
		</section>
	{/if}
</div>

<style>
	.sections {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		max-width: 560px;
	}

	section {
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1rem 1.25rem;
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1rem;
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	h3 {
		margin: 1rem 0 0.5rem;
		font-size: 0.875rem;
		color: var(--banto-text-muted);
	}

	.options {
		display: flex;
		gap: 0.5rem;
	}

	.options label {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.45rem 0.8rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		cursor: pointer;
		font-size: 0.875rem;
	}

	.options label.selected {
		border-color: var(--banto-primary);
		color: var(--banto-primary);
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
	}

	.options input {
		position: absolute;
		opacity: 0;
		pointer-events: none;
	}

	section form {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-width: 320px;
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

	/* ロックダウンは不可逆な操作 - 通常の primary ボタンと見た目を分け、
	   誤って他の保存ボタンと同じ感覚で押させないようにする。 */
	button.danger {
		background: var(--banto-danger);
	}

	button.danger:hover:not(:disabled) {
		background: color-mix(in srgb, var(--banto-danger) 85%, black);
	}

	section.commissioning {
		border-color: var(--banto-warning, #8a5a00);
	}

	.error {
		margin: 0.5rem 0 0;
		color: var(--banto-danger);
		font-size: 0.8rem;
	}

	.note {
		margin: 0.75rem 0 0;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.note code {
		background: var(--banto-bg);
		border: 1px solid var(--banto-border);
		border-radius: 4px;
		padding: 0.05rem 0.3rem;
		font-size: 0.75rem;
	}

	.status-pill {
		font-size: 0.7rem;
		font-weight: 600;
		padding: 0.15rem 0.55rem;
		border-radius: 999px;
	}

	.status-pill.ok {
		color: var(--banto-success, #1a7f37);
		background: color-mix(in srgb, var(--banto-success, #1a7f37) 15%, transparent);
	}

	.status-pill.bad {
		color: var(--banto-text-muted);
		background: color-mix(in srgb, var(--banto-text-muted) 15%, transparent);
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 0.5rem;
	}

	.field.checkbox input {
		width: auto;
	}

	.package-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		align-items: end;
		margin: 0.75rem 0 0;
	}

	.file-field {
		min-width: 220px;
	}

	.package-summary {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		margin-top: 0.75rem;
	}

	.warnings {
		margin: 0;
		padding-left: 1.2rem;
		color: var(--banto-text-muted);
		font-size: 0.8rem;
	}

	.note.warning {
		color: var(--banto-warning, #8a5a00);
	}

	.note.success {
		color: var(--banto-success, #1a7f37);
	}
</style>
