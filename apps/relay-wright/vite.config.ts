import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
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
