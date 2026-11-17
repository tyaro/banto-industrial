import { describe, expect, it } from 'vitest';
import {
	applyNarrowChange,
	initialMobileNavState,
	resolveHamburgerTarget,
	resolveShellNav,
	type MobileNavState
} from './mobileNav';

describe('applyNarrowChange', () => {
	it('広幅へ広がったらドロワーを閉じる', () => {
		const open: MobileNavState = { open: true, isNarrow: true };
		expect(applyNarrowChange(open, false)).toEqual({ open: false, isNarrow: false });
	});

	it('狭幅のあいだは開閉の状態を保つ', () => {
		expect(applyNarrowChange({ open: true, isNarrow: true }, true).open).toBe(true);
		expect(applyNarrowChange(initialMobileNavState(), true)).toEqual({
			open: false,
			isNarrow: true
		});
	});
});

describe('resolveHamburgerTarget', () => {
	it('ドロワーのときは開閉、そうでなければ折り畳み', () => {
		expect(resolveHamburgerTarget(true)).toBe('offcanvas');
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

	it('サイドバー: 広幅も狭幅も従来どおりの列（ドロワーにしない）', () => {
		for (const narrow of [false, true]) {
			expect(resolveShellNav('sidebar', narrow, false)).toEqual({
				showBar: false,
				showSidebar: true,
				drawerMode: false,
				showDrawerButton: false
			});
		}
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
