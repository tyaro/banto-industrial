<script lang="ts">
	/**
	 * T19 S1-b（UX-31、docs/banto-hub-t19-design.md §2・§3.2、2026-09-02
	 * オーナー決定「作成＝中央モーダル、編集＝右ペイン」）: 中央モーダル。
	 * 「作成フロー（ウィザード）」専用の汎用部品 - `Drawer.svelte`
	 * （右からのスライドオーバー、T13-1・既存の編集用）と用途を分ける。
	 *
	 * §3.2 の理由をそのまま踏襲する:「作成は前後関係を必要としない一方向の
	 * 作業なので、中央モーダルで集中させる。編集は一覧を見ながら直す作業
	 * なので、右ペインで並置した方が速い。同じ『ドロワー』実装を使い回さず、
	 * 用途で分ける。」
	 *
	 * **`Drawer.svelte` のフォーカス管理・二重発火防止・オーバーレイの
	 * 仕組みを無改変で踏襲する**（実装指示の制約）: `onRequestClose` 契約
	 * （`false` を返せば `onclose` を呼ばない - dirty フォーム破棄確認・busy
	 * 中クローズ抑止を呼び出し側に委ねる）、Esc・オーバーレイクリック・×の
	 * 三経路すべてが同じ `requestClose` を通る一本化、開いた直後に先頭の
	 * フォーカス可能要素へ移すフォーカストラップ、`aria-modal="true"` +
	 * `role="dialog"`。`Drawer.svelte` を直接再利用しなかった理由は、右固定・
	 * スライド・全高という見た目の性質が中央・可変高・フェード+スケールという
	 * このコンポーネントの性質と相容れず、共通化するとプレゼンテーション用の
	 * 分岐だらけになるため（`ConnectionDrawer.svelte`/
	 * `CollectionGroupDrawer.svelte` 冒頭コメントに合わせ、用途でコンポーネント
	 * 自体を分ける方針を踏襲）。
	 */
	import type { Snippet } from 'svelte';
	import { fade, scale } from 'svelte/transition';

	interface Props {
		open: boolean;
		title?: string;
		/** 既定 560px（CSS の任意の長さ文字列 — 例: '560px', '36rem'）。 */
		width?: string;
		/** オーバーレイクリックで閉じるか。既定 true。 */
		closeOnOverlayClick?: boolean;
		onclose?: () => void;
		/**
		 * `Drawer.svelte` と同じ契約: Esc・オーバーレイクリック・×のいずれで
		 * 閉じようとした場合も必ずこのフックを経由させ、戻り値が `true`
		 * （＝閉じてよい）のときだけ `onclose` を呼ぶ。`false` を返せば
		 * `onclose` は呼ばれずモーダルは開いたまま。未指定時は従来どおり
		 * 即 `onclose`（後方互換）。
		 */
		onRequestClose?: () => boolean;
		children?: Snippet;
	}

	let {
		open,
		title,
		width = '560px',
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
	// （バブリングで届いたモーダル内クリックと区別する）。`Drawer.svelte` と
	// 同じ理由 - a11y 的にクリックハンドラを持つ非インタラクティブ要素を
	// 増やさずに済む。
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
			class="modal"
			role="dialog"
			aria-modal="true"
			aria-label={title}
			style:width
			use:focusFirst
			transition:scale={{ start: 0.96, duration: 160 }}
		>
			<div class="modal-header">
				{#if title}<h3>{title}</h3>{/if}
				<button type="button" class="close" onclick={requestClose} aria-label="閉じる"> × </button>
			</div>
			<div class="modal-body">
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
		align-items: center;
		justify-content: center;
		padding: 1.5rem;
		background: rgba(0, 0, 0, 0.35);
	}

	.modal {
		display: flex;
		flex-direction: column;
		max-height: calc(100vh - 3rem);
		max-width: calc(100vw - 2rem);
		background: var(--banto-surface-raised, var(--banto-surface));
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius-lg, var(--banto-radius));
		box-shadow: 0 24px 48px rgba(0, 0, 0, 0.35);
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.modal-header {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 1rem 1.25rem;
		border-bottom: 1px solid var(--banto-border);
	}

	.modal-header h3 {
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

	.modal-body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 1rem 1.25rem 1.5rem;
	}
</style>
