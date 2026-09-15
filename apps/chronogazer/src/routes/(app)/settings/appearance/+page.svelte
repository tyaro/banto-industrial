<script lang="ts">
	/**
	 * 外観カテゴリのルート（#359 chronogazer 分）。markup・state・
	 * `<style>` は AppearanceSection.svelte（co-located、移動なし）。
	 * chronogazer の Section は自分自身の見出し（`<h2>`）を既に持っている
	 * ため、テンプレートと違いここで追加の見出しは描画しない（DOM構造を
	 * 変えない）。
	 */
	import AppearanceSection from '../AppearanceSection.svelte';
</script>

<div class="sections">
	<AppearanceSection />
</div>
