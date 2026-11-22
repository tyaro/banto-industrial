import { describe, expect, it } from 'vitest';
import { matchPath, type DemoHttpRoute, type DemoRequest } from '@banto-industrial/demo-backend';
import type { TagInput } from '#lib/banto/tagRegistryAdmin.js';
import { ChronoGazerRegistry, DemoNotFoundError, DemoValidationError } from './chronogazerState';
import { createChronoGazerRoutes } from './chronogazerRoutes';

const NEW_TAG: TagInput = {
	name: 'added',
	collectionGroupId: 2,
	address: '40100',
	dataType: 'u16',
	decimals: 0,
	enabled: true
};

function validation(fn: () => unknown): DemoValidationError {
	try {
		fn();
	} catch (e) {
		expect(e).toBeInstanceOf(DemoValidationError);
		return e as DemoValidationError;
	}
	throw new Error('検証の誤りになるはず');
}

describe('ChronoGazerRegistry（模擬の登録）', () => {
	it('初期のデータ: 接続 2 本・グループ 2 つ・タグ 20 本（数値・bit）・表示グループ 2 つ', () => {
		const r = new ChronoGazerRegistry();
		expect(r.connections).toHaveLength(2);
		expect(r.groups).toHaveLength(2);
		expect(r.tags).toHaveLength(20);
		expect(r.displayGroups).toHaveLength(2);
		const types = new Set(r.tags.map((t) => t.dataType));
		expect(types.has('bit') && types.has('f32') && types.has('u16')).toBe(true);
		// ペンは初期のタグを指す。
		const ids = new Set(r.tags.map((t) => t.id));
		for (const g of r.displayGroups) for (const p of g.pens) expect(ids.has(p.tagId)).toBe(true);
	});

	it('タグの登録・編集（revision +1）・削除', () => {
		const r = new ChronoGazerRegistry();
		const created = r.createTag(NEW_TAG);
		expect(created.revision).toBe(1);
		expect(r.tags).toHaveLength(21);
		const updated = r.updateTag(created.id, {
			...NEW_TAG,
			unit: 'rpm',
			expectedRevision: created.revision
		});
		expect(updated.revision).toBe(2);
		expect(r.tag(created.id)?.unit).toBe('rpm');
		r.deleteTag(created.id);
		expect(r.tag(created.id)).toBeUndefined();
		expect(() => r.deleteTag(created.id)).toThrow(DemoNotFoundError);
	});

	it('検証の誤り: 必須・同じグループの重複・参照の存在・周期', () => {
		const r = new ChronoGazerRegistry();
		expect(validation(() => r.createTag({ ...NEW_TAG, name: ' ' })).fieldErrors[0].field).toBe(
			'name'
		);
		expect(
			validation(() => r.createTag({ ...NEW_TAG, name: 'pump_flow' })).fieldErrors[0].message
		).toContain('同じ');
		expect(
			validation(() => r.createTag({ ...NEW_TAG, collectionGroupId: 99 })).fieldErrors[0].field
		).toBe('collectionGroupId');
		expect(
			validation(() =>
				r.createGroup({ name: 'g', plcConnectionId: 1, periodMs: 123, enabled: true })
			).fieldErrors[0].field
		).toBe('periodMs');
		expect(
			validation(() =>
				r.createConnection({
					name: 'x',
					protocol: 'unknown',
					enabled: true,
					simulation: true,
					settings: {}
				})
			).fieldErrors[0].field
		).toBe('protocol');
	});

	it('expectedRevision の不一致は 409・kind validation（本物と同じ）', () => {
		const r = new ChronoGazerRegistry();
		const err = validation(() => r.updateTag(1, { ...NEW_TAG, expectedRevision: 5 }));
		expect(err.status).toBe(409);
		expect(err.fieldErrors[0].field).toBe('expectedRevision');
	});

	it('参照が残る削除は断る（接続→グループ、グループ→タグ）。空にすれば消せる', () => {
		const r = new ChronoGazerRegistry();
		expect(validation(() => r.deleteConnection(1)).fieldErrors[0].message).toContain(
			'収集グループ'
		);
		expect(validation(() => r.deleteGroup(1)).fieldErrors[0].message).toContain('タグ');
		const g = r.createGroup({ name: 'extra', plcConnectionId: 1, periodMs: 1000, enabled: true });
		r.deleteGroup(g.id);
		expect(r.group(g.id)).toBeUndefined();
	});

	it('表示グループのペンに使われているタグの削除は 422 で断り、何も消さない', () => {
		const r = new ChronoGazerRegistry();
		r.updateThresholds(1, { thresholdLl: null, thresholdL: 1, thresholdH: 5, thresholdHh: null });
		const pens = r.displayGroups.map((g) => g.pens.length);
		const error = validation(() => r.deleteTag(1));
		expect(error.status).toBe(422);
		expect(error.fieldErrors).toHaveLength(1);
		expect(error.fieldErrors[0].field).toBe('displayGroups');
		expect(error.fieldErrors[0].message).toContain(`「${r.displayGroups[0].name}」`);
		expect(error.fieldErrors[0].message).toContain('ペンに割り当てられているため削除できません');
		expect(r.tag(1)).toBeDefined();
		expect(r.thresholds).toHaveLength(1);
		expect(r.displayGroups.map((g) => g.pens.length)).toEqual(pens);
	});

	it('参照の無いタグは消せて、しきい値も消える', () => {
		const r = new ChronoGazerRegistry();
		const created = r.createTag(NEW_TAG);
		r.updateThresholds(created.id, {
			thresholdLl: null,
			thresholdL: 1,
			thresholdH: 5,
			thresholdHh: null
		});
		expect(r.thresholds).toHaveLength(1);
		r.deleteTag(created.id);
		expect(r.tag(created.id)).toBeUndefined();
		expect(r.thresholds).toHaveLength(0);
	});

	it('しきい値: 保存の無いタグは全部 null・revision 0。古い版は 409', () => {
		const r = new ChronoGazerRegistry();
		expect(r.thresholdsOf(2)).toMatchObject({ thresholdH: null, revision: 0 });
		const input = { thresholdLl: null, thresholdL: null, thresholdH: 9, thresholdHh: null };
		r.updateThresholds(2, input);
		expect(validation(() => r.updateThresholds(2, { ...input, expectedRevision: 0 })).status).toBe(
			409
		);
	});

	it('登録は読み直す（新しい registry）と初期に戻る', () => {
		const a = new ChronoGazerRegistry();
		a.createTag(NEW_TAG);
		expect(new ChronoGazerRegistry().tags).toHaveLength(20);
	});
});

describe('createChronoGazerRoutes（口）', () => {
	function call(routes: DemoHttpRoute[], method: string, fullPath: string, body?: unknown) {
		const url = new URL(fullPath, 'http://demo.local');
		const path = url.pathname;
		for (const route of routes) {
			if (route.method !== method) continue;
			const params = matchPath(route.path, path);
			if (!params) continue;
			return route.handle({ params, url, json: async () => body } as unknown as DemoRequest);
		}
		throw new Error(`口が無い: ${method} ${path}`);
	}

	it('POST /api/tags は 201、検証の誤りは 422 kind validation、無い id は 404', async () => {
		const routes = createChronoGazerRoutes();
		const created = await call(routes, 'POST', '/api/tags', NEW_TAG);
		expect(created.status).toBe(201);
		const bad = await call(routes, 'POST', '/api/tags', { ...NEW_TAG, name: '' });
		expect(bad.status).toBe(422);
		expect((await bad.json()).kind).toBe('validation');
		const missing = await call(routes, 'DELETE', '/api/tags/999');
		expect(missing.status).toBe(404);
		expect((await missing.json()).kind).toBe('not_found');
		const list = await (await call(routes, 'GET', '/api/tags')).json();
		expect(list).toHaveLength(21);
	});

	it('GET /api/collect/values は全タグの現在値（tag:<id>・good）を返す', async () => {
		const routes = createChronoGazerRoutes();
		const body = await (await call(routes, 'GET', '/api/collect/values')).json();
		expect(body.state).toBe('ready');
		expect(Object.keys(body.data)).toHaveLength(20);
		expect(body.data['tag:1']).toMatchObject({ quality: 'good' });
		expect(typeof body.serverNowMs).toBe('number');
	});

	it('無効なタグ・グループ・接続の値は載らず、有効に戻すと戻る', async () => {
		const r = new ChronoGazerRegistry();
		const routes = createChronoGazerRoutes(r);
		const keys = async () =>
			Object.keys((await (await call(routes, 'GET', '/api/collect/values')).json()).data);
		const base = (await keys()).length;
		expect(await keys()).toContain('tag:1');

		const tag = r.tag(1)!;
		await call(routes, 'PUT', '/api/tags/1', { ...tag, enabled: false });
		expect(await keys()).not.toContain('tag:1');
		expect(await keys()).toHaveLength(base - 1);
		await call(routes, 'PUT', '/api/tags/1', { ...tag, enabled: true });
		expect(await keys()).toContain('tag:1');

		const group = r.group(tag.collectionGroupId)!;
		const inGroup = r.tags.filter((t) => t.collectionGroupId === group.id).length;
		await call(routes, 'PUT', `/api/collection-groups/${group.id}`, { ...group, enabled: false });
		expect(await keys()).toHaveLength(base - inGroup);
		await call(routes, 'PUT', `/api/collection-groups/${group.id}`, { ...group, enabled: true });
		expect(await keys()).toHaveLength(base);

		const conn = r.connections.find((c) => c.id === group.plcConnectionId)!;
		await call(routes, 'PUT', `/api/plc-connections/${conn.id}`, { ...conn, enabled: false });
		expect(await keys()).not.toContain('tag:1');
		await call(routes, 'PUT', `/api/plc-connections/${conn.id}`, { ...conn, enabled: true });
		expect(await keys()).toHaveLength(base);
	});

	it('GET /api/collect/history は系列の無い空の履歴を返し、居ないタグだけを unknownTagIds に並べる', async () => {
		const routes = createChronoGazerRoutes();
		const res = await call(
			routes,
			'GET',
			'/api/collect/history?tagIds=1%2C1%2C9999%2C9999&fromMs=1000&toMs=61000&bins=60'
		);
		expect(res.status).toBe(200);
		expect(await res.json()).toEqual({
			state: 'ready',
			data: { fromMs: 1000, toMs: 61000, series: [], unknownTagIds: [9999] }
		});
	});

	it('DELETE /api/tags/:id は使用中のタグを 422 displayGroups で断る', async () => {
		const routes = createChronoGazerRoutes();
		const res = await call(routes, 'DELETE', '/api/tags/1');
		expect(res.status).toBe(422);
		const body = await res.json();
		expect(body.kind).toBe('validation');
		expect(body.field_errors[0].field).toBe('displayGroups');
	});

	it('接続の削除（グループが残る）は 422', async () => {
		const routes = createChronoGazerRoutes();
		const res = await call(routes, 'DELETE', '/api/plc-connections/1');
		expect(res.status).toBe(422);
	});
});
