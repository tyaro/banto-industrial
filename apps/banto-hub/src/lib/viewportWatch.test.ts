import { describe, expect, it, vi } from 'vitest';
import { watchMediaQuery, type MediaQueryListLike } from './viewportWatch';

/** `matchMedia` のテスト用スタブ: `change` リスナーを外から発火できる。 */
function createFakeMediaQueryList(initialMatches: boolean): MediaQueryListLike & {
	fireChange(matches: boolean): void;
	listenerCount(): number;
} {
	const listeners = new Set<(event: { matches: boolean }) => void>();
	return {
		matches: initialMatches,
		addEventListener: (_type, listener) => listeners.add(listener),
		removeEventListener: (_type, listener) => listeners.delete(listener),
		fireChange(matches: boolean) {
			for (const listener of listeners) listener({ matches });
		},
		listenerCount: () => listeners.size
	};
}

describe('watchMediaQuery', () => {
	it('matchMediaFn 未指定・window 無し環境では onChange(false) を1回呼ぶだけで済む', () => {
		const onChange = vi.fn();
		const unwatch = watchMediaQuery('(max-width: 900px)', onChange, undefined);
		expect(onChange).toHaveBeenCalledTimes(1);
		expect(onChange).toHaveBeenCalledWith(false);
		expect(() => unwatch()).not.toThrow();
	});

	it('購読直後に現在の matches で onChange を呼ぶ', () => {
		const mql = createFakeMediaQueryList(true);
		const onChange = vi.fn();
		watchMediaQuery('(max-width: 900px)', onChange, () => mql);
		expect(onChange).toHaveBeenCalledTimes(1);
		expect(onChange).toHaveBeenCalledWith(true);
	});

	it('change イベントのたびに onChange を呼ぶ', () => {
		const mql = createFakeMediaQueryList(false);
		const onChange = vi.fn();
		watchMediaQuery('(max-width: 900px)', onChange, () => mql);

		mql.fireChange(true);
		mql.fireChange(false);

		expect(onChange.mock.calls).toEqual([[false], [true], [false]]);
	});

	it('返した購読解除関数を呼ぶとリスナーが外れる', () => {
		const mql = createFakeMediaQueryList(false);
		const onChange = vi.fn();
		const unwatch = watchMediaQuery('(max-width: 900px)', onChange, () => mql);

		expect(mql.listenerCount()).toBe(1);
		unwatch();
		expect(mql.listenerCount()).toBe(0);

		mql.fireChange(true);
		expect(onChange).toHaveBeenCalledTimes(1); // 解除後は増えない
	});
});
