<script lang="ts">
	/**
	 * 接続カテゴリ（LANアクセス＝組み込みWebサーバ）（#359 relay-wright 分）。
	 * 元 `+page.svelte` の「LANアクセス（組み込みWebサーバ）」セクションから
	 * markup・state・関数を無改変で移した。admin 限定（`+layout.ts` の
	 * `visible.connectivity` 参照）。
	 *
	 * `authSettings?.disabled`（ログイン不要モード有効中は使用不可の表示）は
	 * `authSettingsStore`（Account/Security とも共有、初回ロードは
	 * `settings/+layout.svelte` に集約済み）を直接読むだけで、この section
	 * 自身はフェッチしない - `authSettingsStore.svelte.ts` の doc comment
	 * 参照。
	 */
	import { isTauri } from '$lib/banto/setup';
	import { applyServerSettings, getServerStatus, type ServerStatus } from '$lib/banto/serverAdmin';
	import { sessionStore } from '$lib/session.svelte';
	import { isAdmin } from '$lib/permissions';
	import { authSettingsStore } from './authSettingsStore.svelte';

	const tauri = isTauri();

	let serverStatus = $state<ServerStatus | null>(null);
	let bindDraft = $state('127.0.0.1');
	let portDraft = $state(8721);
	let enabledDraft = $state(false);
	let applying = $state(false);
	let serverError: string | null = $state(null);

	function applyStatusToDrafts(status: ServerStatus): void {
		serverStatus = status;
		enabledDraft = status.enabled;
		bindDraft = status.bind;
		portDraft = status.port;
	}

	$effect(() => {
		if (!tauri) return;
		void (async () => {
			try {
				applyStatusToDrafts(await getServerStatus());
			} catch (err) {
				serverError = err instanceof Error ? err.message : String(err);
			}
		})();
	});

	async function saveAndApply(): Promise<void> {
		applying = true;
		serverError = null;
		try {
			applyStatusToDrafts(await applyServerSettings(enabledDraft, bindDraft, portDraft));
		} catch (err) {
			serverError = err instanceof Error ? err.message : String(err);
		} finally {
			applying = false;
		}
	}

	// The QR code shown is for the first LAN-reachable URL (i.e. not the
	// 127.0.0.1-only one) - that's the one another machine on the LAN would
	// actually need to scan; showing every URL's QR would just be noise.
	const firstLanUrl = $derived(
		serverStatus?.urls.find((url) => !url.includes('127.0.0.1')) ?? null
	);
	const firstLanQrSvg = $derived(
		firstLanUrl
			? (serverStatus?.qrSvgs.find((entry) => entry.url === firstLanUrl)?.svg ?? null)
			: null
	);
</script>

{#if isAdmin(sessionStore.role)}
	<section>
		<h2>LANアクセス（組み込みWebサーバ）</h2>
		{#if tauri}
			<label class="toggle" class:disabled={authSettingsStore.value?.disabled}>
				<input
					type="checkbox"
					bind:checked={enabledDraft}
					disabled={authSettingsStore.value?.disabled}
				/>
				LANアクセスを有効にする
			</label>
			{#if authSettingsStore.value?.disabled}
				<p class="note">ログイン不要モード有効中は使用できません。</p>
			{/if}

			<div class="server-fields">
				<label class="field">
					バインドアドレス
					<select bind:value={bindDraft}>
						<option value="127.0.0.1">127.0.0.1 のみ</option>
						<option value="0.0.0.0">0.0.0.0（LAN公開）</option>
					</select>
				</label>

				<label class="field">
					ポート番号
					<input type="number" min="1" max="65535" bind:value={portDraft} />
				</label>
			</div>

			<button type="button" onclick={saveAndApply} disabled={applying}>保存して適用</button>

			{#if serverError}
				<p class="error">{serverError}</p>
			{/if}

			{#if serverStatus}
				<p class="status">
					状態: <strong>{serverStatus.running ? '稼働中' : '停止中'}</strong>
				</p>
				{#if serverStatus.running}
					<ul class="urls">
						{#each serverStatus.urls as url (url)}
							<li><a href={url} target="_blank" rel="noreferrer">{url}</a></li>
						{/each}
					</ul>
					{#if firstLanQrSvg}
						<!-- Server-generated QR SVG (Rust `qrcode` crate), not user input. -->
						<!-- eslint-disable-next-line svelte/no-at-html-tags -->
						<div class="qr">{@html firstLanQrSvg}</div>
					{/if}
				{/if}
			{/if}
		{:else}
			<p class="note">サーバー設定はデスクトップアプリでのみ変更できます。</p>
		{/if}
		<p class="note">
			有効化すると、同一LAN内の他端末のブラウザからREST API + SSEで同じ画面を利用できます（仕様
			§11）。信頼できるLANでのみ有効にしてください。
		</p>
	</section>
{/if}

<style>
	.urls {
		margin: 0.4rem 0 0;
		padding-left: 1.2rem;
		font-size: 0.8rem;
	}

	.urls a {
		color: var(--banto-primary);
	}

	.qr {
		margin-top: 0.75rem;
		width: fit-content;
		/* Fixed white, not a --banto-* surface var: a QR code must stay
		   black-on-white to stay scannable in dark mode too. */
		background: #fff;
		padding: 0.5rem;
		border-radius: var(--banto-radius);
	}
</style>
