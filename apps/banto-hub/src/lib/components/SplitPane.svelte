<script lang="ts">
	/**
	 * 汎用部品（T13-1、docs/ux-plan.md §4b）: 左右2ペインレイアウト。
	 * 左幅は固定 prop（`leftWidth`）で十分と判断（2026-08-08 決定）。
	 * リサイズ可能なスプリッタは需要が出てから追加する — 現時点では
	 * 「ドラッグでリサイズ」を要求する利用箇所がなく、実装・状態永続化
	 * （ユーザーごとの幅記憶など）のコストに見合わないため見送り。
	 *
	 * アプリ非依存 — banto-hub の型・ストアを import しない。
	 */
	import type { Snippet } from 'svelte';

	interface Props {
		/** 左ペイン幅（CSS の長さ文字列）。既定 280px。 */
		leftWidth?: string;
		left: Snippet;
		right: Snippet;
	}

	let { leftWidth = '280px', left, right }: Props = $props();
</script>

<div class="split-pane">
	<div class="pane pane-left" style:width={leftWidth}>
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
</style>
