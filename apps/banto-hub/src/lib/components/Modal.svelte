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
	 * フォーカス可能要素へ移すフォーカストラップ（**2026-09-16 #381 で Tab /
	 * Shift+Tab のパネル内循環も両部品へ追加した** - 理由は `focusTrap.ts` と
	 * `Drawer.svelte` 冒頭の doc）、`aria-modal="true"` + `role="dialog"`。`Drawer.svelte` を直接再利用しなかった理由は、右固定・
	 * スライド・全高という見た目の性質が中央・可変高・フェード+スケールという
	 * このコンポーネントの性質と相容れず、共通化するとプレゼンテーション用の
	 * 分岐だらけになるため（`ConnectionDrawer.svelte`/
	 * `CollectionGroupDrawer.svelte` 冒頭コメントに合わせ、用途でコンポーネント
	 * 自体を分ける方針を踏襲）。
	 *
	 * 2026-09-15 追補（誤爆防止、TAG-UX-C 追補 - 「編集中に操作ミスで閉じて
	 * 入力が消える」事故対策）: `Drawer.svelte` と同じ `dirty`/`onBlockedClose`
	 * 契約をそのまま踏襲する - `dirty` が `true` の間は Esc・オーバーレイ
	 * クリックでは閉じない（`drawerCloseGuard.ts::isCloseAllowed`）。**`×`
	 * ボタン経由だけは塞がない** - 未保存確認は従来どおり `onRequestClose`
	 * に委ねたまま。3ステップの作成ウィザードでも同じロジックで効く
	 * （`open` が変わらない限りステップを跨いでも `dirty` は呼び出し側の
	 * baseline 比較に従う）。
	 */
	import type { Snippet } from 'svelte';
	import { tick, untrack } from 'svelte';
	import { fade, scale } from 'svelte/transition';
	import { isCloseAllowed } from './drawerCloseGuard';
	import { hasVisibleLayerAbove, LAYER_INACTIVE_ATTR } from './escLayering';
	import { handleTrapKeydown } from './focusTrap';
	import { restoreFocus } from './focusRestore';

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
		/**
		 * `Drawer.svelte` と同じ契約（そちらの doc コメント参照）: 未保存の
		 * 変更があるか。`true` の間は Esc とオーバーレイクリックでは閉じない
		 * （誤爆防止）。`×` の経路は塞がない。既定 `false`（後方互換）。
		 */
		dirty?: boolean;
		/** `Drawer.svelte` と同じ契約: `dirty` によりブロックされたことを呼び出し側へ知らせる。 */
		onBlockedClose?: () => void;
		/**
		 * #381 レビュー対応11回目: 閉じたときの**フォーカスの戻し先の代替**を返す
		 * （`SplitPane` の同名 prop と同じ形）。開いた元は閉じるまでに消えることが
		 * ある - 代表例が `TreeContextMenu` の項目から開いた場合で、メニューは項目を
		 * 選んだ直後にアンマウントされるため、閉じるころには戻し先が DOM に居ない。
		 * そのとき呼び出し側が「右クリックしたノード」等を返せるようにする。
		 * 未指定・`null` なら何もしない（`<body>` へは落とさない）。
		 */
		focusFallback?: () => HTMLElement | null | undefined;
		children?: Snippet;
	}

	let {
		open,
		title,
		width = '560px',
		closeOnOverlayClick = true,
		onclose,
		onRequestClose,
		dirty = false,
		onBlockedClose,
		focusFallback,
		children
	}: Props = $props();

	/** 自分自身のパネル（`role="dialog"`）。層の約束の「自分以外」の判定に使う。 */
	let panelEl: HTMLDivElement | undefined = $state();

	/**
	 * #381 レビュー対応12回目（層の約束・項目6）: **閉じる意思が固まってから実際に
	 * DOM から消えるまで**（`open` の反映待ち + outro の fade/fly）は「もう無い層」
	 * として扱う。この間、矩形も `visibility` も可視のままなので、印を付けないと
	 * 下の層が譲り続けて **Esc が無反応**になる。props の反映（＝再描画）は非同期で
	 * 直後の Esc に間に合わないため、`requestClose()` で**同期的に**属性も立てる。
	 */
	let closing = $state(false);

	/** `onRequestClose` 経由でクローズ可否を判定し、許可された場合だけ `onclose` を呼ぶ。 */
	function requestClose(): void {
		if (onRequestClose && !onRequestClose()) return;
		closing = true;
		panelEl?.setAttribute(LAYER_INACTIVE_ATTR, 'true');
		onclose?.();
	}

	function handleWindowKeydown(event: KeyboardEvent): void {
		// 閉じる処理が走った後（`open` の反映待ち・outro 中）は、もうこの層は
		// 無いものとして次の Esc を下の層へ渡す。
		if (open && !closing && event.key === 'Escape') {
			// 層の約束（`escLayering.ts` の doc が正、#381 レビュー対応5回目):
			// 自分より手前に別の層（コマンドパレット等）が出ていれば譲る
			// （`defaultPrevented` だけでは足りない理由は `Drawer.svelte` の同じ
			// ガードのコメント参照 - window リスナーの登録順の都合）。
			if (event.defaultPrevented || hasVisibleLayerAbove({ except: panelEl })) return;
			event.preventDefault();
			if (!isCloseAllowed('escape', dirty)) {
				onBlockedClose?.();
				return;
			}
			requestClose();
		}
	}

	// オーバーレイ自身への直接クリックだけを閉じるトリガにする
	// （バブリングで届いたモーダル内クリックと区別する）。`Drawer.svelte` と
	// 同じ理由 - a11y 的にクリックハンドラを持つ非インタラクティブ要素を
	// 増やさずに済む。
	function handleOverlayClick(event: MouseEvent): void {
		if (!closeOnOverlayClick || event.target !== event.currentTarget) return;
		if (!isCloseAllowed('overlay', dirty)) {
			onBlockedClose?.();
			return;
		}
		requestClose();
	}

	/**
	 * #381 レビュー対応10回目（層の約束・項目5、`escLayering.ts`）: 開く前に
	 * フォーカスがあった要素を覚えて、閉じたときに戻す。戻さないとフォーカスが
	 * `<body>` へ落ち、そこからの Tab は**どのパネルの keydown も通らない**ので
	 * 残っている層のトラップをすり抜ける。
	 *
	 * **捕捉は `$effect.pre`（DOM 更新の前）、戻しは `tick()` の後**（#381 レビュー
	 * 対応12回目）。捕捉が前なのは、通常の `$effect` だと `use:focusFirst` が先頭
	 * 要素へフォーカスを移した後になり開く前の要素が分からなくなるため。戻しを
	 * 後にするのは、**閉じるのと同じ更新で戻し先自体が消えることがある**ため
	 * （例: 編集 Drawer からタグを削除すると、`drawerMode` が消えるのと同じ更新で
	 * その行が一覧から外れる）。DOM 更新前に戻すと「まだ生きて見える行」へ戻して
	 * しまい、直後に消えてフォーカスが `<body>` へ落ちる - `restoreFocus` は
	 * `tick()` 後の DOM に対して生存判定するので、死んでいれば `focusFallback` へ
	 * 進める。**遷移したときだけ**動かすのは `SplitPane.svelte` と同じ。
	 */
	let triggerEl: HTMLElement | null = null;
	let openHandled = false;

	$effect.pre(() => {
		const isOpen = open;
		untrack(() => {
			if (isOpen === openHandled) return;
			openHandled = isOpen;
			if (isOpen) {
				closing = false;
				const active = document.activeElement;
				triggerEl = active instanceof HTMLElement ? active : null;
			} else {
				const previous = triggerEl;
				triggerEl = null;
				// 開いた元が死んでいれば呼び出し側の代替へ。代替にも同じ生存判定を
				// かけたいので `restoreFocus` を入れ子にする。
				void tick().then(() =>
					restoreFocus(previous, () => restoreFocus(focusFallback?.() ?? null))
				);
			}
		});
	});

	/**
	 * #381 レビュー対応8回目: 開いている間、Tab / Shift+Tab をパネル内で循環させる
	 * （`focusTrap.ts` - なぜ入れたかは同ファイルの doc）。リスナーは DOM に
	 * 属性を足さずに済むよう `$effect` で張る（`panelEl` は `{#if open}` の中の
	 * `bind:this` なので、開いた後にこの `$effect` が動く）。
	 */
	$effect(() => {
		if (!open) return;
		const node = panelEl;
		if (!node) return;
		const onKeydown = (event: KeyboardEvent): void => handleTrapKeydown(node, event);
		node.addEventListener('keydown', onKeydown);
		return () => node.removeEventListener('keydown', onKeydown);
	});

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
			bind:this={panelEl}
			role="dialog"
			data-layer-inactive={open && !closing ? undefined : 'true'}
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
