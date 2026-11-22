/**
 * Hub の模擬のレジストリ（設計 docs/demo-pages-design.md §3.3）。接続・収集グループ・タグを
 * このタブのメモリだけに持つ（読み直すと初期のデータに戻る）。口（`hubRoutes.ts`）と
 * WebSocket（`hubStream.ts`）が同じ物を見る。型は各 `*Admin.ts` から取る。
 *
 * 検証は最小限の規則だけ（必須・重複・参照の存在・桁数）。本物の Hub の細かい検証は再現しない。
 * `revision` は登録の変更のたびに進み、変更を知らせる listener（`config_changed`）を呼ぶ。
 */
import type {
	CollectionGroup,
	CollectionGroupInput,
	DriverInfo,
	ParsedAddress,
	PlcConnection,
	PlcConnectionInput,
	Tag,
	TagDataType,
	TagInput
} from '#lib/banto/tagRegistryAdmin.js';
import { demoDrivers } from './hubDrivers';
import { seedConnections, seedGroups, seedTags } from './hubSeed';

export interface FieldError {
	field: string;
	message: string;
}

/** 検証に失敗した入力（口が 422 `validation` にする）。 */
export class DemoValidationError extends Error {
	constructor(readonly fieldErrors: FieldError[]) {
		super(fieldErrors.map((e) => e.message).join(' / '));
	}
}

/** 重複・参照の残りなどで断る（口が 409 `conflict` にする）。 */
export class DemoConflictError extends Error {}

/** 無い id（口が 404 `not_found` にする）。 */
export class DemoNotFoundError extends Error {
	constructor(
		readonly resource: string,
		readonly id: number
	) {
		super(`${resource}/${id}`);
	}
}

/** `expectedRevision` が合わない（口が 409 `tag_revision_conflict` にする）。 */
export class DemoRevisionConflictError extends Error {
	constructor(readonly current: Tag) {
		super('他の人が先にこのタグを変更しました');
	}
}

const DATA_TYPES: readonly TagDataType[] = [
	'bit',
	'i16',
	'u16',
	'i32',
	'u32',
	'f32',
	'i64',
	'u64',
	'f64',
	'string'
];

export type RegistryListener = (revision: number) => void;

export class HubRegistry {
	connections: PlcConnection[] = seedConnections();
	groups: CollectionGroup[] = seedGroups();
	tags: Tag[] = seedTags();
	readonly drivers: DriverInfo[] = demoDrivers();
	revision = 1;
	#nextConnectionId = 1 + Math.max(0, ...this.connections.map((c) => c.id));
	#nextGroupId = 1 + Math.max(0, ...this.groups.map((g) => g.id));
	#nextTagId = 1 + Math.max(0, ...this.tags.map((t) => t.id));
	readonly #listeners = new Set<RegistryListener>();

	/** 登録が変わったときの通知（解除の関数を返す）。 */
	onChange(listener: RegistryListener): () => void {
		this.#listeners.add(listener);
		return () => this.#listeners.delete(listener);
	}

	#changed(): void {
		this.revision += 1;
		for (const listener of [...this.#listeners]) listener(this.revision);
	}

	connection(id: number): PlcConnection | undefined {
		return this.connections.find((c) => c.id === id);
	}

	group(id: number): CollectionGroup | undefined {
		return this.groups.find((g) => g.id === id);
	}

	tag(id: number): Tag | undefined {
		return this.tags.find((t) => t.id === id);
	}

	// --- 接続 -----------------------------------------------------------

	#validateConnection(input: PlcConnectionInput, selfId: number | null): void {
		const errors: FieldError[] = [];
		const name = (input.name ?? '').trim();
		if (name === '') errors.push({ field: 'name', message: '名前は必須です' });
		else if (/[.*]/.test(name)) {
			errors.push({ field: 'name', message: '名前に . と * は使えません' });
		} else if (this.connections.some((c) => c.name === name && c.id !== selfId)) {
			errors.push({ field: 'name', message: '同じ名前の接続があります' });
		}
		if (!this.drivers.some((d) => d.id === input.protocol)) {
			errors.push({ field: 'protocol', message: 'このデモで使えるドライバではありません' });
		}
		if (errors.length > 0) throw new DemoValidationError(errors);
	}

	createConnection(input: PlcConnectionInput): PlcConnection {
		this.#validateConnection(input, null);
		const created: PlcConnection = {
			id: this.#nextConnectionId++,
			name: input.name.trim(),
			protocol: input.protocol,
			enabled: input.enabled,
			simulation: input.simulation,
			settings: { ...input.settings }
		};
		this.connections = [...this.connections, created];
		this.#changed();
		return created;
	}

	updateConnection(id: number, input: PlcConnectionInput): PlcConnection {
		const current = this.connection(id);
		if (!current) throw new DemoNotFoundError('plc-connections', id);
		this.#validateConnection(input, id);
		const updated: PlcConnection = {
			...current,
			name: input.name.trim(),
			protocol: input.protocol,
			enabled: input.enabled,
			simulation: input.simulation,
			settings: { ...input.settings }
		};
		this.connections = this.connections.map((c) => (c.id === id ? updated : c));
		this.#changed();
		return updated;
	}

	/** 配下のグループ・タグごと消す（本物の `cascade_delete_tx`。応答は 204 で、件数は監査にだけ載る）。 */
	deleteConnection(id: number): { deletedGroups: number; deletedTags: number } {
		if (!this.connection(id)) throw new DemoNotFoundError('plc-connections', id);
		const groupIds = new Set(this.groups.filter((g) => g.plcConnectionId === id).map((g) => g.id));
		const deletedTags = this.tags.filter((t) => groupIds.has(t.collectionGroupId)).length;
		this.tags = this.tags.filter((t) => !groupIds.has(t.collectionGroupId));
		this.groups = this.groups.filter((g) => !groupIds.has(g.id));
		this.connections = this.connections.filter((c) => c.id !== id);
		this.#changed();
		return { deletedGroups: groupIds.size, deletedTags };
	}

	// --- 収集グループ -----------------------------------------------------

	#validateGroup(input: CollectionGroupInput, selfId: number | null): void {
		const errors: FieldError[] = [];
		const name = (input.name ?? '').trim();
		if (name === '') errors.push({ field: 'name', message: '名前は必須です' });
		else if (/[.*]/.test(name)) {
			errors.push({ field: 'name', message: '名前に . と * は使えません' });
		} else if (
			this.groups.some(
				(g) => g.name === name && g.plcConnectionId === input.plcConnectionId && g.id !== selfId
			)
		) {
			errors.push({ field: 'name', message: '同じ接続に同じ名前のグループがあります' });
		}
		if (!this.connection(input.plcConnectionId)) {
			errors.push({ field: 'plcConnectionId', message: '接続が見つかりません' });
		}
		if (!Number.isFinite(input.periodMs) || input.periodMs < 100) {
			errors.push({ field: 'periodMs', message: '周期は 100ms 以上です' });
		}
		if (errors.length > 0) throw new DemoValidationError(errors);
	}

	createGroup(input: CollectionGroupInput): CollectionGroup {
		this.#validateGroup(input, null);
		const created: CollectionGroup = {
			id: this.#nextGroupId++,
			name: input.name.trim(),
			plcConnectionId: input.plcConnectionId,
			periodMs: input.periodMs,
			enabled: input.enabled,
			defaultWritable: input.defaultWritable,
			querySql: null
		};
		this.groups = [...this.groups, created];
		this.#changed();
		return created;
	}

	updateGroup(id: number, input: CollectionGroupInput): CollectionGroup {
		const current = this.group(id);
		if (!current) throw new DemoNotFoundError('collection-groups', id);
		this.#validateGroup(input, id);
		const updated: CollectionGroup = {
			...current,
			name: input.name.trim(),
			plcConnectionId: input.plcConnectionId,
			periodMs: input.periodMs,
			enabled: input.enabled,
			defaultWritable: input.defaultWritable
		};
		this.groups = this.groups.map((g) => (g.id === id ? updated : g));
		this.#changed();
		return updated;
	}

	/** 属するタグごと消す（本物の `cascade_delete_tx`。応答は 204 で、件数は監査にだけ載る）。 */
	deleteGroup(id: number): { deletedTags: number } {
		if (!this.group(id)) throw new DemoNotFoundError('collection-groups', id);
		const deletedTags = this.tags.filter((t) => t.collectionGroupId === id).length;
		this.tags = this.tags.filter((t) => t.collectionGroupId !== id);
		this.groups = this.groups.filter((g) => g.id !== id);
		this.#changed();
		return { deletedTags };
	}

	// --- タグ -----------------------------------------------------------

	/** 1 本のタグの入力の検証。`pending` は同じ一括の中の、まだ登録していない名前。 */
	validateTag(
		input: TagInput,
		selfId: number | null,
		pending: Tag[] = [],
		checkNameDup = true
	): FieldError[] {
		const errors: FieldError[] = [];
		const name = (input.name ?? '').trim();
		if (name === '') errors.push({ field: 'name', message: '名前は必須です' });
		else if (/[.*]/.test(name)) {
			errors.push({ field: 'name', message: '名前に . と * は使えません' });
		}
		const group = this.group(input.collectionGroupId);
		if (!group) errors.push({ field: 'collectionGroupId', message: 'グループが見つかりません' });
		else if (
			checkNameDup &&
			name !== '' &&
			[...this.tags, ...pending].some(
				(t) => t.name === name && t.collectionGroupId === group.id && t.id !== selfId
			)
		) {
			errors.push({ field: 'name', message: '同じグループに同じ名前のタグがあります' });
		}
		if (!DATA_TYPES.includes(input.dataType)) {
			errors.push({ field: 'dataType', message: 'データ型が正しくありません' });
		}
		if ((input.tagKind ?? 'plc') === 'plc' && (input.address ?? '').trim() === '') {
			errors.push({ field: 'address', message: 'アドレスは必須です' });
		}
		if (!Number.isInteger(input.decimals) || input.decimals < 0 || input.decimals > 6) {
			errors.push({ field: 'decimals', message: '小数の桁は 0〜6 です' });
		}
		if (input.dataType === 'string') {
			const len = input.stringLength;
			if (len == null || !Number.isInteger(len) || len < 1 || len > 128) {
				errors.push({ field: 'stringLength', message: '文字列の長さは 1〜128 です' });
			}
		}
		return errors;
	}

	#buildTag(id: number, input: TagInput, revision: number, current?: Tag): Tag {
		return {
			id,
			name: input.name.trim(),
			// 更新で省けば保存済みを残す。空文字なら消す（tagRegistryAdmin の約束）。
			comment: input.comment ?? current?.comment ?? '',
			collectionGroupId: input.collectionGroupId,
			address: (input.address ?? '').trim(),
			dataType: input.dataType,
			stringLength: input.dataType === 'string' ? (input.stringLength ?? null) : null,
			stringEncoding: input.stringEncoding ?? 'utf8',
			rawLo: input.rawLo ?? null,
			rawHi: input.rawHi ?? null,
			engLo: input.engLo ?? null,
			engHi: input.engHi ?? null,
			unit: input.unit ?? null,
			decimals: input.decimals,
			enabled: input.enabled,
			writable: input.writable ?? false,
			tagKind: input.tagKind ?? 'plc',
			expression: input.expression ?? null,
			retain: input.retain ?? false,
			revision
		};
	}

	createTag(input: TagInput): Tag {
		const errors = this.validateTag(input, null);
		if (errors.length > 0) throw new DemoValidationError(errors);
		const created = this.#buildTag(this.#nextTagId++, input, 1);
		this.tags = [...this.tags, created];
		this.#changed();
		return created;
	}

	updateTag(id: number, input: TagInput): Tag {
		const current = this.tag(id);
		if (!current) throw new DemoNotFoundError('tags', id);
		if (input.expectedRevision !== undefined && input.expectedRevision !== current.revision) {
			throw new DemoRevisionConflictError(current);
		}
		const errors = this.validateTag(input, id);
		if (errors.length > 0) throw new DemoValidationError(errors);
		const updated = this.#buildTag(id, input, current.revision + 1, current);
		this.tags = this.tags.map((t) => (t.id === id ? updated : t));
		this.#changed();
		return updated;
	}

	deleteTag(id: number): void {
		if (!this.tag(id)) throw new DemoNotFoundError('tags', id);
		this.tags = this.tags.filter((t) => t.id !== id);
		this.#changed();
	}

	/** 一括の作成。エラーがあれば 1 本も作らない。`dryRun` は検証だけ。 */
	createTagsBatch(inputs: TagInput[], dryRun: boolean) {
		const errors: { index: number; fieldErrors: FieldError[] }[] = [];
		const pending: Tag[] = [];
		let nextId = this.#nextTagId;
		inputs.forEach((input, index) => {
			const fieldErrors = this.validateTag(input, null, pending);
			if (fieldErrors.length > 0) errors.push({ index, fieldErrors });
			else pending.push(this.#buildTag(nextId++, input, 1));
		});
		if (errors.length > 0 || dryRun) {
			return { ok: errors.length === 0, dryRun, count: inputs.length, errors };
		}
		this.#nextTagId = nextId;
		this.tags = [...this.tags, ...pending];
		this.#changed();
		return { ok: true, dryRun, count: pending.length, errors, tags: pending };
	}

	updateTagsBatch(rows: (TagInput & { id: number })[], dryRun: boolean) {
		const errors: { index: number; id: number; fieldErrors: FieldError[] }[] = [];
		const updates: Tag[] = [];
		rows.forEach((row, index) => {
			const current = this.tag(row.id);
			if (!current) {
				errors.push({
					index,
					id: row.id,
					fieldErrors: [{ field: 'id', message: 'タグが見つかりません' }]
				});
				return;
			}
			// 名前の一意は、更新を当てた後の状態で下で見る（同じバッチの他の行を含む）。
			const fieldErrors = this.validateTag(row, row.id, [], false);
			// 単体 PUT と違い 409 ではなく行エラー（本物の update_batch_tx と同じ形）。
			if (row.expectedRevision !== undefined && row.expectedRevision !== current.revision) {
				fieldErrors.push({
					field: 'expectedRevision',
					message: '他のクライアントがこのタグを更新済みです。再読込してから保存してください。'
				});
			}
			if (fieldErrors.length > 0) errors.push({ index, id: row.id, fieldErrors });
			else updates.push(this.#buildTag(row.id, row, current.revision + 1, current));
		});
		if (errors.length === 0) {
			const after = new Map(this.tags.map((t) => [t.id, t]));
			for (const u of updates) after.set(u.id, u);
			const counts = new Map<string, number>();
			for (const t of after.values()) {
				const key = `${t.collectionGroupId}|${t.name}`;
				counts.set(key, (counts.get(key) ?? 0) + 1);
			}
			rows.forEach((row, index) => {
				const u = after.get(row.id)!;
				if ((counts.get(`${u.collectionGroupId}|${u.name}`) ?? 0) > 1) {
					errors.push({
						index,
						id: row.id,
						fieldErrors: [{ field: 'name', message: '同じグループに同じ名前のタグがあります' }]
					});
				}
			});
		}
		if (errors.length > 0 || dryRun) {
			return { ok: errors.length === 0, dryRun, count: rows.length, errors };
		}
		const byId = new Map(updates.map((t) => [t.id, t]));
		this.tags = this.tags.map((t) => byId.get(t.id) ?? t);
		this.#changed();
		return { ok: true, dryRun, count: updates.length, errors, tags: updates };
	}

	deleteTagsBatch(ids: number[]) {
		const errors = ids.flatMap((id, index) =>
			this.tag(id)
				? []
				: [{ index, id, fieldErrors: [{ field: 'id', message: 'タグが見つかりません' }] }]
		);
		if (errors.length > 0) return { ok: false, count: ids.length, errors };
		const drop = new Set(ids);
		this.tags = this.tags.filter((t) => !drop.has(t.id));
		this.#changed();
		return { ok: true, count: ids.length, errors };
	}

	groupCounts(): { collectionGroupId: number; tagCount: number }[] {
		return this.groups.map((g) => ({
			collectionGroupId: g.id,
			tagCount: this.tags.filter((t) => t.collectionGroupId === g.id).length
		}));
	}

	/** `接続名.グループ名.タグ名`（購読・値の `tag`）。 */
	externalName(tag: Tag): string | null {
		const group = this.group(tag.collectionGroupId);
		const connection = group ? this.connection(group.plcConnectionId) : undefined;
		return group && connection ? `${connection.name}.${group.name}.${tag.name}` : null;
	}

	/** `GET /api/tag-catalog`（camelCase の生の形。`tagMonitorAdmin.ts` が snake_case に直す）。 */
	catalog(): { revision: number; tags: unknown[] } {
		const entries = this.tags.flatMap((tag) => {
			const group = this.group(tag.collectionGroupId);
			const connection = group ? this.connection(group.plcConnectionId) : undefined;
			if (!group || !connection) return [];
			return [
				{
					externalName: `${connection.name}.${group.name}.${tag.name}`,
					tagKey: `tag:${tag.id}`,
					ids: [connection.id, group.id, tag.id],
					connection: connection.name,
					group: group.name,
					name: tag.name,
					address: tag.address,
					dataType: tag.dataType,
					unit: tag.unit,
					decimals: tag.decimals,
					periodMs: group.periodMs,
					enabled: tag.enabled && group.enabled && connection.enabled,
					writable: tag.writable,
					tagKind: tag.tagKind,
					expression: tag.expression,
					retain: tag.retain,
					simulation: connection.simulation
				}
			];
		});
		return { revision: this.revision, tags: entries };
	}

	/** アドレスの解析（最小限の規則: 形が合うかだけ。細かい範囲の検証は再現しない）。 */
	parseAddress(
		connectionId: number,
		item: {
			tagKind: string;
			address: string;
			dataType: string;
			stringLength?: number | null;
		}
	): ParsedAddress {
		const address = (item.address ?? '').trim();
		if (address === '') return { ok: false, message: 'アドレスを入力してください' };
		const connection = this.connection(connectionId);
		if (item.tagKind !== 'plc' || !connection) {
			return {
				ok: true,
				canonical: address,
				area: null,
				writableArea: null,
				index: null,
				bit: null,
				wordCount: null
			};
		}
		const wordCount = ['i32', 'u32', 'f32'].includes(item.dataType)
			? 2
			: ['i64', 'u64', 'f64'].includes(item.dataType)
				? 4
				: item.dataType === 'string'
					? Number.isInteger(item.stringLength) &&
						(item.stringLength as number) >= 1 &&
						(item.stringLength as number) <= 128
						? (item.stringLength as number)
						: null
					: 1;
		if (connection.protocol === 'modbus-tcp') {
			const m = /^([0134])(\d{4,5})$/.exec(address);
			if (!m) return { ok: false, message: 'Modbus のアドレスは 40001 のような形です' };
			return {
				ok: true,
				canonical: address,
				area: m[1],
				writableArea: m[1] === '0' || m[1] === '4',
				index: Number(m[2]) - 1,
				bit: null,
				wordCount
			};
		}
		const m = /^([A-Za-z]{1,2})(\d+)$/.exec(address);
		if (!m) return { ok: false, message: 'アドレスは D100・M10 のような形です' };
		const area = m[1].toUpperCase();
		return {
			ok: true,
			canonical: `${area}${m[2]}`,
			area,
			writableArea: true,
			index: Number(m[2]),
			bit: null,
			wordCount
		};
	}

	/**
	 * アドレスを動かす（`POST /api/tags/offset-addresses` の模擬）。`points` は型のワード数 × `n`、
	 * `words` は `n` ワード。動かした後は `parseAddress` で解析し直す（本物と同じ「動かして再検証」）。
	 * 末尾の数字は十進で扱う（`parseAddress` と同じ最小の規則）。接続が無い・範囲外・解析できない行は
	 * その行だけ `ok: false`。
	 */
	offsetAddress(item: {
		connectionId: number;
		tagKind?: string;
		address: string;
		dataType: string;
		stringLength?: number | null;
		n: number;
		unit: 'points' | 'words';
	}): ParsedAddress {
		const tagKind = item.tagKind ?? 'plc';
		const connection = this.connection(item.connectionId);
		if (!connection) return { ok: false, message: `接続 ${item.connectionId} が見つかりません` };
		const base = this.parseAddress(item.connectionId, {
			tagKind,
			address: item.address,
			dataType: item.dataType,
			stringLength: item.stringLength
		});
		if (!base.ok || tagKind !== 'plc') return base;
		const perPoint =
			item.dataType === 'string' ? Math.max(1, item.stringLength ?? 1) : (base.wordCount ?? 1);
		const step = item.unit === 'points' ? item.n * perPoint : item.n;
		const address = item.address.trim();
		const digits = /^([A-Za-z]{1,2})(\d+)$/.exec(address) ?? /^([0134])(\d{4,5})$/.exec(address);
		if (!digits) return { ok: false, message: 'アドレスを動かせません' };
		const next = Number(digits[2]) + step;
		if (
			!Number.isSafeInteger(next) ||
			next < 0 ||
			(connection.protocol === 'modbus-tcp' && next < 1)
		) {
			return { ok: false, message: 'アドレスの範囲外です' };
		}
		const moved = `${digits[1]}${String(next).padStart(digits[2].length, '0')}`;
		return this.parseAddress(item.connectionId, {
			tagKind,
			address: moved,
			dataType: item.dataType,
			stringLength: item.stringLength
		});
	}
}
