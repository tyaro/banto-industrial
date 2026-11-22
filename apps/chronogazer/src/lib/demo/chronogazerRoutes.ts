/**
 * ChronoGazer の口の模擬（設計 docs/demo-pages-design.md §3.3・§6 の 4b）。`/tags` の画面が使う
 * 登録の口（接続・収集グループ・タグの一覧と登録・編集・削除、しきい値）と、起動で呼ばれる
 * 表示グループの一覧・収集から外れる設定。状態は {@link ChronoGazerRegistry}（このタブの
 * メモリだけ）。
 *
 * どの口が要るかは、デモのビルドで `/tags` を開き、模擬の「口がありません」の警告から
 * 拾った（2026-11-22・2026-11-23）。応答の型は各 `*Admin.ts` から取る（模擬の側で書き直さない）。
 * `/api/tags/from-hub`・`/api/hub*`・表示グループの編集は、今のデモの対象外で 501
 * （押して初めて呼ばれる）。履歴は `/monitor` を開くだけで呼ばれるので空で返す（本当の模擬は次の PR）。現在値（`/api/collect/values`）は
 * `/monitor` を開いたときの起動で呼ばれるので模擬する。
 */
import {
	demoSampleTime,
	demoTagValue,
	json,
	noContent,
	type DemoHttpRoute,
	type DemoRequest
} from '@banto-industrial/demo-backend';
import type { CollectHistory, Readout, ValuesResponse } from '#lib/banto/collectAdmin.js';
import type {
	CollectionGroupInput,
	ExclusionView,
	PlcConnectionInput,
	TagInput
} from '#lib/banto/tagRegistryAdmin.js';
import type { TagThresholdsInput } from '#lib/banto/tagThresholdsAdmin.js';
import { ChronoGazerRegistry, DemoNotFoundError, DemoValidationError } from './chronogazerState';

/** 登録の失敗を本物と同じ形の応答にする。 */
function failure(error: unknown): Response {
	if (error instanceof DemoValidationError) {
		return json({ kind: 'validation', field_errors: error.fieldErrors }, { status: error.status });
	}
	if (error instanceof DemoNotFoundError) {
		return json(
			{ kind: 'not_found', resource: error.resource, id: String(error.id) },
			{ status: 404 }
		);
	}
	throw error;
}

/** 口の本体を包み、登録の失敗を応答にする。 */
function guarded(handle: (req: DemoRequest) => Response | Promise<Response>) {
	return async (req: DemoRequest): Promise<Response> => {
		try {
			return await handle(req);
		} catch (error) {
			return failure(error);
		}
	};
}

/**
 * `GET /api/collect/values` の模擬: 収集の対象になるタグの現在値（タグ ID と時刻だけで決まる純関数）。
 * 本物は、タグ自身・収集グループ・PLC 接続のどれかが無効なら収集の構成に入れない
 * （`crates/banto-collect/src/config.rs` の `enabled` の絞り込み）ので、キーごと載せない。
 */
function valuesOf(registry: ChronoGazerRegistry, nowMs: number): ValuesResponse {
	const data: Extract<ValuesResponse, { state: 'ready' }>['data'] = {};
	for (const tag of registry.tags) {
		if (!tag.enabled) continue;
		const group = registry.group(tag.collectionGroupId);
		if (!group?.enabled) continue;
		if (!registry.connections.find((c) => c.id === group.plcConnectionId)?.enabled) continue;
		const sampleMs = demoSampleTime(nowMs, group?.periodMs);
		data[`tag:${tag.id}`] = {
			value: demoTagValue({ id: tag.id, dataType: tag.dataType, periodMs: group?.periodMs }, nowMs),
			ptimeMs: sampleMs,
			quality: 'good',
			lastGoodMs: sampleMs
		};
	}
	return { state: 'ready', data, serverNowMs: nowMs };
}

/**
 * 系列の無い履歴。`tagIds` のうちレジストリに居ないタグだけを `unknownTagIds` に並べる（本物と同じ意味。
 * 重複は除く）。居るタグは系列が無いだけで、無いタグとは扱わない。
 */
function emptyHistory(
	registry: ChronoGazerRegistry,
	query: URLSearchParams
): Readout<CollectHistory> {
	const ids = (query.get('tagIds') ?? '')
		.split(',')
		.map((s) => Number(s))
		.filter((n) => Number.isInteger(n) && !registry.tag(n));
	return {
		state: 'ready',
		data: {
			fromMs: Number(query.get('fromMs') ?? 0),
			toMs: Number(query.get('toMs') ?? 0),
			series: [],
			unknownTagIds: [...new Set(ids)]
		}
	};
}

function idOf(req: DemoRequest): number {
	return Number(req.params.id);
}

export function createChronoGazerRoutes(
	registry: ChronoGazerRegistry = new ChronoGazerRegistry()
): DemoHttpRoute[] {
	// 収集は動かさないので、外れる設定は無い。
	const configExclusions: ExclusionView[] = [];
	return [
		{ method: 'GET', path: '/api/drivers', handle: () => json(registry.drivers) },
		{ method: 'GET', path: '/api/config-exclusions', handle: () => json(configExclusions) },
		{ method: 'GET', path: '/api/display-groups', handle: () => json(registry.displayGroups) },
		{
			method: 'GET',
			path: '/api/collect/values',
			handle: () => json(valuesOf(registry, Date.now()))
		},
		// 履歴の本当の模擬は次の PR。`/monitor` が最初のグループで読むので、系列の無い空の履歴を返す
		// （本物の `core/src/rest.rs` の collect_history の試験と同じ形。要求の期間はそのまま返す）。
		{
			method: 'GET',
			path: '/api/collect/history',
			handle: (req) => json(emptyHistory(registry, req.url.searchParams))
		},

		// --- 接続 ---
		{ method: 'GET', path: '/api/plc-connections', handle: () => json(registry.connections) },
		{
			method: 'POST',
			path: '/api/plc-connections',
			handle: guarded(async (req) =>
				json(registry.createConnection(await req.json<PlcConnectionInput>()), { status: 201 })
			)
		},
		{
			method: 'PUT',
			path: '/api/plc-connections/:id',
			handle: guarded(async (req) =>
				json(registry.updateConnection(idOf(req), await req.json<PlcConnectionInput>()))
			)
		},
		{
			method: 'DELETE',
			path: '/api/plc-connections/:id',
			handle: guarded((req) => {
				registry.deleteConnection(idOf(req));
				return noContent();
			})
		},

		// --- 収集グループ ---
		{ method: 'GET', path: '/api/collection-groups', handle: () => json(registry.groups) },
		{
			method: 'POST',
			path: '/api/collection-groups',
			handle: guarded(async (req) =>
				json(registry.createGroup(await req.json<CollectionGroupInput>()), { status: 201 })
			)
		},
		{
			method: 'PUT',
			path: '/api/collection-groups/:id',
			handle: guarded(async (req) =>
				json(registry.updateGroup(idOf(req), await req.json<CollectionGroupInput>()))
			)
		},
		{
			method: 'DELETE',
			path: '/api/collection-groups/:id',
			handle: guarded((req) => {
				registry.deleteGroup(idOf(req));
				return noContent();
			})
		},

		// --- タグ ---
		{ method: 'GET', path: '/api/tags', handle: () => json(registry.tags) },
		{
			method: 'POST',
			path: '/api/tags',
			handle: guarded(async (req) =>
				json(registry.createTag(await req.json<TagInput>()), { status: 201 })
			)
		},
		{
			method: 'PUT',
			path: '/api/tags/:id',
			handle: guarded(async (req) =>
				json(registry.updateTag(idOf(req), await req.json<TagInput>()))
			)
		},
		{
			method: 'DELETE',
			path: '/api/tags/:id',
			handle: guarded((req) => {
				registry.deleteTag(idOf(req));
				return noContent();
			})
		},

		// --- しきい値 ---
		{ method: 'GET', path: '/api/tag-thresholds', handle: () => json(registry.thresholds) },
		{
			method: 'GET',
			path: '/api/tag-thresholds/:tagId',
			handle: guarded((req) => json(registry.thresholdsOf(Number(req.params.tagId))))
		},
		{
			method: 'PUT',
			path: '/api/tag-thresholds/:tagId',
			handle: guarded(async (req) =>
				json(
					registry.updateThresholds(Number(req.params.tagId), await req.json<TagThresholdsInput>())
				)
			)
		}
	];
}
