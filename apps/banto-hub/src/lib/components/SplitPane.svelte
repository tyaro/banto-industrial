<script lang="ts">
	/**
	 * 汎用部品（T13-1、docs/ux-plan.md §4b）: 左右2ペインレイアウト。
	 * 左幅は固定 prop（`leftWidth`）で十分と判断（2026-08-08 決定）。
	 * リサイズ可能なスプリッタは需要が出てから追加する — 現時点では
	 * 「ドラッグでリサイズ」を要求する利用箇所がなく、実装・状態永続化
	 * （ユーザーごとの幅記憶など）のコストに見合わないため見送り。
	 *
	 * 2026-09-16 決定（#378）: **狭幅では左ペインをオフキャンバスへ退避する。**
	 * 400px 幅では固定 280px の左ペインにグリッドが押されて残り ≈120px しか
	 * 無く、`overflow: hidden` で clip された行がクリックできなかった（#375 の
	 * E2E で実測）。`Sidebar.svelte` の ≤900px オフキャンバス（`translateX(-100%)`
	 * ↔ `translateX(0)`）と同じ型にし、開いている間は右ペインの上に重ねて
	 * 出し、バックドロップのクリック・Esc・呼び出し側の操作（ノード選択など）で
	 * 閉じる。**狭幅かどうかの判定はこの部品では行わない** — 上の「アプリ
	 * 非依存」の規約を守るため `narrow` prop で受け取る（`mobileNavStore` を
	 * import しない）。`narrow=false`（広幅）のときは DOM も CSS も従来と
	 * 完全に同じ — 追加の属性・クラス・バックドロップはすべて `narrow` を
	 * 条件にしている。
	 *
	 * アプリ非依存 — banto-hub の型・ストアを import しない。
	 */
	import type { Snippet } from 'svelte';
	import { untrack } from 'svelte';

	interface Props {
		/** 左ペイン幅（CSS の長さ文字列）。既定 280px。 */
		leftWidth?: string;
		/**
		 * #378: 真のとき左ペインをオフキャンバス化する（狭幅の判定は呼び出し側
		 * の責務 — banto-hub なら `mobileNavStore.isNarrow`）。既定 `false`。
		 */
		narrow?: boolean;
		/**
		 * #378: 狭幅で左ペインが開いているか。`narrow=false` では無視する
		 * （広幅は常に2ペイン表示）。呼び出し側が `bind:leftOpen` で受け取り、
		 * トグルボタンやノード選択時のクローズに使う。
		 */
		leftOpen?: boolean;
		/**
		 * #378: 退避パネルの `aria-label`・バックドロップの読み上げに使う短い
		 * 名前。既定 `'ツリー'`。
		 */
		leftLabel?: string;
		/**
		 * #378: 狭幅のときだけ左ペインに付ける `id`。呼び出し側のトグルボタンの
		 * `aria-controls` から指すために受け取る（部品側で id を生成すると
		 * 呼び出し側へ渡す口が別途要るため、prop で受ける方を採った）。
		 * 広幅では属性自体を出さない（従来の DOM を変えないため）。
		 */
		leftId?: string;
		left: Snippet;
		right: Snippet;
	}

	let {
		leftWidth = '280px',
		narrow = false,
		leftOpen = $bindable(false),
		leftLabel = 'ツリー',
		leftId,
		left,
		right
	}: Props = $props();

	let leftPaneEl: HTMLDivElement | undefined = $state();

	/**
	 * #378: 開く操作をした要素（呼び出し側のトグルボタン）。閉じたときに
	 * フォーカスを戻す — `TreeContextMenu.svelte` が `triggerEl` を覚えて戻すのと
	 * 同じ形。`$state` にしない（描画に使わないため）。
	 */
	let triggerEl: HTMLElement | null = null;

	/**
	 * 下のフォーカス `$effect` が最後に処理した開閉状態。**遷移したときだけ**
	 * フォーカスを動かすために持つ（同 `$effect` のコメント参照）。`$state` に
	 * しない（描画に使わず、再実行のきっかけにもしたくないため）。
	 */
	let focusHandledOpen = false;

	const offcanvasOpen = $derived(narrow && leftOpen);

	function closeLeft(): void {
		leftOpen = false;
	}

	/**
	 * #378: この退避パネル（z-index 610）より手前に重なる一時的な UI が名乗る
	 * role。`dialog` = `Drawer`/`Modal`（900）、`menu` = `TreeContextMenu`（1000）。
	 * 下の Esc の doc comment 参照。
	 */
	const LAYER_ABOVE_SELECTOR = '[role="dialog"], [role="menu"]';

	/**
	 * #381 レビュー対応（2回目）: **可視な**上位層が出ているか。閉じている
	 * Drawer/Modal は `{#if open}` で DOM ごと消えるが、`display: none` や
	 * `visibility: hidden` で閉じるものが将来混じっても誤検出しないよう、
	 * 矩形の有無と計算済みスタイルの両方で可視性を見る。
	 */
	function hasVisibleLayerAbove(): boolean {
		for (const el of document.querySelectorAll(LAYER_ABOVE_SELECTOR)) {
			if (el.getClientRects().length === 0) continue;
			const style = getComputedStyle(el);
			if (style.display === 'none' || style.visibility === 'hidden') continue;
			return true;
		}
		return false;
	}

	/** 開いた直後、左ペイン内の最初のフォーカス可能要素へフォーカスする（`Drawer.svelte` と同じ最小限）。 */
	function focusFirstInLeftPane(): void {
		const node = leftPaneEl;
		if (!node) return;
		const focusable = node.querySelector<HTMLElement>(
			'input, select, textarea, button, a[href], [tabindex]:not([tabindex="-1"])'
		);
		(focusable ?? node).focus();
	}

	/**
	 * #378: 開閉に伴うフォーカス移動。開いたら左ペイン内へ、閉じたら開く操作を
	 * した要素へ戻す（Esc・バックドロップ・呼び出し側のクローズのどれでも
	 * 同じ扱い — 閉じた左ペインは `inert` でフォーカスを受けられないため、
	 * 戻さないとフォーカスが `<body>` へ落ちる）。
	 *
	 * 書き込み（`triggerEl`）と DOM 操作は `untrack` の中で行う
	 * （`mobileNav.svelte.ts` の doc comment にある `effect_update_depth_exceeded`
	 * の轍を踏まないための定石に合わせる）。
	 */
	/**
	 * #378（#381 レビュー対応A）: **狭幅 → 広幅の遷移で開閉状態を閉じへ戻す。**
	 * 広幅では退避そのものが無効なので `leftOpen` は描画に効かないが、`true` の
	 * まま残すと (a) もう一度狭幅へ戻した瞬間にツリーが開いた状態で現れ、
	 * (b) 呼び出し側が `leftOpen` を条件に使っている箇所（タグ登録の Esc 抑止）が
	 * 広幅でも効き続ける。サイドバーが同じ遷移で状態をリセットしている
	 * （`mobileNav.ts::applyNarrowChange`）のと同じ扱いにし、**部品側の1箇所で**
	 * 両ページぶんを直す（`$bindable` なので呼び出し側の `treeOpen` へ伝わる）。
	 *
	 * `triggerEl` も一緒に捨てる: 残したまま狭幅へ戻ると、下のフォーカス
	 * `$effect` の「閉じたら戻す」枝が**遷移だけで**古いボタンへフォーカスを
	 * 奪ってしまう。
	 */
	$effect(() => {
		if (narrow) return;
		untrack(() => {
			triggerEl = null;
			if (leftOpen) leftOpen = false;
		});
	});

	$effect(() => {
		const open = narrow && leftOpen;
		untrack(() => {
			// **開閉が実際に変わったときだけ**動かす（#381 レビュー対応3回目で
			// 踏んだ罠）: この `$effect` は `narrow`（= 呼び出し側では
			// `mobileNavStore.isNarrow`）にも依存していて、あのストアは
			// 状態を**オブジェクトごと差し替える**（`mobileNav.svelte.ts`）ため、
			// 値が同じでもサイドバーの開閉などで通知が飛んでくる。素直に
			// 「いまの `open` を見て毎回フォーカスを動かす」と、ハンバーガーで
			// サイドバーを開いた瞬間にフォーカスが左ペインへ引き戻され、
			// `triggerEl` も ☰ ボタンで上書きされてしまう（E2E で Esc が
			// サイドバーではなくツリーに効いて発覚）。
			if (open === focusHandledOpen) return;
			focusHandledOpen = open;
			if (open) {
				const active = document.activeElement;
				triggerEl = active instanceof HTMLElement ? active : null;
				focusFirstInLeftPane();
			} else {
				const previous = triggerEl;
				triggerEl = null;
				previous?.focus();
			}
		});
	});

	/**
	 * #378: Esc で閉じる。**開いている左ペインの Esc が最優先**で、他の Esc
	 * ハンドラ（タグ登録の「一覧から挿入」トグル、式欄の補完ポップアップ）には
	 * 届かせない。リスナーは開いている間だけ張る（`CompletionPopup.svelte` /
	 * タグ登録のトグルと同じ作法）。閉じているときは何もしない — 他のハンドラに
	 * 任せる。
	 *
	 * **層の約束（#381 レビュー対応3回目）**: 重なりは「上から1層ずつ Esc で
	 * 畳む」。この退避パネルより手前に出る UI は、**閉じたときに
	 * `event.preventDefault()` してイベントを消費する**こと（`Drawer`/`Modal` は
	 * 既にそうなっている。サイドバーのオフキャンバスは `role` を持たず
	 * {@link LAYER_ABOVE_SELECTOR} で拾えないので、`(app)/+layout.svelte` の Esc
	 * ハンドラ側で同じ約束を守っている）。この部品は `defaultPrevented` と
	 * {@link hasVisibleLayerAbove} の2つでその層を尊重する。
	 *
	 * 2段構えなのは `stopPropagation` の効き方の都合:
	 * 1. **左ペイン要素**の keydown（開いたらフォーカスは左ペイン内にあるので
	 *    ここを通る）で `preventDefault` + `stopPropagation` する。window まで
	 *    バブルさせないので、window に張られた他のハンドラは呼ばれない
	 *    （同じ window 上のリスナー同士では `stopPropagation` が効かないため、
	 *    window 側で止めるのでは間に合わない）。**ただし可視な上位層があるあいだ
	 *    は何もせずバブルさせる**（#381 レビュー対応3回目）: `Drawer` はタブ移動を
	 *    閉じ込めないので、ダイアログが出たままフォーカスだけが左ペインへ戻って
	 *    いることがあり、そこで消費すると手前のダイアログに Esc が届かない。
	 * 2. 念のため window にも張る（フォーカスが左ペイン外にある場合の保険）。
	 *    1 で処理済みのイベントは `defaultPrevented` で弾き、さらに
	 *    **この退避パネルより手前に重なっている一時的な UI が出ていれば譲る**
	 *    （#381 レビュー対応B・2回目）。一般則は「上に重なっているものから順に
	 *    Esc で閉じる」なので、判定は**発生元（`event.target`）ではなく「可視な
	 *    上位層が存在するか」**で行う（{@link hasVisibleLayerAbove}）:
	 *    `Drawer.svelte` は**タブ移動を閉じ込めない**（同ファイル冒頭 doc）ため、
	 *    Drawer が開いたままフォーカスがその外へ出ている状態がありえて、
	 *    発生元だけを見る判定はそこをすり抜けて「Drawer と退避パネルが両方
	 *    閉じる」ことになる。`target.closest(...)` は同じ結論に早く達する
	 *    近道として併用するだけ。
	 *
	 *    上位層は**その種の UI が名乗る role**（`dialog` = `Drawer`/`Modal`、
	 *    `menu` = `TreeContextMenu`）で拾う - どれも z-index はこの退避パネル
	 *    （610）より上（900 / 1000、下の CSS コメント参照）。新しく重なる UI を
	 *    足すときは、その role を {@link LAYER_ABOVE_SELECTOR} へ加えること。
	 */
	$effect(() => {
		if (!offcanvasOpen) return;
		const node = leftPaneEl;

		const onPaneKeydown = (event: KeyboardEvent): void => {
			if (event.key !== 'Escape') return;
			// 手前に層が出ているあいだは消費せずバブルさせる（上の doc の
			// 「層の約束」。フォーカスだけが左ペインへ戻っている場合がある）。
			if (hasVisibleLayerAbove()) return;
			event.preventDefault();
			event.stopPropagation();
			closeLeft();
		};
		const onWindowKeydown = (event: KeyboardEvent): void => {
			if (event.key !== 'Escape' || event.defaultPrevented) return;
			const target = event.target;
			// 近道: 発生元が上位層の中ならそれ以上調べない。
			if (target instanceof Element && target.closest(LAYER_ABOVE_SELECTOR)) return;
			// 本命: 発生元がどこであれ、可視な上位層が出ていれば譲る。
			if (hasVisibleLayerAbove()) return;
			event.preventDefault();
			closeLeft();
		};

		node?.addEventListener('keydown', onPaneKeydown);
		window.addEventListener('keydown', onWindowKeydown);
		return () => {
			node?.removeEventListener('keydown', onPaneKeydown);
			window.removeEventListener('keydown', onWindowKeydown);
		};
	});
</script>

<div class="split-pane" class:has-offcanvas={narrow}>
	{#if offcanvasOpen}
		<!--
			#378: 退避パネルのバックドロップ。`(app)/+layout.svelte` の
			`.nav-backdrop`（サイドバー用）と同じ「クリックで閉じる `<button>`」の
			型にする（クリックハンドラを持つ非インタラクティブ要素を作らない）。
			`position: absolute` なので覆うのは `.split-pane` の中だけ — ヘッダーや
			サイドバーは覆わない。
		-->
		<button
			type="button"
			class="pane-backdrop"
			onclick={closeLeft}
			aria-label={`背景をクリックして${leftLabel}を閉じる`}
		></button>
	{/if}
	<div
		class="pane pane-left"
		class:offcanvas={narrow}
		class:open={offcanvasOpen}
		style:width={leftWidth}
		bind:this={leftPaneEl}
		id={narrow ? leftId : undefined}
		role={offcanvasOpen ? 'region' : undefined}
		aria-label={offcanvasOpen ? leftLabel : undefined}
		aria-hidden={narrow && !leftOpen ? 'true' : undefined}
		inert={narrow && !leftOpen}
	>
		{@render left()}
	</div>
	<div class="pane pane-right">
		{@render right()}
	</div>
</div>

<style>
	.split-pane {
		display: flex;
		height: 100%;
		min-height: 0;
	}

	.pane-left {
		flex: 0 0 auto;
		min-width: 0;
		overflow-y: auto;
		border-right: 1px solid var(--banto-border);
	}

	.pane-right {
		flex: 1;
		min-width: 0;
		min-height: 0;
		display: flex;
		flex-direction: column;
	}

	/*
	 * #378（2026-09-16）: 狭幅のオフキャンバス。`Sidebar.svelte` の
	 * `.offcanvas` / `.open` をそのまま手本にする（`translateX(-100%)` ↔
	 * `translateX(0)` + transition）が、ビューポート全体ではなく
	 * `.split-pane` の中に閉じた `position: absolute` にする — ヘッダーや
	 * サイドバーの上には出さないため。ブレークポイントはこの部品では判定
	 * せず（アプリ非依存の規約）、すべて `narrow` prop 由来のクラスで切る。
	 *
	 * z-index=610/600 は `Drawer`/`Modal`(900)・`CommandPalette`/`ToastHost`/
	 * `TreeContextMenu`(1000)・オフキャンバスサイドバー(710、バックドロップ
	 * 700) より **下**。ユーザーが明示的に開いた一時的な最前面 UI と常設ナビ
	 * の手前に出て隠してはならない、という `Sidebar.svelte` と同じ理由。
	 *
	 * 閉じている間は `visibility: hidden` も併用する: `transform` だけだと
	 * 祖先の `overflow: hidden` で clip されているだけの「見えないが可視」な
	 * 状態になり、フォーカスや支援技術・テストから見て閉じていることが
	 * 表現できない（`inert` / `aria-hidden` と食い違う）。
	 */
	.split-pane.has-offcanvas {
		position: relative;
	}

	.pane-left.offcanvas {
		position: absolute;
		inset-block: 0;
		left: 0;
		z-index: 610;
		transform: translateX(-100%);
		visibility: hidden;
		transition:
			transform 0.2s ease,
			visibility 0.2s ease;
		background: var(--banto-surface);
		box-shadow: 12px 0 32px rgba(0, 0, 0, 0.25);
		backdrop-filter: var(--banto-backdrop, none);
		-webkit-backdrop-filter: var(--banto-backdrop, none);
	}

	.pane-left.offcanvas.open {
		transform: translateX(0);
		visibility: visible;
	}

	.pane-backdrop {
		position: absolute;
		inset: 0;
		z-index: 600;
		display: block;
		width: 100%;
		border: none;
		margin: 0;
		padding: 0;
		background: rgba(0, 0, 0, 0.35);
		cursor: pointer;
	}
</style>
