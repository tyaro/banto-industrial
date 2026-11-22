import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

// ブラウザで触れるデモ（GitHub Pages、docs/demo-pages-design.md §3.5）: `BASE_PATH`
// （例 `/banto-industrial/hub`）を渡したときだけ `paths.base` を置き、ルートごとに写した入口
// （`hub/tags/index.html` など）からも資産を読めるよう、資産のパスを絶対にする
// （`relative: false`）。未設定（Tauri・LAN 配信・製品のビルド）なら今と全く同じ設定。
// 出力先も `DEMO_BUILD_DIR` で分ける（`build/` は embed-ui が埋め込む製品の画面なので、
// デモの組み立てで上書きしない。`scripts/build-demo-pages.mjs` が渡す）。
// `@types/node` を入れていないので、環境変数は `globalThis.process` から読む。
const env =
	(globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env ?? {};
const basePath = env.BASE_PATH as `/${string}` | undefined;
const demoPaths = basePath ? { paths: { base: basePath, relative: false } } : {};
const outDir = env.DEMO_BUILD_DIR ?? 'build';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			preprocess: vitePreprocess(),
			// Tauri has no SSR server: static build with SPA fallback (spec §8.1).
			adapter: adapter({ pages: outDir, assets: outDir, fallback: 'index.html' }),
			...demoPaths
		})
	],
	// @banto/* are git dependencies here (real node_modules packages, not
	// workspace links), so Vite's dep optimizer tries to esbuild-prebundle
	// their uncompiled .svelte/.svelte.ts sources and fails. Exclude them so
	// the Svelte plugin compiles them, same as workspace links in banto itself.
	optimizeDeps: {
		exclude: [
			'@banto/admin-core',
			'@banto/charts',
			'@banto/forms',
			'@banto/grid-svelte',
			'@banto/report',
			'@banto/theme',
			'@banto/tree-svelte',
			'@banto/ui'
		]
	},
	// Fixed port so tauri.conf.json's devUrl always matches.
	server: {
		port: 1420,
		strictPort: true
	},
	clearScreen: false
});
