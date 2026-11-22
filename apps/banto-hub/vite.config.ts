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
			// 旧 svelte.config.js（SvelteKit 3 で vite.config.ts に統合）のコメントをそのまま:
			// relay-wright の svelte.config.js から複製。banto-hub は Tauri を持たず、
			// axum (apps/banto-hub/core/src/assets.rs) が静的ビルドを配信するだけだが、
			// adapter-static + フォールバックによる SPA 構成はそちらと同一（core 側の
			// `#[folder = "../build"]` が `apps/banto-hub/build` を期待するため、出力先
			// もそのまま合わせる）。
			adapter: adapter({ pages: outDir, assets: outDir, fallback: 'index.html' }),
			...demoPaths
		})
	],
	// relay-wright の vite.config.ts から複製: @banto/* は git 依存（実体の
	// node_modules パッケージ）なので、Vite の dep optimizer が未コンパイル
	// の .svelte/.svelte.ts ソースを esbuild で事前バンドルしようとして失敗
	// する。除外して Svelte プラグインにコンパイルさせる。
	optimizeDeps: {
		exclude: [
			'@banto/admin-core',
			'@banto/charts',
			'@banto/dock-svelte',
			'@banto/forms',
			'@banto/grid-svelte',
			'@banto/theme',
			'@banto/tree-svelte',
			'@banto/ui'
		]
	},
	// banto-hub 固有の新設: Tauri を持たず axum サーバーが実体なので、
	// `vite dev` 単体では /api/* が同一オリジンに存在しない。開発時は
	// `cargo run -p banto-hub-host --bin banto-hub`（既定 PORT 8722、
	// apps/banto-hub/host/src/bin/banto-hub.rs 参照）を別途起動し、この
	// プロキシで /api への fetch をそちらへ中継する。本番（`vite build`
	// → axum が静的配信）ではこの設定自体が無関係（プロキシは dev
	// サーバーのみの機能）。
	server: {
		proxy: {
			'/api': 'http://127.0.0.1:8722'
		}
	},
	clearScreen: false
});
