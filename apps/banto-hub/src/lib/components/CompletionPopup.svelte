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
	}

	let { x, y, lineHeight, candidates, totalCount, activeIndex, onSelect, onHover, onClose }: Props =
		$props();

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

	/** 選択中の候補が見えるようスクロールを追従させる（実装指示）。 */
	$effect(() => {
		itemEls[activeIndex]?.scrollIntoView({ block: 'nearest' });
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
			<span class="completion-kind">{KIND_LABELS[candidate.kind]}</span>
			<span class="completion-label">{candidate.label}</span>
			{#if candidate.detail}
				<span class="completion-detail">{candidate.detail}</span>
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
		gap: 0.4rem;
		align-items: baseline;
		padding: 0.25rem 0.4rem;
		border-radius: var(--banto-radius);
		color: var(--banto-text);
		font-size: 0.85rem;
		cursor: pointer;
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
