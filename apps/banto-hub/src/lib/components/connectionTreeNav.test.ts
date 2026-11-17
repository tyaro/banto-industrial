import { describe, expect, it } from 'vitest';
import { isNavigationKey } from './connectionTreeNav';

describe('isNavigationKey', () => {
	it('上下・左右・Home・End はナビゲーション', () => {
		for (const key of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End']) {
			expect(isNavigationKey(key)).toBe(true);
		}
	});

	it('Enter・Space・文字キー・Tab はナビゲーションではない（選んだ扱い）', () => {
		for (const key of ['Enter', ' ', 'a', 'Tab', 'Escape', 'ContextMenu']) {
			expect(isNavigationKey(key)).toBe(false);
		}
	});
});
