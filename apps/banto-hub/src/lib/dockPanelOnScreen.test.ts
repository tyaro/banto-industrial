import { describe, expect, it } from 'vitest';
import type { DockLayout } from '@banto/dock-svelte';
import { isDockPanelOnScreen } from './dockPanelOnScreen';

const panel = (id: string, collapsed?: boolean) => ({
	type: 'panel' as const,
	id,
	title: id,
	...(collapsed ? { collapsed } : {})
});

function layout(partial: Partial<DockLayout>): DockLayout {
	return { version: 4, floating: [], docked: null, ...partial } as DockLayout;
}

const win = (id: string, open: boolean, collapsed?: boolean) => ({
	id,
	title: id,
	x: 0,
	y: 0,
	width: 100,
	height: 100,
	open,
	...(collapsed ? { collapsed } : {})
});

describe('isDockPanelOnScreen', () => {
	it('ドッキングして畳んでいなければ真、畳んでいれば偽', () => {
		const docked = {
			type: 'split' as const,
			id: 's',
			direction: 'vertical' as const,
			sizes: [0.7, 0.3],
			children: [panel('center'), panel('log')]
		};
		expect(isDockPanelOnScreen(layout({ docked } as never), 'log', null)).toBe(true);
		const collapsed = { ...docked, children: [panel('center'), panel('log', true)] };
		expect(isDockPanelOnScreen(layout({ docked: collapsed } as never), 'log', null)).toBe(false);
	});

	it('タブの組は表のタブだけ真', () => {
		const tabs = {
			type: 'tabs' as const,
			id: 't',
			activeIndex: 0,
			children: [panel('right'), panel('log')]
		};
		expect(isDockPanelOnScreen(layout({ docked: tabs } as never), 'log', null)).toBe(false);
		expect(
			isDockPanelOnScreen(layout({ docked: { ...tabs, activeIndex: 1 } } as never), 'log', null)
		).toBe(true);
	});

	it('浮かせた窓は開いていて畳んでいないときだけ真', () => {
		expect(isDockPanelOnScreen(layout({ floating: [win('log', true)] }), 'log', null)).toBe(true);
		expect(isDockPanelOnScreen(layout({ floating: [win('log', false)] }), 'log', null)).toBe(false);
		expect(isDockPanelOnScreen(layout({ floating: [win('log', true, true)] }), 'log', null)).toBe(
			false
		);
	});

	it('自動で隠した帯は引き出している間だけ真', () => {
		const l = layout({
			autoHidden: [{ node: panel('log'), edge: 'bottom' }]
		} as never);
		expect(isDockPanelOnScreen(l, 'log', null)).toBe(false);
		expect(isDockPanelOnScreen(l, 'log', 'log')).toBe(true);
	});

	it('配置に無ければ偽', () => {
		expect(isDockPanelOnScreen(layout({ docked: panel('center') } as never), 'log', null)).toBe(
			false
		);
	});
});
