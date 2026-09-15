<script lang="ts">
	/**
	 * 外観カテゴリのルート（#359 段階2）。markup・state・`<style>` は
	 * AppearanceSection.svelte（段階1のまま co-located、移動なし）。
	 * banto-hub の Section は自分自身の見出し（`<h2>`）を既に持っているため
	 * （段階1参照）、テンプレートと違いここで追加の見出しは描画しない
	 * （DOM構造を変えない）。
	 */
	import AppearanceSection from '../AppearanceSection.svelte';
</script>

<div class="sections">
	<AppearanceSection />
</div>
