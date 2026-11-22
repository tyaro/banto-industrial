/**
 * ChronoGazer の口の模擬（設計 docs/demo-pages-design.md §3.3・§6 の 4b）。`/tags` の画面が使う
 * 登録の口（接続・収集グループ・タグの一覧と登録・編集・削除、しきい値）と、起動で呼ばれる
 * 表示グループの一覧・収集から外れる設定。状態は {@link ChronoGazerRegistry}（このタブの
 * メモリだけ）。
 *
 * どの口が要るかは、デモのビルドで `/tags` を開き、模擬の「口がありません」の警告から
 * 拾った（2026-11-22・2026-11-23）。応答の型は各 `*Admin.ts` から取る（模擬の側で書き直さない）。
 * `/api/tags/from-hub`・`/api/hub*`・表示グループの編集は、今のデモの対象外で 501
 * （押して初めて呼ばれる）。履歴（`/api/collect/history`）と現在値（`/api/collect/values`）は
 * `/monitor` を開いたときの起動で呼ばれるので模擬する。
 */
import {
	demoSampleTime,
	demoTagHistory,
	demoTagValue,
	json,
	noContent,
	type DemoHttpRoute,
	type DemoRequest,
	validateDemoHistoryRequest
} from '@banto-industrial/demo-backend';
import type {
	CollectHistory,
	HistorySeries,
	Readout,
	ValuesResponse
} from '#lib/banto/collectAdmin.js';
import type {
	CollectionGroupInput,
	ExclusionView,
	PlcConnectionInput,
	Tag,
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

/** 収集の対象か（タグ・収集グループ・PLC 接続の全部が有効）。 */
function isCollected(registry: ChronoGazerRegistry, tag: Tag): boolean {
	if (!tag.enabled) return false;
	const group = registry.group(tag.collectionGroupId);
	if (!group?.enabled) return false;
	return !!registry.connections.find((c) => c.id === group.plcConnectionId)?.enabled;
}

/**
 * `GET /api/collect/values` の模擬: 収集の対象になるタグの現在値（タグ ID と時刻だけで決まる純関数）。
 * 本物は、タグ自身・収集グループ・PLC 接続のどれかが無効なら収集の構成に入れない
 * （`crates/banto-collect/src/config.rs` の `enabled` の絞り込み）ので、キーごと載せない。
 */
function valuesOf(registry: ChronoGazerRegistry, nowMs: number): ValuesResponse {
	const data: Extract<ValuesResponse, { state: 'ready' }>['data'] = {};
	for (const tag of registry.tags) {
		if (!isCollected(registry, tag)) continue;
		const group = registry.group(tag.collectionGroupId);
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

/** 要求のタグ ID（重複は除く。数でないものは落とす）。 */
function requestedTagIds(query: URLSearchParams): number[] {
	const ids = (query.get('tagIds') ?? '')
		.split(',')
		.filter((s) => s.trim() !== '')
		.map((s) => Number(s));
	return [...new Set(ids)];
}

/**
 * `GET /api/collect/history` の模擬（本物は `core/src/collect/history.rs`）。検証は本物と同じ 422、
 * 居ないタグは `unknownTagIds`（本物は「レジストリに居ない」だけ。無効なタグは系列に出て、欠測の点になる）。
 * 値はタグ ID と時刻だけで決まる純関数（`packages/demo-backend/src/tagHistory.ts`）。
 */
function historyOf(
	registry: ChronoGazerRegistry,
	query: URLSearchParams,
	nowMs: number
): Readout<CollectHistory> {
	const ids = requestedTagIds(query);
	const fromMs = Number(query.get('fromMs'));
	const toMs = Number(query.get('toMs'));
	const bins = Number(query.get('bins'));
	const errors = validateDemoHistoryRequest(ids, fromMs, toMs, bins);
	if (errors.length > 0) throw new DemoValidationError(errors);

	const series: HistorySeries[] = [];
	const unknownTagIds: number[] = [];
	for (const id of ids) {
		const tag = registry.tag(id);
		if (!tag) {
			unknownTagIds.push(id);
			continue;
		}
		const group = registry.group(tag.collectionGroupId);
		const { binMs, points } = demoTagHistory(
			{
				id: tag.id,
				dataType: tag.dataType,
				decimals: tag.decimals,
				periodMs: group?.periodMs,
				collected: isCollected(registry, tag)
			},
			fromMs,
			toMs,
			bins,
			nowMs
		);
		// `simulation` は常に false（本物はシミュレーション接続の値を記録せず null にするが、デモは模擬の値を出す）。
		series.push({ tagId: id, simulation: false, binMs, points });
	}
	return { state: 'ready', data: { fromMs, toMs, series, unknownTagIds } };
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
		// `/monitor` が最初のグループで読む。
		{
			method: 'GET',
			path: '/api/collect/history',
			handle: guarded((req) => {
				// E2E が「模擬の履歴の応答を実際に返した」ことを待つ印（デモのビルドにだけある模擬のコード）。
				console.debug('demo: 履歴を返しました');
				return json(historyOf(registry, req.url.searchParams, Date.now()));
			})
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
