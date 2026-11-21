import { describe, expect, it } from 'vitest';
import { threePaneDockLayout, type DockLayout, type DockNode } from '@banto/dock-svelte';
import {
	LOG_BAND_MAX_VIEWPORT_HEIGHT,
	lowScreenDefaultLayout,
	startsLogInBand
} from './dockLogBand.svelte';

/** 部品の既定の配置（両ページと同じ: 追加パネル `log` を下の外周）。 */
function defaultLayout(hostHeight: number): DockLayout {
	return threePaneDockLayout({
		hostWidth: 1280,
		hostHeight,
		panels: {
			left: { title: '左' },
			center: { title: '中央' },
			right: { title: '右' },
			extra: [{ id: 'log', title: 'ログ', initial: { edge: 'bottom' } }]
		}
	});
}

function dockedIds(node: DockNode | null): string[] {
	if (!node) return [];
	if (node.type === 'panel') return [node.id];
	return node.children.flatMap((child) => dockedIds(child));
}

describe('startsLogInBand', () => {
	it('閾値未満なら帯、閾値以上なら開く、高さが分からなければ開く', () => {
		expect(LOG_BAND_MAX_VIEWPORT_HEIGHT).toBe(900);
		expect(startsLogInBand(614)).toBe(true); // 1366×768・125%
		expect(startsLogInBand(720)).toBe(true); // 1280×720
		expect(startsLogInBand(899)).toBe(true);
		expect(startsLogInBand(900)).toBe(false);
		expect(startsLogInBand(1080)).toBe(false); // 1920×1080
		expect(startsLogInBand(0)).toBe(false);
	});
});

describe('lowScreenDefaultLayout', () => {
	it('低い画面ではログを下端の帯にし、ほかの区画は木に残す', () => {
		const layout = lowScreenDefaultLayout(defaultLayout(720), 720);
		expect(dockedIds(layout.docked)).not.toContain('log');
		expect(dockedIds(layout.docked)).toEqual(expect.arrayContaining(['left', 'center', 'right']));
		expect(layout.autoHidden).toHaveLength(1);
		expect(layout.autoHidden?.[0]).toMatchObject({ edge: 'bottom' });
		expect(dockedIds(layout.autoHidden?.[0]?.node ?? null)).toEqual(['log']);
	});

	it('閾値の直前（899）は帯、閾値（900）以上はそのまま（下にドッキングして開く）', () => {
		expect(lowScreenDefaultLayout(defaultLayout(899), 899).autoHidden).toHaveLength(1);
		const tall = defaultLayout(900);
		expect(lowScreenDefaultLayout(tall, 900)).toBe(tall);
		const fullHd = defaultLayout(1080);
		const kept = lowScreenDefaultLayout(fullHd, 1080);
		expect(kept).toBe(fullHd);
		expect(dockedIds(kept.docked)).toContain('log');
		expect(kept.autoHidden ?? []).toHaveLength(0);
	});

	it('ログが木に無い配置（狭い表示など）はそのまま', () => {
		const layout: DockLayout = {
			version: 2,
			docked: { type: 'panel', id: 'center', title: '中央' },
			floating: []
		} as DockLayout;
		expect(lowScreenDefaultLayout(layout, 720)).toBe(layout);
	});
});
