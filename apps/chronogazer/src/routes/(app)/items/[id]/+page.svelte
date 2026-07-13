<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { BantoForm, createFormStore } from '@banto/forms';
	import type { FormSchema } from '@banto/forms';
	import { createFormResource, getResource, isProviderError } from '@banto/admin-core';
	import { sessionStore } from '$lib/session.svelte';
	import { canWriteResources } from '$lib/permissions';

	const resource = getResource('items');
	const schema = resource.schema as FormSchema;

	// Spec M10 RBAC: `viewer` can still open this page to look at a record
	// (row-click navigation to it is allowed, per the items list page), but
	// may not save or delete. `BantoForm`'s `submitting` prop disables every
	// field AND the submit button together (there is no separate "read-only"
	// prop) - reusing it here for `!canWrite` doubles as "make the whole form
	// read-only", which is a fine RBAC outcome, not just an incidental side
	// effect: a viewer can't usefully edit fields it can never save anyway.
	const canWrite = $derived(canWriteResources(sessionStore.role));
	// SvelteKit creates a fresh component instance per [id] value, so reading
	// the param once at setup time is enough (no need for $derived here).
	//
	// Rust's items_get/items_update/items_delete commands declare `id: i64`
	// (apps/admin-template/src-tauri/src/lib.rs); Tauri's serde deserializer
	// does NOT coerce a JSON string into a number, so the raw route param
	// (always a string) must be converted to a real `number` before it ever
	// reaches createFormResource/DataProvider. A param that isn't a valid
	// integer (non-numeric, empty, fractional, ...) can never be a real item
	// id, so it's treated as not-found immediately - createFormResource/load
	// is never even called for it.
	const rawId = page.params.id ?? '';
	const parsedId = Number(rawId);
	const idValid = rawId !== '' && Number.isInteger(parsedId);

	const formResource = idValid ? createFormResource(resource.name, parsedId) : null;
	let store = $state(createFormStore(schema));
	let storeReady = $state(false);

	// Shared by the initial mount effect and the "reload" action below (Fix:
	// a transient/storage error used to be rendered as the generic
	// resource-not-found copy, indistinguishable from a genuinely missing
	// id; a `not_found` ProviderError is the only case that should show that
	// message - anything else gets its own message plus a way to retry
	// without a full page navigation).
	async function loadForm() {
		if (!formResource) return;
		await formResource.load();
		if (formResource.initialValues) {
			store = createFormStore(schema, formResource.initialValues);
			storeReady = true;
		}
	}

	$effect(() => {
		void loadForm();
	});

	const isNotFoundError = $derived.by(() => {
		if (!idValid) return true;
		const err = formResource?.error;
		return isProviderError(err) && err.body.kind === 'not_found';
	});

	async function handleSubmit(values: Record<string, unknown>) {
		// Defense in depth (spec M10 RBAC): the submit button is disabled for
		// `!canWrite` via BantoForm's `submitting` prop above, but the backend
		// is the real enforcement point either way (a `viewer` calling
		// items_update/PUT gets `BantoError::Forbidden`) - this guard just
		// avoids a pointless round trip.
		if (!formResource || !canWrite) return;
		const result = await formResource.submit(values);
		if (result.ok) {
			goto('/items');
		} else {
			store.setServerErrors(result.fieldErrors);
		}
	}

	async function handleDelete() {
		if (!formResource || !canWrite) return;
		if (!window.confirm('削除しますか？')) return;
		const removed = await formResource.remove();
		if (removed) goto('/items');
	}
</script>

<div class="page">
	<h2>{resource.label}を編集</h2>

	{#if isNotFoundError}
		<p class="not-found">
			{resource.label}が見つかりません。<a href="/items">一覧へ戻る</a>
		</p>
	{:else if formResource?.loading}
		<p class="loading">読み込み中…</p>
	{:else if formResource?.error}
		<p class="load-error">
			読み込みに失敗しました。
			<button type="button" class="reload" onclick={() => void loadForm()}>再読み込み</button>
			<a href="/items">一覧へ戻る</a>
		</p>
	{:else if storeReady}
		<BantoForm
			{schema}
			{store}
			onSubmit={handleSubmit}
			submitting={(formResource?.saving ?? false) || !canWrite}
		>
			{#if canWrite}
				<button type="button" class="delete" onclick={handleDelete}>削除</button>
			{/if}
		</BantoForm>
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

	.not-found {
		color: var(--banto-text-muted);
	}

	.not-found a {
		color: var(--banto-primary);
	}

	.load-error {
		color: var(--banto-text-muted);
	}

	.load-error a {
		color: var(--banto-primary);
	}

	.reload {
		padding: 0.15rem 0.6rem;
		margin: 0 0.4rem;
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-text);
		cursor: pointer;
	}

	.reload:hover {
		background: color-mix(in srgb, var(--banto-primary) 10%, transparent);
	}

	.delete {
		padding: 0.55rem 1rem;
		border: 1px solid var(--banto-danger);
		border-radius: var(--banto-radius);
		background: transparent;
		color: var(--banto-danger);
		font-weight: 600;
		cursor: pointer;
	}

	.delete:hover {
		background: color-mix(in srgb, var(--banto-danger) 10%, transparent);
	}
</style>
