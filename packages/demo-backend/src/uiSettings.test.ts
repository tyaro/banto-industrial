import { describe, expect, it } from 'vitest';
import { createUiSettingsRoutes } from './uiSettings';
import type { DemoHttpRoute } from './http';

function call(
	routes: DemoHttpRoute[],
	method: string,
	key: string,
	body?: unknown,
	query = ''
): Promise<Response> {
	const route = routes.find((r) => r.method === method)!;
	return Promise.resolve(
		route.handle({
			method,
			url: new URL(`https://demo.test/api/ui-settings/${key}${query}`),
			params: { key },
			headers: new Headers(),
			signal: new AbortController().signal,
			text: async () => JSON.stringify(body),
			json: async <T>() => body as T
		})
	);
}

describe('createUiSettingsRoutes', () => {
	it('無いキーは value: null・revision: "0"', async () => {
		const routes = createUiSettingsRoutes();
		const res = await call(routes, 'GET', 'a');
		expect(await res.json()).toEqual({ value: null, revision: '0' });
	});

	it('PUT は 200 { revision } で、GET が同じ revision を返す', async () => {
		const routes = createUiSettingsRoutes();
		const put = await call(routes, 'PUT', 'a', { value: 'x' });
		expect(put.status).toBe(200);
		const { revision } = (await put.json()) as { revision: string };
		expect(await (await call(routes, 'GET', 'a')).json()).toEqual({ value: 'x', revision });
	});

	it('ifRevision が違えば 409 で現在値を返し、合えば書ける', async () => {
		const routes = createUiSettingsRoutes();
		const first = (await (
			await call(routes, 'PUT', 'a', { value: 'x', ifRevision: '0' })
		).json()) as {
			revision: string;
		};
		const stale = await call(routes, 'PUT', 'a', { value: 'y', ifRevision: '0' });
		expect(stale.status).toBe(409);
		const body = (await stale.json()) as { kind: string; current: { value: string } };
		expect(body.kind).toBe('conflict');
		expect(body.current.value).toBe('x');
		const ok = await call(routes, 'PUT', 'a', { value: 'y', ifRevision: first.revision });
		expect(ok.status).toBe(200);
	});

	it('DELETE は値を消して revision "0" を返す', async () => {
		const routes = createUiSettingsRoutes();
		await call(routes, 'PUT', 'a', { value: 'x' });
		const del = await call(routes, 'DELETE', 'a');
		expect(await del.json()).toEqual({ revision: '0' });
		expect(await (await call(routes, 'GET', 'a')).json()).toEqual({ value: null, revision: '0' });
	});
});
