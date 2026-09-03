/**
 * T18-3b 一括操作（docs/banto-hub-t18-design.md「T18-3b 一括操作
 * （TAG-UX-D 中）」: 複数選択＋一括有効/無効・グループ移動。一括削除は
 * 当時は参照影響・復旧方法が未確定のためバックログ扱いで対象外だった
 * （T19 S2-c1、UX-37 でこのファイルに追加 - {@link formatTagsBulkDeleteConfirmMessage}
 * 参照。既存 `delete`/`delete_tx` と同じ判定を id ごとに適用するだけの
 * 一括削除であり、参照影響（演算タグの式参照）は単票削除と同じく
 * ハードブロックしない設計 - `crates/banto-tags/src/tag.rs::TagService::delete_batch_tx`
 * の doc comment 参照）。
 *
 * `tagCsv.ts`/`tagDuplicate.ts` と同じ方針の依存ゼロ純関数群 -
 * `+page.svelte` 側の `FormState` には依存せず、`tagRegistryAdmin.ts` の
 * wire 型（{@link Tag}/{@link TagInput}/{@link BatchTagUpdateRow}）だけを
 * 扱う。テストは同ディレクトリの `tagBulkOps.test.ts`。
 *
 * `POST /api/tags/batch-update`（`updateTagsBatch`）に渡す行は単票 PUT と
 * 同じ全フィールドを要求する — サーバー実装（`TagBatchUpdatePayload`）が
 * `#[serde(flatten)]` で `TagPayload` をそのまま展開するため、部分更新
 * ではない。{@link buildBulkEnableRows}/{@link buildBulkMoveRows} は、
 * 選択タグそれぞれの既存の全フィールドをそのまま {@link TagInput} へ
 * 写しつつ、対象フィールド（`enabled`/`collectionGroupId`）だけを差し
 * 替える。`id`/`expectedRevision` は選択時点の {@link Tag.id}/
 * {@link Tag.revision} をそのまま使う - バックエンドは all-or-nothing の
 * ため、どれか1件でも revision が古ければ全体が `ok: false` で無書込に
 * なる（単票更新の 409 とは違う集約規則、
 * `apps/banto-hub/core/src/rest.rs::tags_batch_update` の doc comment
 * 参照）。
 */
import type { BatchTagUpdateRow, Tag, TagInput } from './tagRegistryAdmin';

/** {@link Tag} の全フィールドを {@link TagInput} へそのまま写す（`id`/`revision` を除く）。 */
function tagToInput(tag: Tag): TagInput {
	return {
		name: tag.name,
		collectionGroupId: tag.collectionGroupId,
		address: tag.address,
		dataType: tag.dataType,
		stringLength: tag.stringLength,
		rawLo: tag.rawLo,
		rawHi: tag.rawHi,
		engLo: tag.engLo,
		engHi: tag.engHi,
		unit: tag.unit,
		decimals: tag.decimals,
		thresholdH: tag.thresholdH,
		thresholdHh: tag.thresholdHh,
		thresholdL: tag.thresholdL,
		thresholdLl: tag.thresholdLl,
		enabled: tag.enabled,
		writable: tag.writable,
		tagKind: tag.tagKind,
		expression: tag.expression,
		retain: tag.retain
	};
}

/**
 * 選択タグ全件を、`enabled` だけ差し替えた batch-update 行に変換する
 * （一括有効化/無効化）。空配列を渡せば空配列を返す。
 */
export function buildBulkEnableRows(tags: readonly Tag[], enabled: boolean): BatchTagUpdateRow[] {
	return tags.map((tag) => ({
		id: tag.id,
		expectedRevision: tag.revision,
		...tagToInput(tag),
		enabled
	}));
}

/**
 * 選択タグ全件を、`collectionGroupId` だけ差し替えた batch-update 行に
 * 変換する（一括グループ移動）。配置検証（`plc`/`computed`/`internal` の
 * 接続配置ルール、`banto_tags::tag::validate_tag_kind_placement`）は
 * サーバー側が正 - この関数自体は移動先が種別に整合するかを検証しない
 * （呼び出し側 `+page.svelte` が `groupsFor(kind)` で候補を絞り込み、
 * 種別混在の選択ではそもそもグループ移動 UI を無効化する -
 * {@link hasMixedTagKinds} 参照）。
 */
export function buildBulkMoveRows(
	tags: readonly Tag[],
	targetGroupId: number
): BatchTagUpdateRow[] {
	return tags.map((tag) => ({
		id: tag.id,
		expectedRevision: tag.revision,
		...tagToInput(tag),
		collectionGroupId: targetGroupId
	}));
}

/**
 * 選択タグに複数の {@link Tag.tagKind}（`plc`/`computed`/`internal`）が
 * 混在しているか。グループ移動 UI の gate に使う（有効/無効切替は種別
 * 混在でも実行できるため、こちらでは使わない）。
 */
export function hasMixedTagKinds(tags: readonly Tag[]): boolean {
	return new Set(tags.map((tag) => tag.tagKind)).size > 1;
}

/** {@link summarizeBulkChange} が対応する対象フィールド。今のところ一括操作はこの2つのみ。 */
export type BulkChangeField = 'enabled' | 'collectionGroupId';

export interface BulkChangeRow<T> {
	id: number;
	name: string;
	from: T;
	to: T;
	/** `from !== to`（この行が実際に値が変わる行かどうか）。 */
	changed: boolean;
}

export interface BulkChangeSummary<T> {
	/** 選択件数（全件、変更の有無に関わらず）。 */
	targetCount: number;
	/** 実際に値が変わる件数（既に目的値と一致する行は含まない）。 */
	changedCount: number;
	rows: BulkChangeRow<T>[];
}

/**
 * 一括操作の確認パネルが使う「対象N件・差分」サマリ。`field` の現在値と
 * `toValue` を選択タグごとに比較するだけの純粋な差分計算 - 表示用の整形
 * （`enabled` を「有効/無効」に、`collectionGroupId` をグループ名に、等）
 * は呼び出し側 `+page.svelte` の責務にする（このファイルは `groups`/
 * `connections` のようなページ側の状態を一切知らない）。
 */
export function summarizeBulkChange<T extends boolean | number>(
	tags: readonly Tag[],
	field: BulkChangeField,
	toValue: T
): BulkChangeSummary<T> {
	const rows: BulkChangeRow<T>[] = tags.map((tag) => {
		const from = tag[field] as T;
		return { id: tag.id, name: tag.name, from, to: toValue, changed: from !== toValue };
	});
	return {
		targetCount: tags.length,
		changedCount: rows.filter((row) => row.changed).length,
		rows
	};
}

/**
 * T19 S2-c1（UX-37）一括削除の確認パネル文言。`registryCascadeImpact.ts`
 * の接続/収集グループ削除確認文言と同じ2点方針に揃える（実装指示）:
 * 対象を明示すること、記録済みの履歴（収集データ）は残ることを必ず
 * 明示すること。タグ自体は子リソースを持たないため件数計算は不要 -
 * 選択件数をそのまま埋め込むだけの純関数。
 */
export function formatTagsBulkDeleteConfirmMessage(count: number): string {
	return [
		`選択した ${count} 件のタグを削除します。`,
		'',
		'記録済みの履歴（収集データ）は削除されません。'
	].join('\n');
}
