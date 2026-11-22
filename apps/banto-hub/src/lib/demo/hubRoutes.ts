/**
 * Hub の口の模擬（設計 docs/demo-pages-design.md §3.3）。`/tags` の画面が使う登録の口
 * （接続・収集グループ・タグの一覧と登録・編集・削除、単体と一括、アドレスの解析）と、
 * 値の購読の前提の catalog。状態は {@link HubRegistry}（このタブのメモリだけ）。
 *
 * どの口が要るかは、デモのビルドで `/tags` を開き、模擬の「口がありません」の警告から
 * 拾った（2026-11-22・2026-11-23）。応答の型は各 `*Admin.ts` から取る（模擬の側で書き直さない）。
 * 本物の Hub にあって、ここに無い口（接続テスト・時計・診断・設定パッケージなど）は 501
 * （デモの対象外）になる。
 */
import {
	json,
	noContent,
	type DemoHttpRoute,
	type DemoRequest
} from '@banto-industrial/demo-backend';
import type { ListParams, ListResult } from '@banto/admin-core';
import type { CommissioningStatus } from '#lib/banto/commissioning.js';
import type { RawStatusResponse } from '#lib/banto/hubStatus.js';
import type { PendingChange } from '#lib/banto/pendingChangesAdmin.js';
import type {
	CollectionGroupInput,
	ParseAddressItem,
	PlcConnectionInput,
	PlcConnectionTestResult,
	Tag,
	TagInput
} from '#lib/banto/tagRegistryAdmin.js';
import {
	DemoConflictError,
	DemoNotFoundError,
	DemoRevisionConflictError,
	DemoValidationError,
	HubRegistry
} from './hubState';

/** `GET /api/status` の模擬（接続は登録の中身から、収集は動いている・外部への出力は無効）。 */
function statusOf(registry: HubRegistry): RawStatusResponse {
	return {
		version: 'demo',
		revision: registry.revision,
		lastConfigError: null,
		connections: [],
		writeEnabled: false,
		writeWasEnabledBeforeRestart: false,
		mqtt: { enabled: false, connected: false },
		grpc: { enabled: false, port: 0 },
		collectionState: 'running',
		collectionMode: 'configured',
		lastRuntimeError: null,
		system: {
			cpuPercent: 0,
			processMemoryBytes: 0,
			hostMemoryUsedBytes: 0,
			hostMemoryTotalBytes: 0
		},
		dbSource: [],
		ingest: [],
		sink: { sidecar: { state: 'unknown', lastSeenAt: null }, groups: [] }
	};
}

/** 登録の失敗を本物の Hub と同じ形の応答にする。 */
function failure(error: unknown): Response {
	if (error instanceof DemoValidationError) {
		return json({ kind: 'validation', field_errors: error.fieldErrors }, { status: 422 });
	}
	if (error instanceof DemoNotFoundError) {
		return json(
			{ kind: 'not_found', resource: error.resource, id: String(error.id) },
			{ status: 404 }
		);
	}
	if (error instanceof DemoConflictError) {
		return json({ kind: 'conflict', message: error.message }, { status: 409 });
	}
	if (error instanceof DemoRevisionConflictError) {
		return json(
			{ error: 'tag_revision_conflict', message: error.message, tag: error.current },
			{ status: 409 }
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

function idOf(req: DemoRequest): number {
	return Number(req.params.id);
}

/** `POST /api/tags/list` の絞り込み・並べ替え・ページング（`ListParams`）の最小の実装。 */
function listTags(tags: Tag[], params: ListParams): ListResult<Tag> {
	const search = params.search?.trim().toLowerCase();
	let rows = tags.filter((tag) => {
		if (search) {
			const hit = [tag.name, tag.comment, tag.address, tag.unit ?? ''].some((text) =>
				text.toLowerCase().includes(search)
			);
			if (!hit) return false;
		}
		return (params.filters ?? []).every((filter) => {
			const value = (tag as unknown as Record<string, unknown>)[filter.field];
			const operand = filter.value;
			switch (filter.op) {
				case 'eq':
					return value === operand;
				case 'ne':
					return value !== operand;
				case 'contains':
					return String(value ?? '')
						.toLowerCase()
						.includes(String(operand ?? '').toLowerCase());
				case 'starts_with':
					return String(value ?? '')
						.toLowerCase()
						.startsWith(String(operand ?? '').toLowerCase());
				case 'in':
					return Array.isArray(operand) && operand.includes(value);
				case 'is_null':
					return value === null || value === undefined;
				case 'not_null':
					return value !== null && value !== undefined;
				case 'lt':
					return (value as number) < (operand as number);
				case 'lte':
					return (value as number) <= (operand as number);
				case 'gt':
					return (value as number) > (operand as number);
				case 'gte':
					return (value as number) >= (operand as number);
				default:
					return true;
			}
		});
	});
	for (const sort of [...(params.sort ?? [])].reverse()) {
		const sign = sort.direction === 'desc' ? -1 : 1;
		rows = [...rows].sort((a, b) => {
			const x = (a as unknown as Record<string, unknown>)[sort.field];
			const y = (b as unknown as Record<string, unknown>)[sort.field];
			if (x === y) return 0;
			if (x === null || x === undefined) return 1;
			if (y === null || y === undefined) return -1;
			return (x < y ? -1 : 1) * sign;
		});
	}
	const totalCount = rows.length;
	if (params.pagination) {
		rows = rows.slice(params.pagination.offset, params.pagination.offset + params.pagination.limit);
	}
	return { rows, totalCount };
}

export function createHubRoutes(registry: HubRegistry = new HubRegistry()): DemoHttpRoute[] {
	const pendingChanges: PendingChange[] = [];
	// ログイン済みの利用者として入るので、試運転（ロックダウン前）の表示は出さない。
	const commissioning: CommissioningStatus = { lockedDown: true };
	return [
		{ method: 'GET', path: '/api/drivers', handle: () => json(registry.drivers) },
		{ method: 'GET', path: '/api/pending-changes', handle: () => json(pendingChanges) },
		{ method: 'GET', path: '/api/commissioning/status', handle: () => json(commissioning) },
		{ method: 'GET', path: '/api/status', handle: () => json(statusOf(registry)) },
		{ method: 'GET', path: '/api/tag-catalog', handle: () => json(registry.catalog()) },

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
			// 接続テスト（保存前）。どこにもつながないので、いつも成功にする。
			method: 'POST',
			path: '/api/plc-connections/test',
			handle: () => {
				const result: PlcConnectionTestResult = { ok: true, elapsedMs: 12, error: null };
				return json(result);
			}
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
			method: 'POST',
			path: '/api/tags/list',
			handle: guarded(async (req) => json(listTags(registry.tags, await req.json<ListParams>())))
		},
		{
			method: 'GET',
			path: '/api/tags/group-counts',
			handle: () => json(registry.groupCounts())
		},
		{
			method: 'POST',
			path: '/api/tags/parse-addresses',
			handle: guarded(async (req) => {
				const body = await req.json<{ connectionId: number; items: ParseAddressItem[] }>();
				return json({
					items: body.items.map((item) => registry.parseAddress(body.connectionId, item))
				});
			})
		},
		{
			method: 'POST',
			path: '/api/tags/batch',
			handle: guarded(async (req) => {
				const body = await req.json<{ tags: TagInput[]; dryRun: boolean }>();
				return json(registry.createTagsBatch(body.tags, body.dryRun === true));
			})
		},
		{
			method: 'POST',
			path: '/api/tags/batch-update',
			handle: guarded(async (req) => {
				const body = await req.json<{ tags: (TagInput & { id: number })[]; dryRun: boolean }>();
				return json(registry.updateTagsBatch(body.tags, body.dryRun === true));
			})
		},
		{
			method: 'POST',
			path: '/api/tags/batch-delete',
			handle: guarded(async (req) => {
				const body = await req.json<{ ids: number[] }>();
				return json(registry.deleteTagsBatch(body.ids));
			})
		},
		{
			method: 'GET',
			path: '/api/tags/:id',
			handle: guarded((req) => {
				const tag = registry.tag(idOf(req));
				if (!tag) throw new DemoNotFoundError('tags', idOf(req));
				return json(tag);
			})
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
		}
	];
}
