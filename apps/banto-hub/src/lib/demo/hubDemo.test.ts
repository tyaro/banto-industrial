import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DemoSocketConnection } from '@banto-industrial/demo-backend';
import { HubRegistry, DemoValidationError, DemoRevisionConflictError } from './hubState';
import { createHubSocketRoute } from './hubStream';
import type { TagInput } from '#lib/banto/tagRegistryAdmin.js';

const NEW_TAG: TagInput = {
	name: 'added',
	collectionGroupId: 2,
	address: '40100',
	dataType: 'u16',
	decimals: 0,
	enabled: true
};

describe('HubRegistry（模擬の登録）', () => {
	it('初期のデータ: 接続 2 本・グループ 2 つ・タグ 20 本（数値・bit・文字列）', () => {
		const r = new HubRegistry();
		expect(r.connections).toHaveLength(2);
		expect(r.groups).toHaveLength(2);
		expect(r.tags).toHaveLength(20);
		const types = new Set(r.tags.map((t) => t.dataType));
		expect(types.has('bit') && types.has('f32') && types.has('string')).toBe(true);
	});

	it('登録・編集・削除が revision を進め、catalog に出る', () => {
		const r = new HubRegistry();
		const rev0 = r.revision;
		const created = r.createTag(NEW_TAG);
		expect(r.revision).toBe(rev0 + 1);
		expect(r.catalog().tags).toHaveLength(21);
		const updated = r.updateTag(created.id, { ...NEW_TAG, unit: 'rpm', expectedRevision: 1 });
		expect(updated.revision).toBe(2);
		expect(() => r.updateTag(created.id, { ...NEW_TAG, expectedRevision: 1 })).toThrow(
			DemoRevisionConflictError
		);
		r.deleteTag(created.id);
		expect(r.catalog().tags).toHaveLength(20);
	});

	it('同じグループの名前の重複・存在しないグループは検証で断る', () => {
		const r = new HubRegistry();
		expect(() => r.createTag({ ...NEW_TAG, name: 'pump_flow' })).toThrow(DemoValidationError);
		expect(() => r.createTag({ ...NEW_TAG, collectionGroupId: 99 })).toThrow(DemoValidationError);
	});

	it('一括の作成は 1 本でもエラーなら 1 本も作らない', () => {
		const r = new HubRegistry();
		const result = r.createTagsBatch([NEW_TAG, { ...NEW_TAG, name: '' }], false);
		expect(result.ok).toBe(false);
		expect(r.tags).toHaveLength(20);
		expect(r.createTagsBatch([NEW_TAG], true).ok).toBe(true);
		expect(r.tags).toHaveLength(20);
		expect(r.createTagsBatch([NEW_TAG], false).ok).toBe(true);
		expect(r.tags).toHaveLength(21);
	});

	it('更新で comment を省くと保存済みが残り、空文字なら消える', () => {
		const r = new HubRegistry();
		const before = r.tags.find((t) => t.comment !== '')!;
		expect(before).toBeDefined();
		const { id, revision: _revision, ...rest } = before;
		const { comment, ...withoutComment } = rest;
		expect(comment).not.toBe('');
		r.updateTagsBatch([{ ...withoutComment, id, enabled: false }], false);
		const kept = r.tag(id)!;
		expect(kept.enabled).toBe(false);
		expect(kept.comment).toBe(comment);
		r.updateTag(id, { ...withoutComment, enabled: true });
		expect(r.tag(id)!.comment).toBe(comment);
		r.updateTagsBatch([{ ...withoutComment, id, comment: '' }], false);
		expect(r.tag(id)!.comment).toBe('');
	});

	it('接続・グループの削除は配下ごと消し、通知する', () => {
		const r = new HubRegistry();
		const rev0 = r.revision;
		const g1 = r.groups[0];
		const inGroup = r.tags.filter((t) => t.collectionGroupId === g1.id).length;
		expect(inGroup).toBeGreaterThan(0);
		expect(r.deleteGroup(g1.id)).toEqual({ deletedTags: inGroup });
		expect(r.groups.some((g) => g.id === g1.id)).toBe(false);
		expect(r.tags.some((t) => t.collectionGroupId === g1.id)).toBe(false);
		expect(r.revision).toBe(rev0 + 1);
		// 配下にグループとタグが残っているもう一方の接続を消す（連鎖）。
		const conn = r.connections.find((c) => r.groups.some((g) => g.plcConnectionId === c.id))!;
		const groupIds = r.groups.filter((g) => g.plcConnectionId === conn.id).map((g) => g.id);
		const tagCount = r.tags.filter((t) => groupIds.includes(t.collectionGroupId)).length;
		expect(groupIds.length).toBeGreaterThan(0);
		expect(tagCount).toBeGreaterThan(0);
		const tagsBefore = r.tags.length;
		const result = r.deleteConnection(conn.id);
		expect(result).toEqual({ deletedGroups: groupIds.length, deletedTags: tagCount });
		expect(r.connections.some((c) => c.id === conn.id)).toBe(false);
		expect(r.groups.some((g) => g.plcConnectionId === conn.id)).toBe(false);
		expect(r.tags.some((t) => groupIds.includes(t.collectionGroupId))).toBe(false);
		expect(r.tags).toHaveLength(tagsBefore - tagCount);
		expect(r.revision).toBe(rev0 + 2);
	});

	it('一括更新の名前の一意は、更新後の状態（同じバッチの他の行を含む）で見る', () => {
		const r = new HubRegistry();
		const empty = r.createGroup({
			name: 'empty',
			plcConnectionId: r.connections[0].id,
			periodMs: 1000,
			enabled: true,
			defaultWritable: false
		});
		const a = r.tags.find((t) => t.collectionGroupId === r.groups[0].id)!;
		const b = r.createTag({ ...NEW_TAG, name: a.name, collectionGroupId: r.groups[1].id });
		const toInput = (t: typeof a, groupId: number) =>
			({ ...t, collectionGroupId: groupId }) as unknown as TagInput & { id: number };
		const rows = [toInput(a, empty.id), toInput(b, empty.id)];
		const before = r.tags.map((t) => t.collectionGroupId);
		for (const dryRun of [true, false]) {
			const res = r.updateTagsBatch(rows, dryRun);
			expect(res.ok).toBe(false);
			expect(res.errors.map((e) => e.index)).toEqual([0, 1]);
		}
		expect(r.tags.map((t) => t.collectionGroupId)).toEqual(before);
		// 片方だけ移すのは通る。
		expect(r.updateTagsBatch([rows[0]], false).ok).toBe(true);
		// 名前を入れ替える一括は、元の状態では重複でも結果は一意なので通る。
		const x = r.tags.find((t) => t.collectionGroupId === r.groups[1].id && t.id !== b.id)!;
		const swap = [
			{ ...toInput(b, b.collectionGroupId), name: x.name },
			{ ...toInput(x, x.collectionGroupId), name: b.name }
		];
		expect(r.updateTagsBatch(swap, false).ok).toBe(true);
	});

	it('一括更新の expectedRevision が合わない行は行エラーで、全件を反映しない（dry run も）', () => {
		const r = new HubRegistry();
		const [a, b] = r.tags;
		const row = (t: typeof a, extra: object) =>
			({ ...t, unit: 'zz', ...extra }) as unknown as TagInput & { id: number };
		const rev0 = r.revision;
		const rows = [
			row(a, { expectedRevision: a.revision }),
			row(b, { expectedRevision: b.revision + 5 })
		];
		for (const dryRun of [true, false]) {
			const res = r.updateTagsBatch(rows, dryRun);
			expect(res.ok).toBe(false);
			expect(res.errors).toHaveLength(1);
			expect(res.errors[0].index).toBe(1);
			expect(res.errors[0].id).toBe(b.id);
			expect(res.errors[0].fieldErrors.map((e) => e.field)).toEqual(['expectedRevision']);
		}
		expect(r.tag(a.id)!.unit).toBe(a.unit);
		expect(r.revision).toBe(rev0);
		// 合えば通る（省略も通る）。
		const ok = r.updateTagsBatch([row(a, { expectedRevision: a.revision }), row(b, {})], false);
		expect(ok.ok).toBe(true);
		expect(r.tag(a.id)!.unit).toBe('zz');
	});
});

/** 模擬の WebSocket の相手（画面）の代わり。 */
function fakeConnection() {
	const sent: Record<string, unknown>[] = [];
	let onMessage: (data: string) => void = () => {};
	let onClose: () => void = () => {};
	const connection: DemoSocketConnection = {
		url: new URL('http://localhost/api/v1/stream'),
		params: {},
		protocols: [],
		send: (data) => sent.push(JSON.parse(data) as Record<string, unknown>),
		close: () => onClose(),
		onMessage: (l) => (onMessage = l),
		onClose: (l) => (onClose = l as () => void)
	};
	return {
		connection,
		sent,
		say: (m: unknown) => onMessage(JSON.stringify(m)),
		hangUp: () => onClose()
	};
}

describe('/api/v1/stream の模擬', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('購読の直後に 1 回のスナップショット、以後は周期で変わった値だけ', () => {
		let now = 1_700_000_000_000;
		const registry = new HubRegistry();
		const peer = fakeConnection();
		createHubSocketRoute(registry, () => now).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, tags: ['line1.press.*'], mode: 'on_change' });
		const snapshot = peer.sent[0] as { op: string; id: number; values: { tag: string }[] };
		expect(snapshot.op).toBe('data');
		expect(snapshot.id).toBe(1);
		expect(snapshot.values).toHaveLength(10);
		expect(snapshot.values.every((v) => v.tag.startsWith('line1.press.'))).toBe(true);

		// 数十秒進めると、変わった値（bit の切り替えなど）だけの差分が届く。
		const before = peer.sent.length;
		for (let i = 0; i < 120; i++) {
			now += 250;
			vi.advanceTimersByTime(250);
		}
		const deltas = peer.sent.slice(before) as { values: { id: number }[] }[];
		expect(deltas.length).toBeGreaterThan(0);
		for (const d of deltas) expect(d.values.length).toBeLessThanOrEqual(10);
	});

	it('ids の購読・登録の変更で config_changed・解除でタイマーが止まる', () => {
		const registry = new HubRegistry();
		const peer = fakeConnection();
		createHubSocketRoute(registry).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 7, ids: [1, 2] });
		expect((peer.sent[0] as { values: unknown[] }).values).toHaveLength(2);
		registry.createTag(NEW_TAG);
		expect(peer.sent.some((m) => m.op === 'config_changed')).toBe(true);

		peer.hangUp();
		const count = peer.sent.length;
		vi.advanceTimersByTime(5000);
		registry.createTag({ ...NEW_TAG, name: 'again' });
		expect(peer.sent).toHaveLength(count);
		expect(vi.getTimerCount()).toBe(0);
	});

	it('無効にしたタグは v: null・q: bad を 1 回送り、有効に戻すと値が戻る（タグ単体・グループ）', () => {
		let now = 1_700_000_000_000;
		const registry = new HubRegistry();
		const peer = fakeConnection();
		createHubSocketRoute(registry, () => now).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, tags: ['line1.press.*'], mode: 'on_change' });
		type Row = { id: number; v: number | null; q: string; t: number };
		const rowsOf = (from: number) =>
			peer.sent.slice(from).flatMap((m) => (m.values as Row[] | undefined) ?? []);
		const tag = registry.tags.find((t) => t.collectionGroupId === registry.groups[0].id)!;
		const step = () => {
			now += 250;
			vi.advanceTimersByTime(250);
		};

		// タグ単体。
		let from = peer.sent.length;
		registry.updateTag(tag.id, { ...tag, enabled: false } as unknown as TagInput);
		step();
		let rows = rowsOf(from).filter((r) => r.id === tag.id);
		expect(rows).toHaveLength(1);
		expect(rows[0]).toMatchObject({ v: null, q: 'bad' });
		// 無効のままなら再送しない。
		from = peer.sent.length;
		for (let i = 0; i < 8; i++) step();
		expect(rowsOf(from).filter((r) => r.id === tag.id)).toHaveLength(0);
		// 有効に戻すと、値が good で戻る。
		from = peer.sent.length;
		registry.updateTag(tag.id, { ...registry.tag(tag.id)!, enabled: true } as unknown as TagInput);
		step();
		rows = rowsOf(from).filter((r) => r.id === tag.id);
		expect(rows).toHaveLength(1);
		expect(rows[0].q).toBe('good');

		// グループ。
		const group = registry.groups[0];
		const inGroup = registry.tags.filter((t) => t.collectionGroupId === group.id).length;
		from = peer.sent.length;
		registry.updateGroup(group.id, { ...group, querySql: undefined, enabled: false });
		step();
		rows = rowsOf(from);
		expect(rows).toHaveLength(inGroup);
		expect(rows.every((r) => r.v === null && r.q === 'bad')).toBe(true);
		from = peer.sent.length;
		registry.updateGroup(group.id, { ...group, querySql: undefined, enabled: true });
		step();
		rows = rowsOf(from);
		expect(rows).toHaveLength(inGroup);
		expect(rows.every((r) => r.q === 'good')).toBe(true);

		// 削除は「消えた」通知を送らない。
		from = peer.sent.length;
		registry.deleteTag(tag.id);
		step();
		expect(rowsOf(from).filter((r) => r.id === tag.id)).toHaveLength(0);
		peer.hangUp();
	});

	it('購読直後のスナップショットは無効のタグも v: null・q: bad で載せる', () => {
		const registry = new HubRegistry();
		const tag = registry.tags[0];
		registry.updateTag(tag.id, { ...tag, enabled: false } as unknown as TagInput);
		const now = 1_700_000_000_000;
		const peer = fakeConnection();
		createHubSocketRoute(registry, () => now).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, ids: [tag.id], mode: 'on_change' });
		const first = peer.sent[0] as { values: { id: number; v: unknown; q: string; t: number }[] };
		expect(first.values).toEqual([
			expect.objectContaining({ id: tag.id, v: null, q: 'bad', t: now })
		]);
		peer.hangUp();
	});

	it('形の悪い購読は error、ping は pong', () => {
		const peer = fakeConnection();
		createHubSocketRoute(new HubRegistry()).connect(peer.connection);
		peer.say({ op: 'subscribe', id: 1, tags: [] });
		peer.say({ op: 'subscribe', id: 2, tags: ['a.*'] });
		peer.say({ op: 'ping' });
		expect(peer.sent.map((m) => m.op)).toEqual(['error', 'error', 'pong']);
		peer.hangUp();
	});
});

describe('HubRegistry.offsetAddress（模擬のアドレス移動）', () => {
	const item = (o: Record<string, unknown>) =>
		({
			connectionId: 1,
			address: 'D100',
			dataType: 'u16',
			n: 1,
			unit: 'points',
			...o
		}) as Parameters<HubRegistry['offsetAddress']>[0];
	const slmp = () => new HubRegistry().connections.find((c) => c.protocol !== 'modbus-tcp')!;
	const modbus = () => new HubRegistry().connections.find((c) => c.protocol === 'modbus-tcp')!;

	it('points は型のワード数ぶん・words は n ワード動かす', () => {
		const r = new HubRegistry();
		const id = slmp().id;
		expect(r.offsetAddress(item({ connectionId: id, n: 3 }))).toMatchObject({
			ok: true,
			canonical: 'D103'
		});
		expect(r.offsetAddress(item({ connectionId: id, dataType: 'f32', n: 3 }))).toMatchObject({
			ok: true,
			canonical: 'D106'
		});
		expect(
			r.offsetAddress(item({ connectionId: id, dataType: 'f32', n: 3, unit: 'words' }))
		).toMatchObject({
			canonical: 'D103'
		});
	});

	it('Modbus の桁を保つ・範囲外・解析できない・接続なしは行ごとの誤り', () => {
		const r = new HubRegistry();
		expect(
			r.offsetAddress(item({ connectionId: modbus().id, address: '40001', n: 10 }))
		).toMatchObject({ ok: true, canonical: '40011' });
		expect(r.offsetAddress(item({ connectionId: slmp().id, n: -500 }))).toMatchObject({
			ok: false
		});
		expect(r.offsetAddress(item({ connectionId: slmp().id, address: 'zzz' }))).toMatchObject({
			ok: false
		});
		expect(r.offsetAddress(item({ connectionId: 9999 }))).toEqual({
			ok: false,
			message: '接続 9999 が見つかりません'
		});
	});
});
