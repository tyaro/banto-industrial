import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	// Fixed port so tauri.conf.json's devUrl always matches.
	server: {
		port: 1420,
		strictPort: true
	},
	clearScreen: false
});
