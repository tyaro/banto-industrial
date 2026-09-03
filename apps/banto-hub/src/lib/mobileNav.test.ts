import { describe, expect, it } from 'vitest';
import {
	applyNarrowChange,
	initialMobileNavState,
	resolveHamburgerTarget,
	type MobileNavState
} from './mobileNav';

describe('initialMobileNavState', () => {
	it('初期状態は閉・非狭幅', () => {
		expect(initialMobileNavState()).toEqual({ open: false, isNarrow: false });
	});
});

describe('applyNarrowChange', () => {
	it('狭幅になっただけでは開閉状態を変えない（既定は閉のまま）', () => {
		const state: MobileNavState = { open: false, isNarrow: false };
		expect(applyNarrowChange(state, true)).toEqual({ open: false, isNarrow: true });
	});

	it('狭幅のままオフキャンバスが開いていれば開いたまま', () => {
		const state: MobileNavState = { open: true, isNarrow: true };
		expect(applyNarrowChange(state, true)).toEqual({ open: true, isNarrow: true });
	});

	it('デスクトップ幅に戻ったら開いていても強制的に閉じる', () => {
		const state: MobileNavState = { open: true, isNarrow: true };
		expect(applyNarrowChange(state, false)).toEqual({ open: false, isNarrow: false });
	});
});

describe('resolveHamburgerTarget', () => {
	it('狭幅ならオフキャンバスを対象にする', () => {
		expect(resolveHamburgerTarget(true)).toBe('offcanvas');
	});

	it('デスクトップ幅ならサイドバー折り畳みを対象にする', () => {
		expect(resolveHamburgerTarget(false)).toBe('desktop-collapse');
	});
});
