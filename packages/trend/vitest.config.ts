import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// 時刻の目盛り（core/timeTicks.ts）はローカル時刻の区切りに揃えるので、試験の
// 結果が走らせる PC の時間帯で変わらないよう日本時間に固定する。
process.env.TZ = 'Asia/Tokyo';

export default defineConfig({
	plugins: [svelte()],
	test: {
		include: ['src/**/*.test.ts'],
		env: { TZ: 'Asia/Tokyo' }
	}
});
