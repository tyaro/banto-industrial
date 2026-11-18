/**
 * #1179 PR6b（docs/chronogazer/chronogazer-ui-improvement-proposal.md §4、2026-11-18）:
 * 「表示グループ」ページの右ペイン「タグ選択」（`PenTagPicker.svelte`）の判断を、
 * DOM に依存しない純関数に出したもの。unit test で確かめる。
 *
 * 設計の要点:
 * - ペンに割り当てるタグの出どころは 2 つ。(a) ChronoGazer の Hub 以外の接続先
 *   （SLMP/Modbus など）→ 収集グループ → **登録済みの記録項目**（`tagId`）。
 *   (b) 「Hub」の根 → Hub の接続先（`HubTag.connection`）→ Hub の収集グループ
 *   （`HubTag.group`）→ カタログのタグ。Hub のタグは、外部名が Hub 接続の記録項目の
 *   `address` と一致すれば登録済み（`tagId` で入れる）、無ければ未登録（`hubTag` で入れる。
 *   保存で記録項目に登録される）。
 * - **ツリーにタグを常時展開しない**。木は「接続先 → 収集グループ」までで、タグの行は
 *   選んだ収集グループのぶんか、検索の結果だけを描く（数万件でも重くならない）。
 *   検索の結果は {@link MAX_RESULT_ROWS} 件までで打ち切る。
 * - Hub 接続の記録項目でカタログに無いもの（カタログが読めないときも）は
 *   「Hub（カタログに無い記録項目）」の収集グループに出し、選べるようにする。
 * - 行の状態は {@link rowState}: 追加できる / 追加済み（このグループのペンが使っている）/
 *   登録できない（理由つきで選べない）。「未登録の Hub タグ」は追加できる行の印
 *   （{@link rowMark}）で区別する。
 */
import type { TreeNode } from '@banto/tree-svelte';
import type { HubStatus, HubTag } from '#lib/banto/hubAdmin.js';
import { availabilityLabel, isRegistrableDataType } from '#lib/banto/hubRegistration.js';
import type {
	CollectionGroup,
	HubTagRow,
	PlcConnection,
	Tag
} from '#lib/banto/tagRegistryAdmin.js';

/** 検索の結果に描く行の上限（超えた分は「ほか N 件」）。 */
export const MAX_RESULT_ROWS = 200;

/** Hub の根のノード id。 */
export const HUB_ROOT_ID = 'hub:root';
/** 「Hub（カタログに無い記録項目）」の収集グループのノード id。 */
export const ORPHAN_NODE_ID = 'hub:orphan';
/** 同じ名前の表示。 */
export const ORPHAN_GROUP_LABEL = 'Hub（カタログに無い記録項目）';

/** タグ選択で読んだ Hub のカタログ（`HubCatalog` のうち使う所）。 */
export interface PickerHubCatalog {
	status: HubStatus;
	/** `null` = カタログを読めていない。 */
	tags: HubTag[] | null;
	/** Hub のカタログが「登録済み」とする外部名。 */
	registered: readonly string[];
}

export interface PickerCatalogInput {
	connections: readonly PlcConnection[];
	collectionGroups: readonly CollectionGroup[];
	tags: readonly Tag[];
	/** `null` = まだ読んでいない（読み込み中・読めなかった）。 */
	hub: PickerHubCatalog | null;
	/** Hub の根に出す状態の文言（{@link hubNoteOf}）。 */
	hubNote: string | null;
}

/** ペンへの足し方。`tag` = 登録済みの記録項目、`hub` = 保存で登録する Hub のタグ。 */
export type PickTarget =
	| { kind: 'tag'; tagId: number }
	| { kind: 'hub'; hubTag: HubTagRow; connection: string; group: string };

export interface PickerRow {
	/** 重複判定のキー（`tag:<id>` / `hub:<外部名>`、`groupsPageLogic.penKey` と同じ形）。 */
	key: string;
	name: string;
	/** Hub の外部名、または記録項目のアドレス。 */
	address: string;
	connection: string;
	group: string;
	unit: string | null;
	dataType: string;
	/** この行が属する収集グループのノード id。 */
	nodeId: string;
	/** `recorded` = 記録項目になっている、`unregistered` = 未登録の Hub タグ、`blocked` = 登録できない。 */
	kind: 'recorded' | 'unregistered' | 'blocked';
	/** `blocked` の理由。 */
	blockedReason: string | null;
	/** `blocked` のときは `null`。 */
	pick: PickTarget | null;
	/** 検索用（小文字）。 */
	search: string;
	/** Hub のカタログ由来か（`true` なら「記録中」の印を出せる）。 */
	fromHub: boolean;
}

export type PickerNodeData = {
	kind: 'connection' | 'group' | 'hub-root' | 'hub-connection' | 'hub-group' | 'orphan';
	/** 収集グループのノードだけ: この id で {@link PickerCatalog.rowsByNode} を引く。 */
	rowsKey: string | null;
	/** 左のツリーの行に添える短い状態（Hub の根の読み込み状態など）。 */
	note?: string | null;
};

export interface PickerCatalog {
	nodes: TreeNode<PickerNodeData>[];
	rowsByNode: Map<string, PickerRow[]>;
	allRows: PickerRow[];
	/** Hub 接続の記録項目: 外部名 → tagId。 */
	registeredByName: Map<string, number>;
}

const collator = new Intl.Collator('ja');

function searchText(parts: readonly (string | null | undefined)[]): string {
	return parts.filter(Boolean).join(' ').toLowerCase();
}

/** Hub の根に出す状態の文言（普通に読めて 1 件以上あるときは `null`）。 */
export function hubNoteOf(
	hub: PickerHubCatalog | null,
	loading: boolean,
	error: string | null,
	statusLabel: (status: HubStatus) => string
): string | null {
	if (loading) return '読み込み中…';
	if (error !== null) return `カタログを読めませんでした（${error}）`;
	if (hub === null) return null;
	if (hub.tags === null) return `カタログを読めませんでした（${statusLabel(hub.status)}）`;
	if (hub.tags.length === 0) return statusLabel(hub.status);
	return null;
}

/** タグ選択の材料（木・収集グループごとの行・全行）を作る。 */
export function buildPickerCatalog(input: PickerCatalogInput): PickerCatalog {
	const connectionById = new Map(input.connections.map((c) => [c.id, c]));
	const groupById = new Map(input.collectionGroups.map((g) => [g.id, g]));
	const rowsByNode = new Map<string, PickerRow[]>();
	const allRows: PickerRow[] = [];
	const push = (row: PickerRow): void => {
		allRows.push(row);
		const list = rowsByNode.get(row.nodeId);
		if (list) list.push(row);
		else rowsByNode.set(row.nodeId, [row]);
	};

	// Hub 接続の記録項目（外部名 → tagId）と、Hub 以外の接続の記録項目。
	const registeredByName = new Map<string, number>();
	const hubRecords: { tag: Tag; connection: PlcConnection; group: CollectionGroup }[] = [];
	for (const tag of input.tags) {
		const group = groupById.get(tag.collectionGroupId);
		const connection = group ? connectionById.get(group.plcConnectionId) : undefined;
		if (!group || !connection) continue;
		if (connection.protocol === 'hub') {
			registeredByName.set(tag.address, tag.id);
			hubRecords.push({ tag, connection, group });
		} else {
			push({
				key: `tag:${tag.id}`,
				name: tag.name,
				address: tag.address,
				connection: connection.name,
				group: group.name,
				unit: tag.unit,
				dataType: tag.dataType,
				nodeId: `rec:group:${group.id}`,
				kind: 'recorded',
				blockedReason: null,
				pick: { kind: 'tag', tagId: tag.id },
				search: searchText([
					tag.name,
					tag.address,
					connection.name,
					group.name,
					tag.unit,
					tag.dataType
				]),
				fromHub: false
			});
		}
	}

	// (a) Hub 以外の接続先 → 収集グループ。
	const nodes: TreeNode<PickerNodeData>[] = [];
	for (const connection of input.connections) {
		if (connection.protocol === 'hub') continue;
		const groups = input.collectionGroups.filter((g) => g.plcConnectionId === connection.id);
		nodes.push({
			id: `rec:conn:${connection.id}`,
			label: connection.name,
			data: { kind: 'connection', rowsKey: null },
			children: groups.map((group) => {
				const nodeId = `rec:group:${group.id}`;
				return {
					id: nodeId,
					label: `${group.name}（${rowsByNode.get(nodeId)?.length ?? 0}）`,
					data: { kind: 'group', rowsKey: nodeId }
				};
			})
		});
	}

	// (b) Hub: カタログ → 接続先 → 収集グループ。
	const catalogNames = new Set<string>();
	const registeredByHub = new Set(input.hub?.registered ?? []);
	const hubGroups = new Map<string, Map<string, string>>(); // connection → group → nodeId
	for (const tag of input.hub?.tags ?? []) {
		catalogNames.add(tag.externalName);
		const nodeId = `hub:group:${tag.connection}\u0000${tag.group}`;
		let groups = hubGroups.get(tag.connection);
		if (!groups) hubGroups.set(tag.connection, (groups = new Map()));
		groups.set(tag.group, nodeId);
		const tagId = registeredByName.get(tag.externalName);
		const base = {
			name: tag.name,
			address: tag.externalName,
			connection: tag.connection,
			group: tag.group,
			unit: tag.unit,
			dataType: tag.dataType,
			nodeId,
			search: searchText([
				tag.name,
				tag.externalName,
				tag.connection,
				tag.group,
				tag.unit,
				tag.dataType
			]),
			fromHub: true
		};
		if (tagId !== undefined) {
			push({
				...base,
				key: `tag:${tagId}`,
				kind: 'recorded',
				blockedReason: null,
				pick: { kind: 'tag', tagId }
			});
		} else if (registeredByHub.has(tag.externalName)) {
			push({
				...base,
				key: `hub:${tag.externalName}`,
				kind: 'blocked',
				blockedReason: '登録済みです（「一覧を更新」で記録項目を読み直してください）',
				pick: null
			});
		} else if (!isRegistrableDataType(tag.dataType)) {
			push({
				...base,
				key: `hub:${tag.externalName}`,
				kind: 'blocked',
				blockedReason: availabilityLabel(
					{ selectable: false, reason: 'unsupportedType' },
					tag.dataType
				),
				pick: null
			});
		} else {
			push({
				...base,
				key: `hub:${tag.externalName}`,
				kind: 'unregistered',
				blockedReason: null,
				pick: {
					kind: 'hub',
					hubTag: {
						externalName: tag.externalName,
						name: tag.name,
						dataType: tag.dataType,
						unit: tag.unit,
						decimals: tag.decimals
					},
					connection: tag.connection,
					group: tag.group
				}
			});
		}
	}
	// カタログに無い Hub 接続の記録項目（カタログが読めないときは全部）。
	for (const { tag, connection, group } of hubRecords) {
		if (catalogNames.has(tag.address)) continue;
		push({
			key: `tag:${tag.id}`,
			name: tag.name,
			address: tag.address,
			connection: connection.name,
			group: group.name,
			unit: tag.unit,
			dataType: tag.dataType,
			nodeId: ORPHAN_NODE_ID,
			kind: 'recorded',
			blockedReason: null,
			pick: { kind: 'tag', tagId: tag.id },
			search: searchText([
				tag.name,
				tag.address,
				connection.name,
				group.name,
				tag.unit,
				tag.dataType
			]),
			fromHub: false
		});
	}
	for (const list of rowsByNode.values()) {
		list.sort((a, b) => collator.compare(a.name, b.name));
	}

	const hubChildren: TreeNode<PickerNodeData>[] = [...hubGroups.entries()]
		.sort(([a], [b]) => collator.compare(a, b))
		.map(([connection, groups]) => ({
			id: `hub:conn:${connection}`,
			label: connection,
			data: { kind: 'hub-connection', rowsKey: null } as PickerNodeData,
			children: [...groups.entries()]
				.sort(([a], [b]) => collator.compare(a, b))
				.map(([group, nodeId]) => ({
					id: nodeId,
					label: `${group}（${rowsByNode.get(nodeId)?.length ?? 0}）`,
					data: { kind: 'hub-group', rowsKey: nodeId } as PickerNodeData
				}))
		}));
	const orphanCount = rowsByNode.get(ORPHAN_NODE_ID)?.length ?? 0;
	if (orphanCount > 0) {
		hubChildren.push({
			id: ORPHAN_NODE_ID,
			label: `${ORPHAN_GROUP_LABEL}（${orphanCount}）`,
			data: { kind: 'orphan', rowsKey: ORPHAN_NODE_ID }
		});
	}
	nodes.push({
		id: HUB_ROOT_ID,
		label: input.hubNote ? `Hub — ${input.hubNote}` : 'Hub',
		data: { kind: 'hub-root', rowsKey: null, note: input.hubNote },
		children: hubChildren
	});

	return { nodes, rowsByNode, allRows, registeredByName };
}

/** 行の状態。 */
export type RowState =
	{ kind: 'addable' } | { kind: 'added' } | { kind: 'blocked'; reason: string };

/** `used` = いま下書きのペンが使っているキー（置き換えのときは、置き換える本人のペンを除いておく）。 */
export function rowState(row: PickerRow, used: ReadonlySet<string>): RowState {
	if (row.kind === 'blocked')
		return { kind: 'blocked', reason: row.blockedReason ?? '追加できません' };
	if (used.has(row.key)) return { kind: 'added' };
	return { kind: 'addable' };
}

/** 状態の列の文言（追加済み・登録できない理由・未登録の印・記録中）。 */
export function rowMark(row: PickerRow, state: RowState): string {
	if (state.kind === 'blocked') return state.reason;
	if (state.kind === 'added') return '追加済み';
	if (row.kind === 'unregistered') return '未登録・保存で記録に追加';
	return row.fromHub ? '記録中' : '';
}

/** 検索（名前・外部名/アドレス・接続先・収集グループ・単位・型の部分一致、空白区切りは AND）。 */
export function searchRows(rows: readonly PickerRow[], query: string): PickerRow[] {
	const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length === 0) return [];
	return rows.filter((row) => words.every((word) => row.search.includes(word)));
}

/** 描く行を {@link MAX_RESULT_ROWS} 件までにする。 */
export function limitRows<T>(
	rows: readonly T[],
	limit: number = MAX_RESULT_ROWS
): { shown: T[]; hidden: number } {
	return rows.length <= limit
		? { shown: [...rows], hidden: 0 }
		: { shown: rows.slice(0, limit), hidden: rows.length - limit };
}

export function hiddenRowsMessage(hidden: number): string {
	return `ほか ${hidden} 件。検索を絞り込んでください`;
}

/**
 * 追加を止める理由（`null` なら追加できる）。`max` はペンの上限、`remaining` はあと何本
 * 足せるか。残りを超えるときは**一部だけ足さず**止める。
 */
export function addBlocker(count: number, remaining: number, max: number): string | null {
	if (remaining <= 0) {
		return `ペンは ${max} 本までです。追加するには、既存のペンを外してください。`;
	}
	if (count > remaining) {
		return `選んだ ${count} 件はあと ${remaining} 本を超えます。選択を減らしてください。`;
	}
	return null;
}

/** 選択（キーの集合）のうち、いま追加できる行を、並びのまま返す。 */
export function addableSelection(
	rows: readonly PickerRow[],
	selected: ReadonlySet<string>,
	used: ReadonlySet<string>
): PickerRow[] {
	return rows.filter((row) => selected.has(row.key) && rowState(row, used).kind === 'addable');
}

/** ペンの下書きに足す形（色は未指定）。 */
export function penFromRow(row: PickerRow): {
	tagId: number | null;
	hubTag: HubTagRow | null;
	hubSource: { connection: string; group: string } | null;
	colorSlot: null;
} {
	const pick = row.pick;
	if (pick === null) throw new Error('追加できない行です');
	return pick.kind === 'tag'
		? { tagId: pick.tagId, hubTag: null, hubSource: null, colorSlot: null }
		: {
				tagId: null,
				hubTag: pick.hubTag,
				hubSource: { connection: pick.connection, group: pick.group },
				colorSlot: null
			};
}

/** 右ペインの「今のタグ」の表示（名前・接続先・収集グループ・未登録の印）。 */
export interface PenTagSummary {
	name: string;
	connection: string | null;
	group: string | null;
	unregistered: boolean;
}

export function describePenTag(
	pen: {
		tagId: number | null;
		hubTag: HubTagRow | null;
		hubSource?: { connection: string; group: string } | null;
	},
	tagsById: ReadonlyMap<number, Tag>,
	collectionGroups: readonly CollectionGroup[],
	connections: readonly PlcConnection[]
): PenTagSummary | null {
	if (pen.hubTag !== null) {
		return {
			name: pen.hubTag.name,
			connection: pen.hubSource?.connection ?? null,
			group: pen.hubSource?.group ?? null,
			unregistered: true
		};
	}
	if (pen.tagId === null) return null;
	const tag = tagsById.get(pen.tagId);
	if (!tag) {
		return {
			name: `（見つからないタグ ID ${pen.tagId}）`,
			connection: null,
			group: null,
			unregistered: false
		};
	}
	const group = collectionGroups.find((g) => g.id === tag.collectionGroupId);
	const connection = group ? connections.find((c) => c.id === group.plcConnectionId) : undefined;
	return {
		name: tag.name,
		connection: connection?.name ?? null,
		group: group?.name ?? null,
		unregistered: false
	};
}
