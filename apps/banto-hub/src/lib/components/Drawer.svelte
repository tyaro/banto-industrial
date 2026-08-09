<script lang="ts">
	/**
	 * 汎用部品（T13-1、docs/ux-plan.md §4b）: 右からのスライドオーバー。
	 * アプリ固有の結合を持たない — `src/lib/components/` 内で完結し、将来
	 * `@banto/*` へ昇格しやすいよう banto-hub の型・ストアを一切 import
	 * しない（呼び出し側が `open`/`title`/`children` を渡すだけの純表示部品）。
	 *
	 * フォーカストラップは「開いたら先頭要素へフォーカス」の最低限のみ
	 * （設計指示: 凝りすぎない範囲で）。Tab キーでのフォーカス循環制御は
	 * 行わない — 必要になったら需要を見て追加する。
	 */
	import type { Snippet } from 'svelte';
	import { fade, fly } from 'svelte/transition';

	interface Props {
		open: boolean;
		title?: string;
		/** 既定 480px（CSS の任意の長さ文字列 — 例: '480px', '36rem'）。 */
		width?: string;
		/** オーバーレイクリックで閉じるか。既定 true。 */
		closeOnOverlayClick?: boolean;
		onclose?: () => void;
		/**
		 * T18-1（TAG-UX-C 一部、docs/banto-hub-desktop-plan.md §9.4）:
		 * Esc・オーバーレイクリック・`×` のいずれで閉じようとした場合も
		 * 必ずこのフックを経由させ、戻り値が `true`（＝閉じてよい）の
		 * ときだけ `onclose` を呼ぶ。`false` を返せば `onclose` は呼ばれず
		 * Drawer は開いたまま — dirty フォームの破棄確認や busy 中の
		 * クローズ抑止を呼び出し側（`tags/+page.svelte` 等）に委ねる。
		 * 未指定時は従来どおり即 `onclose`（後方互換）。
		 */
		onRequestClose?: () => boolean;
		children?: Snippet;
	}

	let {
		open,
		title,
		width = '480px',
		closeOnOverlayClick = true,
		onclose,
		onRequestClose,
		children
	}: Props = $props();

	/** `onRequestClose` 経由でクローズ可否を判定し、許可された場合だけ `onclose` を呼ぶ。 */
	function requestClose(): void {
		if (onRequestClose && !onRequestClose()) return;
		onclose?.();
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		if (open && event.key === 'Escape') {
			event.preventDefault();
			requestClose();
		}
	}

	// オーバーレイ自身への直接クリックだけを閉じるトリガにする
	// （バブリングで届いたドロワー内クリックと区別する）。ドロワー側に
	// `stopPropagation` の click ハンドラを付けずに済むので、a11y 的に
	// クリックハンドラを持つ非インタラクティブ要素が増えない。
	function handleOverlayClick(event: MouseEvent): void {
		if (closeOnOverlayClick && event.target === event.currentTarget) requestClose();
	}

	/** 開いた直後、パネル内の最初のフォーカス可能要素へフォーカスする。 */
	function focusFirst(node: HTMLElement): void {
		const focusable = node.querySelector<HTMLElement>(
			'input, select, textarea, button, a[href], [tabindex]:not([tabindex="-1"])'
		);
		(focusable ?? node).focus();
	}
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#if open}
	<div
		class="overlay"
		role="presentation"
		onclick={handleOverlayClick}
		transition:fade={{ duration: 120 }}
	>
		<div
			class="drawer"
			role="dialog"
			aria-modal="true"
			aria-label={title}
			style:width
			use:focusFirst
			transition:fly={{ x: 48, duration: 160 }}
		>
			<div class="drawer-header">
				{#if title}<h3>{title}</h3>{/if}
				<button type="button" class="close" onclick={requestClose} aria-label="閉じる"> × </button>
			</div>
			<div class="drawer-body">
				{@render children?.()}
			</div>
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		z-index: 900;
		display: flex;
		justify-content: flex-end;
		background: rgba(0, 0, 0, 0.35);
	}

	.drawer {
		display: flex;
		flex-direction: column;
		height: 100%;
		max-width: calc(100vw - 2rem);
		background: var(--banto-surface-raised, var(--banto-surface));
		border-left: 1px solid var(--banto-border);
		box-shadow: -12px 0 32px rgba(0, 0, 0, 0.25);
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.drawer-header {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 1rem 1.25rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.drawer-header h3 {
		margin: 0;
		font-size: 1rem;
	}

	.close {
		border: none;
		background: none;
		color: var(--banto-text-muted);
		font-size: 1.25rem;
		line-height: 1;
		padding: 0.15rem 0.4rem;
		cursor: pointer;
		border-radius: var(--banto-radius);
	}

	.close:hover {
		color: var(--banto-text);
		background: color-mix(in srgb, var(--banto-primary) 8%, transparent);
	}

	.drawer-body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 1rem 1.25rem 1.5rem;
	}
</style>
