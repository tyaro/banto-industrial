<script lang="ts">
	// relay-wright の同名ファイルから無改変で複製。
	import Header from '$lib/components/Header.svelte';
	import Sidebar from '$lib/components/Sidebar.svelte';
	import CommandPalette from '$lib/components/CommandPalette.svelte';
	import CommissioningBanner from '$lib/components/CommissioningBanner.svelte';
	import { listPendingChanges } from '$lib/banto/pendingChangesAdmin';
	import { countUnappliedPendingChanges } from '$lib/banto/pendingUnappliedCount';
	import { commandPaletteStore } from '$lib/commandPalette.svelte';
	import { isAdmin } from '$lib/permissions';
	import { sessionStore } from '$lib/session.svelte';

	let { children } = $props();
	let pendingCount = $state(0);

	const POLL_INTERVAL_MS = 3000;
	const hubAdmin = $derived(isAdmin(sessionStore.role));

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key.toLowerCase() === 'k' && (event.ctrlKey || event.metaKey)) {
			event.preventDefault();
			commandPaletteStore.toggle();
		}
	}

	$effect(() => {
		if (!hubAdmin) {
			pendingCount = 0;
			return;
		}

		let cancelled = false;

		async function pollPendingCount(): Promise<void> {
			try {
				const pendingChanges = await listPendingChanges();
				if (!cancelled) {
					// 実機で発見された不具合(2026-08-31、オーナー報告): 全件を
					// 数えると applied/canceled/failed も「未適用」に含めてし
					// まう（4件中3件キャンセル済み・1件適用済みなのに「未適用
					// 4件」と表示された）。未適用として数える state の判断は
					// `pendingUnappliedCount.ts` を参照。
					pendingCount = countUnappliedPendingChanges(pendingChanges);
				}
			} catch {
				// 常時表示用の補助ポーリングなので、失敗時は静かに無視する。
			}
		}

		void pollPendingCount();
		const timer = setInterval(() => void pollPendingCount(), POLL_INTERVAL_MS);

		return () => {
			cancelled = true;
			clearInterval(timer);
		};
	});
</script>

<svelte:window onkeydown={handleKeydown} />

<CommissioningBanner />

<div class="shell">
	<Sidebar {pendingCount} />
	<div class="main">
		<Header {pendingCount} />
		<main>
			{@render children()}
		</main>
	</div>
</div>

{#if commandPaletteStore.open}
	<CommandPalette />
{/if}

<style>
	.shell {
		display: flex;
		min-height: 100vh;
	}

	.main {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	main {
		flex: 1;
		padding: 1.25rem;
	}
</style>
