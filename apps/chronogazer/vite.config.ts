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
			// Tauri has no SSR server: static build with SPA fallback (spec §8.1).
			adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html' })
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
			'@banto/theme'
		]
	},
	// Fixed port so tauri.conf.json's devUrl always matches.
	server: {
		port: 1420,
		strictPort: true
	},
	clearScreen: false
});
