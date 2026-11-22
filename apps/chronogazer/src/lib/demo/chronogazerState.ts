/**
 * ChronoGazer の模擬のレジストリ（設計 docs/demo-pages-design.md §3.3）。接続・収集グループ・
 * タグ・しきい値・表示グループをこのタブのメモリだけに持つ（読み直すと初期のデータに戻る）。
 * 口（`chronogazerRoutes.ts`）が同じ物を見る。型は各 `*Admin.ts` から取る。
 *
 * 検証は最小限の規則だけ（必須・重複・参照の存在・周期）。本物の細かい検証は再現しない。
 * 誤りの形は本物（`banto_tags` の検証・`apps/chronogazer/core/src/rest.rs`）に合わせる:
 * 検証は 422 `validation`、使われている接続・グループの削除も 422（`field: "id"`）、
 * `expectedRevision` の不一致は 409 で本文は `kind: validation`（`field: "expectedRevision"`）。
 * Hub と違い、接続・グループの削除は配下を道連れにせず、残っていれば断る。
 */
import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
import {
	ALLOWED_PERIOD_MS,
	type CollectionGroup,
	type CollectionGroupInput,
	type DriverInfo,
	type PlcConnection,
	type PlcConnectionInput,
	type Tag,
	type TagDataType,
	type TagInput
} from '#lib/banto/tagRegistryAdmin.js';
import type { TagThresholds, TagThresholdsInput } from '#lib/banto/tagThresholdsAdmin.js';
import { demoDrivers } from './chronogazerDrivers';
import { seedConnections, seedDisplayGroups, seedGroups, seedTags } from './chronogazerSeed';

export interface FieldError {
	field: string;
	message: string;
}

/** 検証に失敗した入力（口が `kind: validation` にする）。`status` は本物に合わせて 409 もある。 */
export class DemoValidationError extends Error {
	constructor(
		readonly fieldErrors: FieldError[],
		readonly status: 422 | 409 = 422
	) {
		super(fieldErrors.map((e) => e.message).join(' / '));
	}
}

/** 無い id（口が 404 `not_found` にする）。 */
export class DemoNotFoundError extends Error {
	constructor(
		readonly resource: string,
		readonly id: number
	) {
		super(`${resource}/${id}`);
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
	'f64'
];

const MAX_NAME_LEN = 100;

/** `expectedRevision` が合わない（本物と同じく 409・`kind: validation`）。 */
function revisionConflict(): DemoValidationError {
	return new DemoValidationError(
		[
			{
				field: 'expectedRevision',
				message: '他の人が先に変更しました。読み込み直してからやり直してください'
			}
		],
		409
	);
}

function nameErrors(name: string): FieldError[] {
	if (name === '') return [{ field: 'name', message: '名前は必須です' }];
	if (name.length > MAX_NAME_LEN) {
		return [{ field: 'name', message: `名前は${MAX_NAME_LEN}文字以内です` }];
	}
	return [];
}

export class ChronoGazerRegistry {
	connections: PlcConnection[] = seedConnections();
	groups: CollectionGroup[] = seedGroups();
	tags: Tag[] = seedTags();
	thresholds: TagThresholds[] = [];
	displayGroups: DisplayGroup[] = seedDisplayGroups();
	readonly drivers: DriverInfo[] = demoDrivers();
	#nextConnectionId = 1 + Math.max(0, ...this.connections.map((c) => c.id));
	#nextGroupId = 1 + Math.max(0, ...this.groups.map((g) => g.id));
	#nextTagId = 1 + Math.max(0, ...this.tags.map((t) => t.id));

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
		const name = (input.name ?? '').trim();
		const errors = nameErrors(name);
		if (errors.length === 0 && this.connections.some((c) => c.name === name && c.id !== selfId)) {
			errors.push({ field: 'name', message: '同じ名前の接続があります' });
		}
		// `hub` は Hub 接続の「有効」の切り替えの更新でだけ送られる（ドライバの一覧には出ない）。
		if (input.protocol !== 'hub' && !this.drivers.some((d) => d.id === input.protocol)) {
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
			simulation: input.simulation ?? false,
			settings: { ...input.settings }
		};
		this.connections = [...this.connections, created];
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
			simulation: input.simulation ?? current.simulation,
			settings: { ...input.settings }
		};
		this.connections = this.connections.map((c) => (c.id === id ? updated : c));
		return updated;
	}

	/** 収集グループが残っていれば断る（本物の `delete`。件数を文言に入れる）。 */
	deleteConnection(id: number): void {
		if (!this.connection(id)) throw new DemoNotFoundError('plc-connections', id);
		const count = this.groups.filter((g) => g.plcConnectionId === id).length;
		if (count > 0) {
			throw new DemoValidationError([
				{
					field: 'id',
					message: `この接続を使用している収集グループが${count}件あるため削除できません`
				}
			]);
		}
		this.connections = this.connections.filter((c) => c.id !== id);
	}

	// --- 収集グループ -----------------------------------------------------

	#validateGroup(input: CollectionGroupInput, selfId: number | null): void {
		const name = (input.name ?? '').trim();
		const errors = nameErrors(name);
		if (
			errors.length === 0 &&
			this.groups.some(
				(g) => g.name === name && g.plcConnectionId === input.plcConnectionId && g.id !== selfId
			)
		) {
			errors.push({ field: 'name', message: '同じ接続に同じ名前のグループがあります' });
		}
		if (!this.connection(input.plcConnectionId)) {
			errors.push({ field: 'plcConnectionId', message: '接続が見つかりません' });
		}
		if (!ALLOWED_PERIOD_MS.includes(input.periodMs)) {
			errors.push({
				field: 'periodMs',
				message: `周期は ${ALLOWED_PERIOD_MS.join('・')} ms のいずれかです`
			});
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
			enabled: input.enabled
		};
		this.groups = [...this.groups, created];
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
			enabled: input.enabled
		};
		this.groups = this.groups.map((g) => (g.id === id ? updated : g));
		return updated;
	}

	/** タグが残っていれば断る（本物の `delete`）。 */
	deleteGroup(id: number): void {
		if (!this.group(id)) throw new DemoNotFoundError('collection-groups', id);
		const count = this.tags.filter((t) => t.collectionGroupId === id).length;
		if (count > 0) {
			throw new DemoValidationError([
				{ field: 'id', message: `このグループに属するタグが${count}件あるため削除できません` }
			]);
		}
		this.groups = this.groups.filter((g) => g.id !== id);
	}

	// --- タグ -----------------------------------------------------------

	#validateTag(input: TagInput, selfId: number | null): void {
		const name = (input.name ?? '').trim();
		const errors = nameErrors(name);
		const group = this.group(input.collectionGroupId);
		if (!group) errors.push({ field: 'collectionGroupId', message: 'グループが見つかりません' });
		else if (
			errors.length === 0 &&
			this.tags.some((t) => t.name === name && t.collectionGroupId === group.id && t.id !== selfId)
		) {
			errors.push({ field: 'name', message: '同じグループに同じ名前のタグがあります' });
		}
		if (!DATA_TYPES.includes(input.dataType)) {
			errors.push({ field: 'dataType', message: 'データ型が正しくありません' });
		}
		if ((input.address ?? '').trim() === '') {
			errors.push({ field: 'address', message: 'アドレスは必須です' });
		}
		if (!Number.isInteger(input.decimals) || input.decimals < 0 || input.decimals > 6) {
			errors.push({ field: 'decimals', message: '小数の桁は 0〜6 です' });
		}
		if (errors.length > 0) throw new DemoValidationError(errors);
	}

	#buildTag(id: number, input: TagInput, revision: number): Tag {
		return {
			id,
			name: input.name.trim(),
			collectionGroupId: input.collectionGroupId,
			address: input.address.trim(),
			dataType: input.dataType,
			rawLo: input.rawLo ?? null,
			rawHi: input.rawHi ?? null,
			engLo: input.engLo ?? null,
			engHi: input.engHi ?? null,
			unit: input.unit ?? null,
			decimals: input.decimals,
			enabled: input.enabled,
			revision
		};
	}

	createTag(input: TagInput): Tag {
		this.#validateTag(input, null);
		const created = this.#buildTag(this.#nextTagId++, input, 1);
		this.tags = [...this.tags, created];
		return created;
	}

	updateTag(id: number, input: TagInput): Tag {
		const current = this.tag(id);
		if (!current) throw new DemoNotFoundError('tags', id);
		if (input.expectedRevision !== undefined && input.expectedRevision !== current.revision) {
			throw revisionConflict();
		}
		this.#validateTag(input, id);
		const updated = this.#buildTag(id, input, current.revision + 1);
		this.tags = this.tags.map((t) => (t.id === id ? updated : t));
		return updated;
	}

	/**
	 * タグを消す。そのしきい値も消す。表示グループのペンに使われていれば、参照元を並べて 422 で断り
	 * 何も消さない（本物 `display_groups.rs` の `delete_tag_unless_referenced`。自由トレンドは模擬しない）。
	 */
	deleteTag(id: number): void {
		if (!this.tag(id)) throw new DemoNotFoundError('tags', id);
		const referrers = this.displayGroups.filter((g) => g.pens.some((p) => p.tagId === id));
		if (referrers.length > 0) {
			const names = referrers.map((g) => `「${g.name}」`).join('');
			throw new DemoValidationError([
				{
					field: 'displayGroups',
					message: `このタグは表示グループ${names}のペンに割り当てられているため削除できません。先に「表示グループ」ページでペンから外してください`
				}
			]);
		}
		this.tags = this.tags.filter((t) => t.id !== id);
		this.thresholds = this.thresholds.filter((t) => t.tagId !== id);
	}

	// --- しきい値 ---------------------------------------------------------

	/** 保存の無いタグは全部 `null`・`revision: 0`。 */
	thresholdsOf(tagId: number): TagThresholds {
		if (!this.tag(tagId)) throw new DemoNotFoundError('tags', tagId);
		return (
			this.thresholds.find((t) => t.tagId === tagId) ?? {
				tagId,
				thresholdLl: null,
				thresholdL: null,
				thresholdH: null,
				thresholdHh: null,
				revision: 0
			}
		);
	}

	updateThresholds(tagId: number, input: TagThresholdsInput): TagThresholds {
		const current = this.thresholdsOf(tagId);
		if (input.expectedRevision !== undefined && input.expectedRevision !== current.revision) {
			throw revisionConflict();
		}
		// 本物（`apps/chronogazer/core/src/tag_thresholds.rs` の `validate_threshold_order`）と同じ:
		// 設定したものだけを順に比べ、`LL <= L <= H <= HH`（等しいのは可）。誤りは後ろの欄に付ける。
		const entries: [string, number | null | undefined][] = [
			['thresholdLl', input.thresholdLl],
			['thresholdL', input.thresholdL],
			['thresholdH', input.thresholdH],
			['thresholdHh', input.thresholdHh]
		];
		const set = entries.filter((e): e is [string, number] => e[1] !== null && e[1] !== undefined);
		for (let i = 1; i < set.length; i++) {
			const [prevField, prevValue] = set[i - 1];
			const [field, value] = set[i];
			if (prevValue > value) {
				throw new DemoValidationError([{ field, message: `${prevField} 以上の値にしてください` }]);
			}
		}
		const updated: TagThresholds = {
			tagId,
			thresholdLl: input.thresholdLl ?? null,
			thresholdL: input.thresholdL ?? null,
			thresholdH: input.thresholdH ?? null,
			thresholdHh: input.thresholdHh ?? null,
			revision: current.revision + 1
		};
		this.thresholds = [...this.thresholds.filter((t) => t.tagId !== tagId), updated];
		return updated;
	}
}
