import { describe, expect, it } from 'vitest';
import {
	applyNarrowChange,
	initialMobileNavState,
	resolveHamburgerTarget,
	resolveShellNav,
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

describe('resolveShellNav（#1159 メニューバー）', () => {
	it('メニューバー: >900px はバーだけでサイドバーは無い', () => {
		expect(resolveShellNav('menubar', false, false)).toEqual({
			showBar: true,
			showSidebar: false,
			drawerMode: false,
			showDrawerButton: false
		});
	});

	it('メニューバー: ≤900px はバーを出さずサイドバーがドロワー', () => {
		expect(resolveShellNav('menubar', true, false)).toEqual({
			showBar: false,
			showSidebar: true,
			drawerMode: true,
			showDrawerButton: true
		});
	});

	it('メニューバー: >900px でも「その他」にも入らなければドロワーに戻る（バーはマウントしたまま）', () => {
		expect(resolveShellNav('menubar', false, true)).toEqual({
			showBar: true,
			showSidebar: true,
			drawerMode: true,
			showDrawerButton: true
		});
	});

	it('入りきらないフラグは ≤900px・メニューバー以外では効かない', () => {
		expect(resolveShellNav('menubar', true, true).showBar).toBe(false);
		expect(resolveShellNav('sidebar', false, true)).toMatchObject({
			showSidebar: true,
			drawerMode: false
		});
	});

	it('サイドバー: 広幅は列、狭幅はドロワー', () => {
		expect(resolveShellNav('sidebar', false, false).drawerMode).toBe(false);
		expect(resolveShellNav('sidebar', true, false)).toMatchObject({
			showBar: false,
			showSidebar: true,
			drawerMode: true
		});
	});

	it('ドック: サイドバーもドロワーのボタンも無い（狭幅もタブバー）', () => {
		for (const narrow of [false, true]) {
			expect(resolveShellNav('launcher', narrow, true)).toEqual({
				showBar: false,
				showSidebar: false,
				drawerMode: false,
				showDrawerButton: false
			});
		}
	});
});
