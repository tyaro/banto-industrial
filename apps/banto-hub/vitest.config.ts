/**
 * H5: フロントテスト基盤（docs/improvement-plan.md）。純関数のユニット
 * テストのみを対象とし、Svelte コンポーネントは対象外（E2E は別途）。
 * SvelteKit の `$lib` エイリアスはテストで使わない（テストコードは相対
 * import で対象モジュールを読む）ため、`@sveltejs/vite-plugin-svelte` 等の
 * プラグインは導入しない最小構成。
 */
import { defineConfig } from 'vitest/config';

export default defineConfig({
	test: {
		include: ['src/**/*.test.ts']
	}
});
