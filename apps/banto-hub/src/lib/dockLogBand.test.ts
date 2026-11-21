import { describe, expect, it } from 'vitest';
import { threePaneDockLayout, type DockLayout, type DockNode } from '@banto/dock-svelte';
import {
	LOG_BAND_MAX_VIEWPORT_HEIGHT,
	lowScreenDefaultLayout,
	lowScreenLogBand,
	startsLogInBand,
	type LogBandPane
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

describe('lowScreenLogBand（狭い表示: 控えた広い配置を置き換える、PR #1270 のレビュー）', () => {
	function setup(options: { narrow: boolean; height?: number }) {
		let kept = defaultLayout(800);
		const restored: unknown[] = [];
		let finishRestore: (() => void) | null = null;
		const pane: LogBandPane = {
			resetLayout: async () => {
				kept = defaultLayout(800);
				return 'applied';
			},
			layoutSnapshot: () => ({ schema: 1, panels: [], layout: kept }),
			restoreLayout: (saved) =>
				new Promise((resolve) => {
					restored.push(saved);
					finishRestore = () => {
						kept = (saved as { layout: DockLayout }).layout;
						resolve({ outcome: 'applied' });
					};
				})
		};
		const band = lowScreenLogBand({
			pane: () => pane,
			// 狭い表示ではログが木に無い（部品の口は動かせない）。
			dock: { autoHideRegion: () => !options.narrow },
			narrow: () => options.narrow,
			viewportHeight: () => options.height ?? 800
		});
		return {
			band,
			restored,
			finish: async () => {
				finishRestore?.();
				await new Promise((r) => setTimeout(r, 0));
			},
			kept: () => kept
		};
	}

	it('狭い表示で既定から始めたら、控えをログを帯にした配置で置き換え、済むまで保存を止める', async () => {
		const t = setup({ narrow: true });
		t.band.onRestore('fresh');
		t.band.beforeBaseline();
		expect(t.band.holding()).toBe(true);
		expect(t.restored).toHaveLength(1);
		const layout = (t.restored[0] as { layout: DockLayout }).layout;
		expect(dockedIds(layout.autoHidden?.[0]?.node ?? null)).toEqual(['log']);
		await t.finish();
		expect(t.band.holding()).toBe(false);
	});

	it('保存値から始めた・高い画面なら置き換えない', async () => {
		const saved = setup({ narrow: true });
		saved.band.onRestore('restored');
		saved.band.beforeBaseline();
		expect(saved.restored).toEqual([]);
		expect(saved.band.holding()).toBe(false);
		const tall = setup({ narrow: true, height: 1080 });
		tall.band.onRestore('fresh');
		tall.band.beforeBaseline();
		await new Promise((r) => setTimeout(r, 0));
		expect(tall.restored).toEqual([]);
		expect(tall.band.holding()).toBe(false);
	});

	it('狭い表示の「既定に戻す」は、戻した既定を帯にして控えを置き換える（済むまで保存を止める）', async () => {
		const t = setup({ narrow: true });
		t.band.onRestore('restored');
		const reset = t.band
			.viewItems([{ kind: 'action', id: 'reset-layout', label: '戻す', onSelect: () => {} }])
			.find((item) => item.kind === 'action');
		(reset as { onSelect: () => void }).onSelect();
		expect(t.band.holding()).toBe(true);
		await new Promise((r) => setTimeout(r, 0));
		expect(t.restored).toHaveLength(1);
		await t.finish();
		expect(t.band.holding()).toBe(false);
		expect(dockedIds(t.kept().autoHidden?.[0]?.node ?? null)).toEqual(['log']);
	});
});

describe('lowScreenLogBand.restoreSaved（サーバーの保存値、PR #1270 再レビュー P2）', () => {
	function setup(options: { status: string; narrow?: boolean; height?: number }) {
		let kept = defaultLayout(800);
		const restored: unknown[] = [];
		let banded = 0;
		const pane: LogBandPane = {
			resetLayout: async () => 'applied',
			layoutSnapshot: () => ({ schema: 1, panels: [], layout: kept }),
			restoreLayout: async (saved) => {
				restored.push(saved);
				// 部品は保存値を採らなければ開いた既定を当てる。
				kept =
					options.status === 'default'
						? defaultLayout(800)
						: (saved as { layout: DockLayout }).layout;
				return { status: options.status, outcome: 'applied' };
			}
		};
		const band = lowScreenLogBand({
			pane: () => pane,
			dock: {
				autoHideRegion: () => {
					if (options.narrow) return false;
					banded += 1;
					return true;
				}
			},
			narrow: () => options.narrow ?? false,
			viewportHeight: () => options.height ?? 720
		});
		return { band, pane, restored, banded: () => banded };
	}
	const flush = () => new Promise((r) => setTimeout(r, 0));

	it('部品が既定へ戻した（default）なら、低い画面では帯にし、済むまで保存を止め、基準に置き直す', async () => {
		const t = setup({ status: 'default' });
		t.band.onRestore('fresh');
		t.band.beforeBaseline();
		expect(t.banded()).toBe(1);
		t.band.restoreSaved(t.pane, { schema: 1, panels: [], layout: {} });
		expect(t.band.holding()).toBe(true);
		await flush();
		expect(t.banded()).toBe(2);
		expect(t.band.holding()).toBe(false);
		expect(t.band.rebaseline()).toBe(true);
		expect(t.band.rebaseline()).toBe(false);
	});

	it('狭い表示で default なら、控えを帯にした配置で置き換える', async () => {
		const t = setup({ status: 'default', narrow: true });
		t.band.restoreSaved(t.pane, { schema: 1, panels: [], layout: {} });
		await flush();
		expect(t.restored).toHaveLength(2);
		const layout = (t.restored[1] as { layout: DockLayout }).layout;
		expect(dockedIds(layout.autoHidden?.[0]?.node ?? null)).toEqual(['log']);
		expect(t.band.holding()).toBe(false);
		expect(t.band.rebaseline()).toBe(true);
	});

	it('高い画面で default なら帯にしないが、保存もしない（基準に置き直す）', async () => {
		const t = setup({ status: 'default', height: 1080 });
		t.band.restoreSaved(t.pane, { schema: 1, panels: [], layout: {} });
		await flush();
		expect(t.banded()).toBe(0);
		expect(t.band.rebaseline()).toBe(true);
	});

	it('保存値を採った（restored・repaired）なら補正せず、基準にも置き直さない（利用者の配置に従う）', async () => {
		for (const status of ['restored', 'repaired']) {
			const t = setup({ status });
			t.band.restoreSaved(t.pane, { schema: 1, panels: [], layout: defaultLayout(800) });
			await flush();
			expect(t.banded()).toBe(0);
			expect(t.restored).toHaveLength(1);
			expect(t.band.holding()).toBe(false);
			expect(t.band.rebaseline()).toBe(false);
		}
	});

	it('保存値を当て始めた後の最初の基準では補正しない（復元の側が補正する）', async () => {
		const t = setup({ status: 'restored' });
		t.band.onRestore('fresh');
		t.band.restoreSaved(t.pane, { schema: 1, panels: [], layout: defaultLayout(800) });
		t.band.beforeBaseline();
		await flush();
		expect(t.banded()).toBe(0);
	});
});
