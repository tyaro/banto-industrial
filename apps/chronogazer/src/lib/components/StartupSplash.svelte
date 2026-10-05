<script lang="ts">
	// banto v3.0.0（タグ v3.0.0 = f0dcece）の admin-template
	// `apps/admin-template/src/lib/components/StartupSplash.svelte` からコピー
	// （I2c、banto #286）。
	// chronogazer 固有の差: chronogazer は i18n（Paraglide）を持たないので、
	// 文言は admin-template の `messages/ja.json` の `app.starting` /
	// `app.startup.*` を日本語で直書きしている。それ以外は無改変。
	import { startupState, retryStartup } from '#lib/banto/startupState.svelte.js';
</script>

{#if startupState.status === 'unreachable'}
	<div class="banto-splash" role="alert">
		<h1>サーバーに接続できません</h1>
		<p>
			サーバーから応答がありませんでした。サーバーが起動しているか、ネットワークの接続を確認してから再試行してください。接続できるまで画面は開きません。
		</p>
		<button type="button" onclick={retryStartup}>再接続</button>
	</div>
{:else}
	<p class="banto-splash" role="status">起動中…</p>
{/if}

<style>
	.banto-splash {
		min-height: 100vh;
		display: grid;
		place-items: center;
		align-content: center;
		gap: 0.75rem;
		padding: 1rem;
		text-align: center;
		color: var(--banto-text-muted);
	}
	h1 {
		font-size: 1.125rem;
		color: var(--banto-text);
	}
	button {
		padding: 0.5rem 1.25rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text);
		cursor: pointer;
	}
</style>
