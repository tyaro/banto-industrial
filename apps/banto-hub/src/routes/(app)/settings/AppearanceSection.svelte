<script lang="ts">
	/**
	 * 外観カテゴリ（テーマ・プリセット）（#359 段階1）。元 `+page.svelte` の
	 * 「テーマ」セクションから markup・state・`<style>` を無改変で移した
	 * だけで、挙動は変えない。常に表示（権限ガード無し）。
	 *
	 * #1159（banto v7.2.0 のメニューバー）: 「ナビゲーション」（サイドバー / ドック /
	 * メニューバー、nav-launcher-plan.md §3・nav-menubar-plan.md §3）を足した。保存先は
	 * admin-template と同じキー。ドックを選んだときだけ位置（上下左右）を出す。
	 */
	import type { ThemeMode, ThemePreset } from '@banto/theme';
	import { settings, type LauncherPosition, type NavLayout } from '#lib/settings.svelte.js';

	const modes: { value: ThemeMode; label: string }[] = [
		{ value: 'light', label: 'ライト' },
		{ value: 'dark', label: 'ダーク' },
		{ value: 'system', label: 'システムに従う' }
	];

	const presets: { value: ThemePreset; label: string }[] = [
		{ value: 'standard', label: 'スタンダード' },
		{ value: 'glass', label: 'ガラス' }
	];

	const navLayouts: { value: NavLayout; label: string }[] = [
		{ value: 'menubar', label: 'メニューバー' },
		{ value: 'sidebar', label: 'サイドバー' },
		{ value: 'launcher', label: 'ドック' }
	];

	// ドックの画面の端。900px 以下は常に下のタブバー。
	const launcherPositions: { value: LauncherPosition; label: string }[] = [
		{ value: 'top', label: '上' },
		{ value: 'bottom', label: '下' },
		{ value: 'left', label: '左' },
		{ value: 'right', label: '右' }
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

	<h3>ナビゲーション</h3>
	<p class="note">
		ページへのリンクを、ヘッダーのメニューバー・左のサイドバー・画面の端のドックのどれで表示するか選びます。
	</p>
	<div class="options" role="radiogroup" aria-label="ナビゲーションの表示">
		{#each navLayouts as layout (layout.value)}
			<label class:selected={settings.navLayout === layout.value}>
				<input
					type="radio"
					name="nav-layout"
					value={layout.value}
					checked={settings.navLayout === layout.value}
					onchange={() => settings.setNavLayout(layout.value)}
				/>
				{layout.label}
			</label>
		{/each}
	</div>

	{#if settings.navLayout === 'launcher'}
		<h3>ドックの位置</h3>
		<div class="options" role="radiogroup" aria-label="ドックの位置">
			{#each launcherPositions as position (position.value)}
				<label class:selected={settings.launcherPosition === position.value}>
					<input
						type="radio"
						name="launcher-position"
						value={position.value}
						checked={settings.launcherPosition === position.value}
						onchange={() => settings.setLauncherPosition(position.value)}
					/>
					{position.label}
				</label>
			{/each}
		</div>
		<p class="note">900px 以下の幅では、位置にかかわらず下のタブバーになります。</p>
	{/if}

	<p class="note">設定はこの端末に即時保存されます。</p>
</section>
