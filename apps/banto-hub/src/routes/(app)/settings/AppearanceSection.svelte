<script lang="ts">
	/**
	 * 外観カテゴリ（テーマ・プリセット）（#359 段階1）。元 `+page.svelte` の
	 * 「テーマ」セクションから markup・state・`<style>` を無改変で移した
	 * だけで、挙動は変えない。常に表示（権限ガード無し）。
	 */
	import type { ThemeMode, ThemePreset } from '@banto/theme';
	import { settings } from '$lib/settings.svelte';

	const modes: { value: ThemeMode; label: string }[] = [
		{ value: 'light', label: 'ライト' },
		{ value: 'dark', label: 'ダーク' },
		{ value: 'system', label: 'システムに従う' }
	];

	const presets: { value: ThemePreset; label: string }[] = [
		{ value: 'standard', label: 'スタンダード' },
		{ value: 'glass', label: 'ガラス' }
	];
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
	<p class="note">設定はこの端末に即時保存されます。</p>
</section>
