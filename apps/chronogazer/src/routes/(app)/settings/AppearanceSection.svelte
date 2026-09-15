<script lang="ts">
	/**
	 * 外観カテゴリ（テーマ/プリセット/ウィンドウ効果）（#359 chronogazer 分）。
	 * 元 `+page.svelte` の該当セクションから markup・state・関数を無改変で
	 * 移した。常に表示（テーマ/プリセット自体にガード無し。ウィンドウ効果は
	 * `tauri && isAdmin && vibrancyStatus?.supported` のときだけ内部で描画）。
	 */
	import type { ThemeMode, ThemePreset } from '@banto/theme';
	import { settings } from '$lib/settings.svelte';
	import { isTauri } from '$lib/banto/setup';
	import { applyVibrancy, getVibrancyStatus, type VibrancyStatus } from '$lib/banto/vibrancy';
	import { toastStore } from '$lib/toast.svelte';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { errorMessage } from './shared';

	const modes: { value: ThemeMode; label: string }[] = [
		{ value: 'light', label: 'ライト' },
		{ value: 'dark', label: 'ダーク' },
		{ value: 'system', label: 'システムに従う' }
	];

	// M12 preset axis (standard/glass), orthogonal to light/dark above.
	const presets: { value: ThemePreset; label: string }[] = [
		{ value: 'standard', label: 'スタンダード' },
		{ value: 'glass', label: 'ガラス' }
	];

	// M6 Phase B (spec §11.4): the server controls only exist inside the Tauri
	// webview - a LAN browser client has nothing here to configure (it IS the
	// remote side of this same server). Decided once per page load; isTauri()
	// never changes at runtime.
	const tauri = isTauri();

	// --- M12: window vibrancy (Tauri only, admin only, Windows only) --------
	// The whole section renders only when `vibrancy_status()` reports
	// `supported: true` (spec §11.3: capability-hide, don't grey out).
	let vibrancyStatus = $state<VibrancyStatus | null>(null);
	let applyingVibrancy = $state(false);

	$effect(() => {
		if (!tauri || !isAdmin(sessionStore.role)) return;
		void (async () => {
			try {
				vibrancyStatus = await getVibrancyStatus();
			} catch {
				// An older backend without the command (Phase A not deployed
				// yet) or any failure: keep the section hidden, never broken.
				vibrancyStatus = null;
			}
		})();
	});

	async function toggleVibrancy(event: Event): Promise<void> {
		const input = event.currentTarget as HTMLInputElement;
		const next = input.checked;
		applyingVibrancy = true;
		try {
			const enabled = await applyVibrancy(next);
			if (vibrancyStatus) vibrancyStatus = { ...vibrancyStatus, enabled };
		} catch (err) {
			toastStore.push('error', errorMessage(err));
			input.checked = vibrancyStatus?.enabled ?? false;
		} finally {
			applyingVibrancy = false;
		}
	}
</script>

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
	<p class="note">
		設定はこの端末に即時保存され、ログイン中は設定DB（Tauri/LANサーバ）にも保存されて他クライアントと共有されます（仕様
		§12.1 / M12）。
	</p>
</section>

{#if tauri && isAdmin(sessionStore.role) && vibrancyStatus?.supported}
	<section>
		<h2>ウィンドウ効果</h2>
		<label class="toggle">
			<input
				type="checkbox"
				checked={vibrancyStatus.enabled}
				disabled={applyingVibrancy}
				onchange={toggleVibrancy}
			/>
			ウィンドウのアクリル効果（Windows）
		</label>
		<p class="note">
			ウィンドウ背面を OS
			のアクリル（すりガラス）効果で描画します。ガラスプリセットと組み合わせると、デスクトップが透ける本物のガラス感になります（M12、Windows
			のみ）。
		</p>
	</section>
{/if}

<style>
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
</style>
