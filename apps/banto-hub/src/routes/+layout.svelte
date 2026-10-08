<script lang="ts">
	// relay-wright の同名ファイルから複製。
	import '../app.css';
	import { bantoReady } from '#lib/banto/setup.js'; // initBanto() (+ EventProvider) をどのルートガードより先に完了させる
	import { settings } from '#lib/settings.svelte.js';
	// トーストの表示は `@banto/ui` の ToastHost（banto #220 段階 3）。ストアは
	// アプリ全体で 1 つ（`#lib/toast.svelte.ts`）。
	import { ToastHost } from '@banto/ui';
	import { toastStore } from '#lib/toast.svelte.js';
	import { trackFirstNavigation } from '#lib/banto/navigationSettled.svelte.js';

	let { children } = $props();

	$effect(() => {
		settings.init();
	});

	// banto v4.0.0（#326）: このレイアウトは最初のナビゲーションで mount される
	// ので、その終わりをアプリ全体のために記録する（`trackFirstNavigation`）。
	// `(app)/+layout.svelte` の配線①が `isNavigationSettled()` で使う
	// （`#lib/banto/navigationSettled.svelte.ts`）。SvelteKit は最初の
	// ナビゲーションを `navigating` に出さないので、これが無いとその間を
	// 「ナビゲーション中でない」と読んでしまう。
	trackFirstNavigation();
</script>

{#await bantoReady}
	<p class="banto-splash">起動中…</p>
{:then}
	{@render children()}
	<ToastHost store={toastStore} />
{/await}

<style>
	.banto-splash {
		min-height: 100vh;
		display: grid;
		place-items: center;
		color: var(--banto-text-muted);
	}
</style>
