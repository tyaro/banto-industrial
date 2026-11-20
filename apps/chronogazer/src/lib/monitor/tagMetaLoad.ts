/**
 * タグ情報（単位・小数桁・収集周期）としきい値を読む（監視画面・
 * 自由トレンドの画面の共通部品、#1207・#1231 PR 8b1。監視画面に書いてあったものを切り出した）。
 *
 * #532: しきい値はタグではなく記録計の側の設定から読み、タグに添える。タグ情報としきい値は
 * **別の失敗の軸**（`applyTagMetaLoad`）: しきい値だけ読めなくてもタグの名前・単位・小数桁は使い、
 * しきい値は前の値を残さず「判定なし」にする。
 */
import {
	listCollectionGroups,
	listTags,
	type CollectionGroup,
	type Tag
} from '../banto/tagRegistryAdmin';
import { listTagThresholds, type TagThresholds } from '../banto/tagThresholdsAdmin';
import type { LoadResult } from './monitorLogic';

function errorText(err: unknown): string {
	return err instanceof Error ? err.message : String(err);
}

/**
 * タグ情報としきい値を読む（2 軸）。結果は読み終えた時点の状態に `applyTagMetaLoad` で反映する
 * （読み込みが重なっても、最後に読み終えたものが前の状態を土台にする）。
 */
export async function loadTagMetaResults(): Promise<{
	meta: LoadResult<{ tags: Tag[]; collectionGroups: CollectionGroup[] }>;
	thresholds: LoadResult<TagThresholds[]>;
}> {
	const metaLoad: Promise<LoadResult<{ tags: Tag[]; collectionGroups: CollectionGroup[] }>> =
		Promise.all([listTags(), listCollectionGroups()]).then(
			([tagRows, groupRows]) => ({
				ok: true as const,
				value: { tags: tagRows, collectionGroups: groupRows }
			}),
			(err: unknown) => ({ ok: false as const, error: errorText(err) })
		);
	const thresholdsLoad = listTagThresholds().then(
		(rows) => ({ ok: true as const, value: rows }),
		(err: unknown) => ({ ok: false as const, error: errorText(err) })
	);
	const [meta, thresholds] = await Promise.all([metaLoad, thresholdsLoad]);
	return { meta, thresholds };
}
