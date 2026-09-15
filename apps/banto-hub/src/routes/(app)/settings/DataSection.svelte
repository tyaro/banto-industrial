<script lang="ts">
	/**
	 * データカテゴリ（データ保持・構成の保存/読み込み＝バックアップ）
	 * （#359 段階1）。元 `+page.svelte` の「データ保持」「構成の保存・
	 * 読み込み（バックアップ）」セクションから markup・state・関数を無改変で
	 * 移した。いずれも admin 限定（T19 S2-d・T19 S4）。
	 *
	 * 構成パッケージの import 成功後、ConnectivitySection が保有していた
	 * MQTT/gRPC フォームの値を最新化する必要がある（元 `+page.svelte` の
	 * `applyMqttSettings`/`applyGrpcSettings`/`loadMqttStatus` 直接呼び出し）。
	 * section 分割後は `mqttSettingsStore`/`grpcSettingsStore`/
	 * `hubStatusStore`（2つ以上の section が参照する共有 state）の `load()`
	 * を直接 await することで同じ順序・同じタイミングを保つ。
	 */
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';
	import { toastStore } from '$lib/toast.svelte';
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
		exportConfigPackageToDownload
	} from '$lib/banto/configPackageAdmin';
	import {
		parseConfigPackage,
		type ConfigPackage,
		type ConfigPackageInspection,
		type ConfigPackageImportSummary
	} from '$lib/banto/configPackage';
	import { errorMessage } from './shared';
	import { mqttSettingsStore } from './mqttSettingsStore.svelte';
	import { grpcSettingsStore } from './grpcSettingsStore.svelte';
	import { hubStatusStore } from './hubStatusStore.svelte';

	const IMPORT_BLOCKED_MESSAGE = '構成パッケージの取り込みは収集を停止してから実行してください';

	/**
	 * T19 S4（UX-42）: コマンドパレットの `config.import` は構成パッケージの
	 * セクションへ誘導する（段階1では `/settings#config-package`、段階2では
	 * `/settings/data#config-package` - `commands.ts` 参照）。SvelteKit は
	 * ナビゲーション完了時に URL のハッシュに一致する id 要素へ自動で
	 * スクロールする実装を持つが、同一ページ内（既にこのページを開いている
	 * 状態でパレットからハッシュだけ変えて再実行する）ケースで確実に効くとは
	 * 限らないため、`onMount`/`afterNavigate` の両方で明示的に
	 * `scrollIntoView()` する最小のフォールバックを入れておく。
	 */
	function scrollToConfigPackageIfHashed(): void {
		if (typeof location === 'undefined' || location.hash !== '#config-package') return;
		document.getElementById('config-package')?.scrollIntoView();
	}

	onMount(scrollToConfigPackageIfHashed);
	afterNavigate(scrollToConfigPackageIfHashed);

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

	// --- 構成の保存・読み込み（バックアップ）（T19 S4、UX-42、admin 限定） ----

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
	 * `hubStatusStore.collectionState` が `'stopped'` 以外の間はボタンを
	 * 無効化し警告文を出す。best-effort な UX ヒントであり、権威ある判定は
	 * `handleApplyConfigPackage` 冒頭の再フェッチで行う。
	 */
	const importGuardActive = $derived(
		hubStatusStore.collectionState !== null && hubStatusStore.collectionState !== 'stopped'
	);

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

	/**
	 * T19 S4（UX-42）: export の中核（load → serialize → ダウンロード）は
	 * コマンドパレットからも呼べるよう `configPackageAdmin.ts` の
	 * `exportConfigPackageToDownload()` へ切り出した。ここではトースト表示
	 * とエラー state（`configPackageLoadError`）だけを担う - 挙動は従来と
	 * 同じ。
	 */
	async function handleExportConfigPackage(): Promise<void> {
		configPackageWorking = true;
		try {
			await exportConfigPackageToDownload();
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
			const freshStatus = await hubStatusStore.load();
			if (freshStatus.collection_state !== 'stopped') {
				configPackageApplyError = IMPORT_BLOCKED_MESSAGE;
				return;
			}
			const summary = await applyConfigPackage(configPackageData, {
				mqttUsername: mqttImportUsername.trim() === '' ? undefined : mqttImportUsername,
				mqttPassword: mqttImportPassword
			});
			configPackageImportSummary = summary;
			await Promise.all([mqttSettingsStore.load(), grpcSettingsStore.load()]);
			await hubStatusStore.load();
			toastStore.push('success', '構成パッケージを適用しました');
		} catch (err) {
			configPackageApplyError = errorMessage(err);
		} finally {
			configPackageApplying = false;
		}
	}
</script>

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

{#if canManageStore}
	<section id="config-package">
		<h2>構成の保存・読み込み（バックアップ）</h2>
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
					{configPackageInspection.counts.tags.update} 更新、sink group
					{configPackageInspection.counts.sinkGroups.create} 追加 /
					{configPackageInspection.counts.sinkGroups.update} 更新。
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
				{#if configPackageInspection.dbConnectionsPasswordRequired.length > 0}
					<p class="note warning">
						以下の PostgreSQL（DB Source）接続はパスワードを含みません（新規作成、または
						既存接続でパスワード未設定のもの）。インポート後にパスワードの再設定が必要です:
						{configPackageInspection.dbConnectionsPasswordRequired.join('、')}
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
						インポート完了: PLC 接続 {configPackageImportSummary.counts.plcConnections.create} 追加 /
						{configPackageImportSummary.counts.plcConnections.update} 更新、収集グループ
						{configPackageImportSummary.counts.collectionGroups.create} 追加 /
						{configPackageImportSummary.counts.collectionGroups.update} 更新、タグ
						{configPackageImportSummary.counts.tags.create} 追加 /
						{configPackageImportSummary.counts.tags.update} 更新、sink group
						{configPackageImportSummary.counts.sinkGroups.create} 追加 /
						{configPackageImportSummary.counts.sinkGroups.update} 更新。
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

<style>
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
