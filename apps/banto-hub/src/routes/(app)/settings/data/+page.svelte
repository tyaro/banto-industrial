<script lang="ts">
	/**
	 * データカテゴリのルート（#359 段階2）。markup・state・関数は
	 * DataSection.svelte（段階1のまま co-located、移動なし）。
	 * `id="config-package"` は DataSection.svelte 側に残っているので、
	 * `/settings/data#config-package` のハッシュ誘導もそのまま効く。
	 */
	import DataSection from '../DataSection.svelte';
</script>

<div class="sections">
	<DataSection />
</div>
