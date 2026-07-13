<script lang="ts">
	import { goto } from '$app/navigation';
	import { BantoForm, createFormStore } from '@banto/forms';
	import type { FormSchema } from '@banto/forms';
	import { createFormResource, getResource } from '@banto/admin-core';

	const resource = getResource('items');
	const schema = resource.schema as FormSchema;

	const formResource = createFormResource('items');
	const store = createFormStore(schema);

	$effect(() => {
		void formResource.load();
	});

	async function handleSubmit(values: Record<string, unknown>) {
		const result = await formResource.submit(values);
		if (result.ok) {
			goto('/items');
		} else {
			store.setServerErrors(result.fieldErrors);
		}
	}
</script>

<div class="page">
	<h2>{resource.label}を新規作成</h2>

	{#if formResource.loading}
		<p class="loading">読み込み中…</p>
	{:else}
		<BantoForm {schema} {store} onSubmit={handleSubmit} submitting={formResource.saving} />
	{/if}
</div>

<style>
	.page {
		max-width: 480px;
		background: var(--banto-surface);
		border: 1px solid var(--banto-border);
		border-radius: calc(var(--banto-radius) * 2);
		padding: 1.25rem;
	}

	h2 {
		margin: 0 0 1rem;
		font-size: 1.1rem;
	}

	.loading {
		color: var(--banto-text-muted);
	}
</style>
