/**
 * H5: フロントテスト基盤（docs/improvement-plan.md）。純関数のユニット
 * テストが中心で、Svelte コンポーネントは対象外（E2E は別途）。
 * SvelteKit の `$lib` エイリアスはテストで使わない（テストコードは相対
 * import で対象モジュールを読み、対象が `$lib/...` を import するときは
 * `vi.mock` で向け先を決める）。
 *
 * banto v2.0.0（#260、v2 移行 PR1d）: `@sveltejs/vite-plugin-svelte` の
 * `svelte()` だけを足した。`@banto/admin-core` の SessionController
 * （`sessionController.svelte.ts`・`registry.svelte.ts`）は runes
 * （`$state`）で書かれているので、コンパイルしないと import しただけで
 * `$state is not defined` になる。ガード・policy runner・ログアウトの
 * テストは**本物の** controller の上で走らせたい（モックの controller では
 * adopt/end・ticket・世代の規則が見えない）ため。SvelteKit のプラグイン
 * （`sveltekit()`）は入れない（`$app/*` は各テストが `vi.mock` で差し替える）。
 */
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [svelte()],
	test: {
		include: ['src/**/*.test.ts']
	}
});
