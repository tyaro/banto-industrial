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

	it('書き込みが無ければ get は素通し', async () => {
		const { inner } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		expect(await s.get('x')).toBe('got:x');
	});

	it('書き込みの送信中に呼んだ get は、送信が終わるまで内側の get を呼ばない', async () => {
		const { inner, sent } = fakeInner();
		const calls: string[] = [];
		const s = createSerializedUiSettings(
			{
				get: (k) => {
					calls.push(k);
					return inner.get(k);
				},
				set: inner.set
			},
			() => 't'
		);
		void s.set('k', '1');
		const g = s.get('k');
		await tick();
		expect(calls).toEqual([]);
		sent[0].resolve();
		expect(await g).toBe('got:k');
		expect(calls).toEqual(['k']);
	});

	it('控えている値も送られてから読む', async () => {
		const { inner, sent } = fakeInner();
		const calls: string[] = [];
		const s = createSerializedUiSettings(
			{
				get: (k) => {
					calls.push(k);
					return inner.get(k);
				},
				set: inner.set
			},
			() => 't'
		);
		void s.set('k', '1');
		void s.set('k', '2');
		const g = s.get('k');
		sent[0].resolve();
		await tick();
		expect(sent.map((x) => x.value)).toEqual(['1', '2']);
		expect(calls).toEqual([]);
		sent[1].resolve();
		await g;
		expect(calls).toEqual(['k']);
	});

	it('書き込みが失敗しても get は読む（reject しない）', async () => {
		const { inner, sent } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		const p = s.set('k', '1');
		const g = s.get('k');
		sent[0].reject(new Error('500'));
		await expect(p).rejects.toThrow('500');
		expect(await g).toBe('got:k');
	});

	it('別のキーの書き込みは待たない', async () => {
		const { inner } = fakeInner();
		const s = createSerializedUiSettings(inner, () => 't');
		void s.set('a', '1');
		expect(await s.get('b')).toBe('got:b');
	});
});
