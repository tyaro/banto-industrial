/**
 * `drawerCloseGuard.ts` に対するユニットテスト（`formDirty.test.ts` と同じ
 * スタイル、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { isCloseAllowed } from './drawerCloseGuard';

describe('isCloseAllowed', () => {
	it('dirty=false なら Esc・オーバーレイ・× のすべてで許可する（従来どおり）', () => {
		expect(isCloseAllowed('escape', false)).toBe(true);
		expect(isCloseAllowed('overlay', false)).toBe(true);
		expect(isCloseAllowed('close-button', false)).toBe(true);
	});

	it('dirty=true では Esc とオーバーレイクリックを許可しない（誤爆防止）', () => {
		expect(isCloseAllowed('escape', true)).toBe(false);
		expect(isCloseAllowed('overlay', true)).toBe(false);
	});

	it('dirty=true でも × は常に許可する（最終判断は onRequestClose に委ねる）', () => {
		expect(isCloseAllowed('close-button', true)).toBe(true);
	});
});
