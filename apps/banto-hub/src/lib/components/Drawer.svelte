<script lang="ts">
	/**
	 * 汎用部品（T13-1、docs/ux-plan.md §4b）: 右からのスライドオーバー。
	 * アプリ固有の結合を持たない — `src/lib/components/` 内で完結し、将来
	 * `@banto/*` へ昇格しやすいよう banto-hub の型・ストアを一切 import
	 * しない（呼び出し側が `open`/`title`/`children` を渡すだけの純表示部品）。
	 *
	 * フォーカストラップは当初「開いたら先頭要素へフォーカス」の最低限のみで、
	 * Tab キーの循環制御は「需要を見て追加する」としていた。
	 * **2026-09-16（#381）にその需要が出たので追加した**（実装は `focusTrap.ts`）:
	 * フォーカスがパネルの外へ出られると、**オーバーレイの裏にある起動ボタンへ
	 * Tab で到達して別の層を開けてしまい**（接続 Drawer を開いたまま「収集
	 * グループを追加」に届く等）、「同じ z 順の層が2つ開いて Esc がどちらも
	 * 効かない」「未保存の入力を捨てずにどう排他するか」という症状が連鎖して
	 * 出ていた（#381 レビュー5〜8回目）。入口を塞ぐのが根本対策。
	 *
	 * 2026-09-15 追補（誤爆防止、TAG-UX-C 追補 - 「編集中に操作ミスで閉じて
	 * 入力が消える」事故対策）: `dirty` prop が `true` の間は Esc・オーバー
	 * レイクリックでは閉じない（`drawerCloseGuard.ts::isCloseAllowed`）。
	 * **`×` ボタン経由だけは塞がない** - 未保存確認は従来どおり
	 * `onRequestClose` に委ねたままで、`dirty` は「誤爆しやすい2経路だけを
	 * 事前に殺す」ためのもの。ブロックされたことは `onBlockedClose` で
	 * 呼び出し側へ伝える（トースト等の案内は呼び出し側の責務 - 本コンポーネ
	 * ントは banto-hub の型・ストアを import しない規約のため）。
	 */
	import type { Snippet } from 'svelte';
	import { tick, untrack } from 'svelte';
	import { fade, fly } from 'svelte/transition';
	import { isCloseAllowed } from './drawerCloseGuard';
	import { hasVisibleLayerAbove, LAYER_INACTIVE_ATTR } from './escLayering';
	import { handleTrapKeydown } from './focusTrap';
	import { restoreFocus } from './focusRestore';

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
		/**
		 * 2026-09-15 追補（誤爆防止、TAG-UX-C 追補）: 未保存の変更があるか。
		 * `true` の間は **Esc とオーバーレイクリックでは閉じない**
		 * （`drawerCloseGuard.ts::isCloseAllowed` 参照）。閉じるのは `×`
		 * ボタン経由だけになり、そこでは従来どおり `onRequestClose` の確認が
		 * 走る（`×` の経路自体は変えない）。既定 `false`（従来どおり全経路で
		 * 閉じる - 後方互換）。
		 */
		dirty?: boolean;
		/**
		 * `dirty` が `true` のときに Esc またはオーバーレイクリックで閉じようと
		 * した（＝ブロックされた）ことを呼び出し側へ知らせるコールバック。
		 * 未保存のときに操作が「効かない」ように見えて戸惑わないよう、案内
		 * （トースト等）を出す用途を想定するが、この部品自身は
		 * `banto-hub` の型・ストアを一切 import しない規約（冒頭コメント）
		 * のため、案内の実体は呼び出し側に委ねる。
		 */
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
		width = '480px',
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
			// 自分より手前に別の層（コマンドパレット等）が出ていれば譲る。
			// `defaultPrevented` だけでは足りない - window リスナーは登録順に走り、
			// **先に開いていたこの Drawer のリスナーが後から開いたパレットより先**
			// に実行されるので、この時点ではまだ消費されていない。
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
	// （バブリングで届いたドロワー内クリックと区別する）。ドロワー側に
	// `stopPropagation` の click ハンドラを付けずに済むので、a11y 的に
	// クリックハンドラを持つ非インタラクティブ要素が増えない。
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
			class="drawer"
			bind:this={panelEl}
			role="dialog"
			data-layer-inactive={open && !closing ? undefined : 'true'}
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
