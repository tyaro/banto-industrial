import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

// relay-wright の svelte.config.js から複製。banto-hub は Tauri を持たず、
// axum (apps/banto-hub/core/src/assets.rs) が静的ビルドを配信するだけだが、
// adapter-static + フォールバックによる SPA 構成はそちらと同一（core 側の
// `#[folder = "../build"]` が `apps/banto-hub/build` を期待するため、出力先
// もそのまま合わせる）。
/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({
			pages: 'build',
			assets: 'build',
			fallback: 'index.html'
		})
	}
};

export default config;
