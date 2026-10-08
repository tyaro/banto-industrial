import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

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
			adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html' })
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
			'@banto/forms',
			'@banto/grid-svelte',
			'@banto/theme',
			'@banto/ui'
		]
	},
	// banto-hub 固有の新設: Tauri を持たず axum サーバーが実体なので、
	// `vite dev` 単体では /api/* が同一オリジンに存在しない。開発時は
	// `cargo run -p banto-hub-core --bin banto-hub`（既定 PORT 8722、
	// apps/banto-hub/core/src/bin/banto-hub.rs 参照）を別途起動し、この
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
