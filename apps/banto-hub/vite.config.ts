import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
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
			'@banto/theme'
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
