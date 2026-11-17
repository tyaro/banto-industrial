import { describe, expect, it } from 'vitest';
import {
	isLauncherPosition,
	isNavLayout,
	LAUNCHER_POSITION_DEFAULT,
	NAV_LAYOUT_DEFAULT,
	parseLauncherPosition,
	parseNavLayout
} from './navLayout';

describe('NAV_LAYOUT_DEFAULT', () => {
	it('ChronoGazer の既定はメニューバー（#1159）', () => {
		expect(NAV_LAYOUT_DEFAULT).toBe('menubar');
	});
});

describe('parseNavLayout', () => {
	it.each(['sidebar', 'launcher', 'menubar'] as const)('保存値 %s はそのまま読む', (value) => {
		expect(parseNavLayout(value)).toBe(value);
	});

	it('未保存・知らない値は既定（メニューバー）に戻す', () => {
		expect(parseNavLayout(null)).toBe('menubar');
		expect(parseNavLayout('dock')).toBe('menubar');
		expect(parseNavLayout('')).toBe('menubar');
	});

	it('isNavLayout は 3 つの値だけを通す', () => {
		expect(isNavLayout('menubar')).toBe(true);
		expect(isNavLayout('Menubar')).toBe(false);
		expect(isNavLayout(undefined)).toBe(false);
	});
});

describe('parseLauncherPosition', () => {
	it.each(['bottom', 'top', 'left', 'right'] as const)('保存値 %s はそのまま読む', (value) => {
		expect(parseLauncherPosition(value)).toBe(value);
		expect(isLauncherPosition(value)).toBe(true);
	});

	it('未保存・知らない値は既定（下）に戻す', () => {
		expect(LAUNCHER_POSITION_DEFAULT).toBe('bottom');
		expect(parseLauncherPosition(null)).toBe('bottom');
		expect(parseLauncherPosition('center')).toBe('bottom');
	});
});
