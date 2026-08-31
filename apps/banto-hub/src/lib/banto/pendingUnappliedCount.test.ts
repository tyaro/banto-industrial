/**
 * `pendingUnappliedCount.ts` に対するユニットテスト。実機で再現した不具合
 * （2026-08-31 オーナー報告: pending 4件中3件キャンセル済み・1件適用済み
 * ＝未適用0件のはずが「未適用 4件」と表示された）を直接検証する。
 */
import { describe, expect, it } from 'vitest';
import { countUnappliedPendingChanges, type PendingChangeStateLike } from './pendingUnappliedCount';

function withState(state: string): PendingChangeStateLike {
	return { state };
}

describe('countUnappliedPendingChanges', () => {
	it('空配列なら0を返す', () => {
		expect(countUnappliedPendingChanges([])).toBe(0);
	});

	it('pending と applying だけを数える', () => {
		const pending: PendingChangeStateLike[] = [
			withState('pending'),
			withState('applying'),
			withState('applied'),
			withState('canceled'),
			withState('failed')
		];
		expect(countUnappliedPendingChanges(pending)).toBe(2);
	});

	it('実機再現ケース: 4件中3件キャンセル済み・1件適用済み（未適用0件）は0を返す', () => {
		const pending: PendingChangeStateLike[] = [
			withState('canceled'),
			withState('canceled'),
			withState('canceled'),
			withState('applied')
		];
		expect(countUnappliedPendingChanges(pending)).toBe(0);
	});

	it('failed は数えない', () => {
		const pending: PendingChangeStateLike[] = [withState('failed'), withState('failed')];
		expect(countUnappliedPendingChanges(pending)).toBe(0);
	});

	it('pending/applying が複数件あればその件数を返す', () => {
		const pending: PendingChangeStateLike[] = [
			withState('pending'),
			withState('pending'),
			withState('applying'),
			withState('applied'),
			withState('canceled')
		];
		expect(countUnappliedPendingChanges(pending)).toBe(3);
	});
});
