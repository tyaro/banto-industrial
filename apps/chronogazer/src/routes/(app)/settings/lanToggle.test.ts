import { describe, expect, it } from 'vitest';
import { lanToggleLocked } from './lanToggle';

describe('lanToggleLocked', () => {
	it('ログイン不要モードでなければ常に操作できる', () => {
		for (const viewer of [false, true]) {
			for (const enabled of [false, true]) {
				expect(lanToggleLocked(false, viewer, enabled)).toBe(false);
				expect(lanToggleLocked(undefined, viewer, enabled)).toBe(false);
			}
		}
	});

	it('ログイン不要 + 閲覧公開 OFF + LAN OFF は有効化できない（塞ぐ）', () => {
		expect(lanToggleLocked(true, false, false)).toBe(true);
	});

	it('ログイン不要 + 閲覧公開 ON なら有効化できる', () => {
		expect(lanToggleLocked(true, true, false)).toBe(false);
	});

	it('回帰: 全部 ON から閲覧公開を先に外しても、LAN を止める操作は残る', () => {
		// 閲覧公開 ON のとき → 外した後（enabledDraft は ON のまま）。
		expect(lanToggleLocked(true, true, true)).toBe(false);
		expect(lanToggleLocked(true, false, true)).toBe(false);
		// LAN を外すと「閲覧公開 OFF + LAN OFF」= 適用は停止・失効として通る。
		expect(lanToggleLocked(true, false, false)).toBe(true);
	});
});
