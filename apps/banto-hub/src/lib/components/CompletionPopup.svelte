<script module lang="ts">
	/**
	 * 式欄の `aria-controls`/`aria-activedescendant` から参照する固定 id
	 * （`<script module>` に置いて `+page.svelte` からも import できるように
	 * している）。式欄は同時に高々1つしかマウントされない（`drawerMode` が
	 * 高々1つ）ので固定値でよい - `#tag-expression` 自体が固定 id なのと同じ理由。
	 */
	export const COMPLETION_LISTBOX_ID = 'tag-expression-completion';

	/** `${COMPLETION_LISTBOX_ID}-option-{i}` - `aria-activedescendant` に入れる値。 */
	export function completionOptionId(index: number): string {
		return `${COMPLETION_LISTBOX_ID}-option-${index}`;
	}
</script>

<script lang="ts">
	/**
	 * #342 段階B（docs/tag-server-design.md §4.2「セグメント補完」）: 演算タグの
	 * 式欄（`(app)/tags/+page.svelte` の `#tag-expression`）のキャレット位置に
	 * 浮かべる補完候補のポップアップ。座標に浮かべる小さな一覧という点は
	 * `TreeContextMenu.svelte` を手本にしている（`position: fixed` + x/y、
	 * ビューポート端の補正、ウィンドウ外クリックで閉じる）。
	 *
	 * **`TreeContextMenu` と決定的に違う点: フォーカスを奪わない。**
	 * コンテキストメニューは開いた時点で自分へフォーカスを移してキーを
	 * 自分で拾うが、補完は**打鍵の途中**に出るので、フォーカスは式欄の
	 * `<textarea>` に残っていなければならない（移すと次の1文字が打てない）。
	 * そのため ARIA の combobox パターンと同じく、
	 *
	 * - この要素は `role="listbox"` の**非フォーカス要素**、
	 * - 選択中の候補は `activeIndex`（呼び出し元が持つ）と
	 *   `aria-activedescendant`（式欄側に付ける）で示し、
	 * - ArrowDown/ArrowUp/Enter/Tab/Escape は**式欄の `keydown`** で処理する
	 *   （`+page.svelte::handleExpressionKeydown`）
	 *
	 * という分担にする。Escape をポップアップ側の `keydown` で拾って
	 * `stopPropagation` する案を採らなかったのも同じ理由（キーはそもそも
	 * ここへ来ない）。Esc の優先順位（ポップアップが開いている間は
	 * 「一覧から挿入」トグルを OFF にしない）は式欄側で
	 * `stopPropagation` して担保する。
	 *
	 * 候補の中身（何を出すか・何を除外するか）は一切持たない - 純関数
	 * `$lib/banto/expressionCompletion.ts` が決めたものを描くだけ。
	 */
	import type { CompletionCandidate } from '$lib/banto/expressionCompletion';

	interface Props {
		/** 一覧の左上（クライアント座標）。呼び出し元が `.expr-mirror` から算出する。 */
		x: number;
		/** キャレット行の**上端**（クライアント座標）。 */
		y: number;
		/** キャレット行の高さ。下に出すときの縦オフセットに使う。 */
		lineHeight: number;
		/** 表示する候補（呼び出し元が上限まで切ってから渡す）。 */
		candidates: CompletionCandidate[];
		/** 上限で切る前の総数 - `candidates.length` より多ければ「他 N 件」を出す。 */
		totalCount: number;
		/** 選択中の候補の添字（`candidates` に対する）。 */
		activeIndex: number;
		/** 候補を確定した（クリック）。 */
		onSelect: (index: number) => void;
		/** 候補にホバーした（選択中を移す）。 */
		onHover: (index: number) => void;
		/** ウィンドウ外クリックで閉じる。 */
		onClose: () => void;
		/**
		 * 座標の基準になっている要素（式欄の `<textarea>`）。**この要素自身の
		 * スクロールは「閉じる」ではなく「measure し直す」**に振り分けるために使う
		 * （下の scroll ハンドラ参照）。
		 */
		anchorEl: HTMLElement | null;
		/** `anchorEl` がスクロールしたので座標を取り直してほしい。 */
		onReanchor: () => void;
	}

	let {
		x,
		y,
		lineHeight,
		candidates,
		totalCount,
		activeIndex,
		onSelect,
		onHover,
		onClose,
		anchorEl,
		onReanchor
	}: Props = $props();

	let listEl: HTMLDivElement | undefined = $state();
	let itemEls: (HTMLDivElement | undefined)[] = $state([]);

	/** 候補の種別ごとのラベル（読み上げと視認の両方に効く短い日本語）。 */
	const KIND_LABELS: Record<CompletionCandidate['kind'], string> = {
		connection: '接続',
		group: 'グループ',
		tag: 'タグ',
		function: '関数'
	};

	/**
	 * 画面端の補正（`TreeContextMenu.clampPosition` と同じ「はみ出しぶんだけ
	 * 戻す」最小限の方式）。下端で溢れるときは**キャレット行の上**へ出す
	 * （実装指示）。`use:` アクションではなく `$effect` なのは、候補の増減で
	 * 高さが変わるたびに測り直す必要があるため。
	 */
	$effect(() => {
		const node = listEl;
		if (!node) return;
		// 候補が変わったら測り直す（依存として読む）。
		void candidates;
		node.style.left = `${x}px`;
		node.style.top = `${y + lineHeight}px`;
		const rect = node.getBoundingClientRect();
		const overflowX = rect.right - window.innerWidth;
		if (overflowX > 0) node.style.left = `${Math.max(0, x - overflowX)}px`;
		if (rect.bottom > window.innerHeight && y - rect.height >= 0) {
			node.style.top = `${y - rect.height}px`;
		}
	});

	/**
	 * 選択中の候補が見えるようスクロールを追従させる（実装指示）。
	 *
	 * `scrollIntoView` ではなく**この一覧の `scrollTop` を直接動かす**:
	 * `scrollIntoView` はポップアップが画面端に掛かっていると**祖先（ページ）まで
	 * スクロールさせる**ことがあり、下の「スクロールで閉じる」と噛み合って
	 * 開いた瞬間に自分で閉じてしまう。
	 */
	$effect(() => {
		const list = listEl;
		const item = itemEls[activeIndex];
		if (!list || !item) return;
		const top = item.offsetTop;
		const bottom = top + item.offsetHeight;
		if (top < list.scrollTop) list.scrollTop = top;
		else if (bottom > list.scrollTop + list.clientHeight)
			list.scrollTop = bottom - list.clientHeight;
	});

	/**
	 * #380 レビュー対応3: `position: fixed` なので、開いている間にページ・ペイン・
	 * 式欄のどれかがスクロールしたりウィンドウがリサイズされたりすると、
	 * ポップアップだけが古い座標に取り残される（画面外にも行く）。**再測定では
	 * なく閉じる**: 矢印キーでキャレットが動いたときに閉じる既存方針（「打ち直せば
	 * また開く」）と揃うし、スクロール中に追いかけ続けるより素直。
	 *
	 * `scroll` は**バブルしない**ので、任意の祖先のスクロールを拾うために
	 * `capture: true` で `window` に張る。ただし**ポップアップ自身のスクロール**
	 * （候補が多いときの選択追従）で閉じてしまわないよう、発生元が
	 * この要素の中なら無視する。リスナーはこのコンポーネントが存在する間
	 * ＝ポップアップが開いている間だけ張られる（`{#if}` で破棄されると
	 * `$effect` のクリーンアップで外れる）。
	 *
	 * **式欄そのもののスクロールだけは「閉じる」ではなく「座標を取り直す」**
	 * （`onReanchor`）。textarea は `rows="2"` なので**長い式を打つと編集のたびに
	 * 自動スクロールする**（改行や折り返しでキャレット行が見える位置へ動く）。
	 * これで閉じてしまうと「長い式を打っている最中に候補が消える」ことになり、
	 * ポップアップが一番役に立つ場面で使えない。座標はキャレットから測り直せば
	 * よいだけなので、閉じる必要がない（CI で
	 * `banto-hub-tags-expression-completion.spec.ts` のテスト6 が落ちて判明した
	 * 実挙動の不具合 - 2026-09-16）。ページ・ペインのスクロールは従来どおり閉じる
	 * （こちらはキャレットごと画面外へ出ていくため、追いかける意味が薄い）。
	 */
	$effect(() => {
		const onScroll = (event: Event): void => {
			const target = event.target;
			if (!(target instanceof Node)) {
				onClose();
				return;
			}
			// 自分自身のスクロール（候補が多いときの選択追従）は無視。
			if (listEl?.contains(target)) return;
			// 式欄自身のスクロール（長い式の編集で自動的に動く）は座標を取り直すだけ。
			if (anchorEl?.contains(target)) {
				onReanchor();
				return;
			}
			onClose();
		};
		const onResize = (): void => onClose();
		// **次のフレームまで待ってから張る**: ポップアップを開くきっかけになった
		// 操作（候補をクリックする前のフォーカス移動など）が既にスクロールを
		// 発生させていると、その `scroll` イベントは次のフレームで配送される。
		// HTML 仕様上「スクロールステップ（scroll イベントの配送）」は
		// 「アニメーションフレームコールバック」より**前**に走るので、
		// `requestAnimationFrame` の中で張れば、開く直前の操作に由来する
		// スクロールで即座に閉じてしまうことがない。
		let armed = false;
		const frame = requestAnimationFrame(() => {
			armed = true;
			window.addEventListener('scroll', onScroll, true);
			window.addEventListener('resize', onResize);
		});
		return () => {
			cancelAnimationFrame(frame);
			if (!armed) return;
			window.removeEventListener('scroll', onScroll, true);
			window.removeEventListener('resize', onResize);
		};
	});

	function handleWindowPointerDown(event: PointerEvent): void {
		if (listEl && event.target instanceof Node && !listEl.contains(event.target)) onClose();
	}
</script>

<svelte:window onpointerdown={handleWindowPointerDown} />

<!--
	フォーカスを持たないので `tabindex` は付けない（上の doc comment 参照）。
	確定は `onclick` ではなく **`onmousedown` で `preventDefault` してから**
	行う: `mousedown` の既定動作で式欄からフォーカスが外れると、挿入先の
	キャレット位置（`selectionStart`）が失われる。副次的に、`role="option"` の
	要素に `onclick` を付けたときの Svelte a11y 警告
	（`a11y_click_events_have_key_events` - キーボード操作は式欄側が担うので
	ここには付けようがない）も避けられる。
-->
<div
	class="completion-popup"
	id={COMPLETION_LISTBOX_ID}
	role="listbox"
	aria-label="式の補完候補"
	data-testid="expression-completion"
	bind:this={listEl}
>
	{#each candidates as candidate, i (`${candidate.kind}:${candidate.label}`)}
		<div
			class="completion-item"
			class:active={i === activeIndex}
			id={completionOptionId(i)}
			role="option"
			aria-selected={i === activeIndex}
			tabindex="-1"
			bind:this={itemEls[i]}
			onmouseenter={() => onHover(i)}
			onmousedown={(e) => {
				e.preventDefault();
				onSelect(i);
			}}
		>
			<!--
				#380 レビュー対応1: 1行目は候補の種別を問わず同じ並び
				（種別 / 名前 / `detail`）にして見た目を揃え、組み込み関数だけが
				持つ `description`（1行の日本語説明）は2行目へ回す。`detail` に
				連結すると、タグ候補の「型・単位」と同じスロットに一文が入って
				1行目が崩れるため。
			-->
			<div class="completion-line">
				<span class="completion-kind">{KIND_LABELS[candidate.kind]}</span>
				<span class="completion-label">{candidate.label}</span>
				{#if candidate.detail}
					<span class="completion-detail">{candidate.detail}</span>
				{/if}
			</div>
			{#if candidate.description}
				<span class="completion-description">{candidate.description}</span>
			{/if}
		</div>
	{/each}
	{#if totalCount > candidates.length}
		<!-- 上限で切った残り。候補ではないので `role="option"` にはしない。 -->
		<div class="completion-more" data-testid="expression-completion-more">
			他 {totalCount - candidates.length} 件（絞り込んでください）
		</div>
	{/if}
</div>

<style>
	.completion-popup {
		position: fixed;
		z-index: 1000;
		display: flex;
		flex-direction: column;
		min-width: 14rem;
		max-width: min(28rem, calc(100vw - 1rem));
		max-height: 15rem;
		overflow-y: auto;
		padding: 0.25rem;
		background: var(--banto-surface-raised, var(--banto-surface));
		border: 1px solid var(--banto-border);
		border-radius: var(--banto-radius);
		box-shadow: 0 8px 28px rgba(0, 0, 0, 0.28);
	}

	.completion-item {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
		padding: 0.25rem 0.4rem;
		border-radius: var(--banto-radius);
		color: var(--banto-text);
		font-size: 0.85rem;
		cursor: pointer;
	}

	/* 1行目（種別 / 名前 / detail）。候補の種別によらず同じ並び。 */
	.completion-line {
		display: flex;
		gap: 0.4rem;
		align-items: baseline;
	}

	/* 2行目（組み込み関数の説明だけ。種別ラベルのぶん字下げして揃える）。 */
	.completion-description {
		padding-left: calc(3.5rem + 0.4rem);
		color: var(--banto-text-muted);
		font-size: 0.72rem;
	}

	.completion-item.active {
		background: color-mix(in srgb, var(--banto-primary) 14%, transparent);
		color: var(--banto-primary);
	}

	.completion-kind {
		flex: none;
		min-width: 3.5rem;
		color: var(--banto-text-muted);
		font-size: 0.7rem;
	}

	.completion-label {
		flex: 1 1 auto;
		font-family: var(--banto-font-mono, monospace);
		overflow-wrap: anywhere;
	}

	.completion-detail {
		flex: none;
		color: var(--banto-text-muted);
		font-size: 0.72rem;
	}

	.completion-more {
		padding: 0.25rem 0.4rem;
		color: var(--banto-text-muted);
		font-size: 0.72rem;
	}
</style>
