import { describe, expect, it } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { createSerializedUiSettings } from './serializedUiSettings';

/** 内側の `set` を手で解決・失敗させられる偽物。 */
function fakeInner() {
	const sent: Array<{
		key: string;
		value: string;
		resolve: () => void;
		reject: (e: unknown) => void;
	}> = [];
	const inner: UiSettingsProvider = {
		get: async (key) => `got:${key}`,
		set: (key, value) =>
			new Promise<void>((resolve, reject) => {
				sent.push({ key, value, resolve, reject });
			})
	};
	return { inner, sent };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('createSerializedUiSettings', () => {
	it('同じキーは送信中 1 本だけ。最後の値だけが後から送られ、中間値は送られない', async () => {
		const { inner, sent } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		const p1 = s.set('k', '1');
		const p2 = s.set('k', '2');
		const p3 = s.set('k', '3');
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1']);
		sent[0].resolve();
		await p1;
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1', '3']);
		sent[1].resolve();
		await Promise.all([p2, p3]);
		await tick();
		expect(sent).toHaveLength(2);
	});

	it('別のキーは並行して送る', async () => {
		const { inner, sent } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		void s.set('a', '1');
		void s.set('b', '2');
		await tick();
		expect(sent.map((x) => `${x.key}=${x.value}`)).toEqual(['a=1', 'b=2']);
	});

	it('受け付けた後にトークンが替わったら、控えた書き込みは送られない', async () => {
		const { inner, sent } = fakeInner();
		let token: string | null = 'alice';
		const s = createSerializedUiSettings(inner, () => token);
		const p1 = s.set('k', '1');
		const p2 = s.set('k', '2'); // alice として控える
		token = 'bob';
		sent[0].resolve();
		await Promise.all([p1, p2]); // 捨てられた書き込みも解決する
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1']);
		// bob の書き込みは通る。
		void s.set('k', '3');
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1', '3']);
	});

	it('トークンが無ければ送らない', async () => {
		const { inner, sent } = fakeInner();
		const s = createSerializedUiSettings(inner, () => null);
		await s.set('k', '1');
		expect(sent).toHaveLength(0);
	});

	it('送信が失敗しても次の値は送られる（失敗した呼び出しは reject）', async () => {
		const { inner, sent } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		const p1 = s.set('k', '1');
		const p2 = s.set('k', '2');
		sent[0].reject(new Error('500'));
		await expect(p1).rejects.toThrow('500');
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1', '2']);
		sent[1].resolve();
		await p2;
	});

	it('get は内側へそのまま渡す', async () => {
		const { inner } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		expect(await s.get('x')).toBe('got:x');
	});
});
