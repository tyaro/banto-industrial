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
 *
 * banto v4.0.0（SvelteKit 3、#325）: `svelte()` を `sveltekit()` に替えた
 * （`sveltekit()` は Svelte のプラグインを含む）。アプリ内の遷移先は
 * `#lib/navigation.ts` の `resolveAppPath()`（SvelteKit の `resolve()`、
 * `$app/paths`）を通すようになり、ルートガード（`routes/(app)/+layout.ts`）の
 * テストが `$app/paths` を読むため - モックではなく本物の `resolve()`
 * （base は ''）で /login への redirect の行き先を確かめる。`$app/navigation`
 * などは今までどおり各テストが `vi.mock` で差し替える。`#lib/…` は
 * package.json の `imports`（Node のサブパス import）なので、プラグイン無しでも
 * テストから解決できる（上の「`$lib` を使わない」は SvelteKit 2 の頃の制約）。
 * アプリのビルド設定（adapter・preprocess）は `vite.config.ts` 側にあり、
 * ここでは要らない。
 */
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	test: {
		include: ['src/**/*.test.ts']
	}
});
