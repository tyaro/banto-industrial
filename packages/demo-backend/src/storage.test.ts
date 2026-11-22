import { describe, expect, it } from 'vitest';
import { createMemoryStorage, installDemoStorage, type DemoStorageTarget } from './storage';

describe('createMemoryStorage', () => {
	it('Storage の口（getItem・setItem・removeItem・clear・key・length）が動く', () => {
		const s = createMemoryStorage();
		expect(s.length).toBe(0);
		expect(s.getItem('a')).toBeNull();
		s.setItem('a', '1');
		s.setItem('b', 2 as unknown as string);
		expect(s.getItem('a')).toBe('1');
		expect(s.getItem('b')).toBe('2');
		expect(s.length).toBe(2);
		expect(s.key(0)).toBe('a');
		expect(s.key(1)).toBe('b');
		expect(s.key(2)).toBeNull();
		s.removeItem('a');
		expect(s.getItem('a')).toBeNull();
		s.clear();
		expect(s.length).toBe(0);
	});

	it('インスタンスごとに独立している', () => {
		const a = createMemoryStorage();
		const b = createMemoryStorage();
		a.setItem('k', 'v');
		expect(b.getItem('k')).toBeNull();
	});
});

describe('installDemoStorage', () => {
	it('本物の代わりに入り、本物へは書かない。2 回目は何もしない。戻せる', () => {
		const realLocal = createMemoryStorage();
		const realSession = createMemoryStorage();
		realLocal.setItem('banto.auth.token', 'other-demo-token');
		const target: DemoStorageTarget = { localStorage: realLocal, sessionStorage: realSession };
		const handle = installDemoStorage(target);

		// 本物の値を拾わない。
		expect(target.localStorage!.getItem('banto.auth.token')).toBeNull();
		target.localStorage!.setItem('banto.theme', 'dark');
		target.sessionStorage!.setItem('x', '1');
		expect(realLocal.getItem('banto.theme')).toBeNull();
		expect(realSession.getItem('x')).toBeNull();

		const replaced = target.localStorage;
		installDemoStorage(target);
		expect(target.localStorage).toBe(replaced);

		handle.uninstall();
		expect(target.localStorage).toBe(realLocal);
		expect(target.sessionStorage).toBe(realSession);
	});
});
