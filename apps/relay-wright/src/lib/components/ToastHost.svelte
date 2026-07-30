<script lang="ts">
	/** Fixed bottom-right toast stack (spec §3.4 notification sink), mounted once in the root layout. */
	import { toastStore } from '$lib/toast.svelte';
</script>

<div class="toast-host" role="status" aria-live="polite">
	{#each toastStore.toasts as toast (toast.id)}
		<div class="toast {toast.kind}">
			<span class="message">{toast.message}</span>
			<button
				type="button"
				class="close"
				onclick={() => toastStore.dismiss(toast.id)}
				aria-label="閉じる"
			>
				×
			</button>
		</div>
	{/each}
</div>

<style>
	.toast-host {
		position: fixed;
		right: 1rem;
		bottom: 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		z-index: 1000;
		max-width: 320px;
	}

	.toast {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.6rem 0.75rem;
		border-radius: var(--banto-radius);
		background: var(--banto-surface-raised);
		border: 1px solid var(--banto-border);
		border-left-width: 4px;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
		font-size: 0.85rem;
		color: var(--banto-text);
	}

	.toast.success {
		border-left-color: var(--banto-success);
	}

	.toast.error {
		border-left-color: var(--banto-danger);
	}

	.toast.info {
		border-left-color: var(--banto-primary);
	}

	.message {
		flex: 1;
	}

	.close {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		cursor: pointer;
		font-size: 1rem;
		line-height: 1;
		padding: 0;
	}

	.close:hover {
		color: var(--banto-text);
	}
</style>
