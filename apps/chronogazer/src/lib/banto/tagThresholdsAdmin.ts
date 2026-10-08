/**
 * 記録計の側のタグごとのしきい値（#532）の API クライアント。
 * `tagRegistryAdmin.ts` と同じ三環境分岐:
 *
 * - Tauri webview -> `invoke()` の `tag_thresholds_*` コマンド
 *   （`apps/chronogazer/src-tauri/src/lib.rs`）。
 * - LAN ブラウザ（組み込みサーバー） -> `/api/tag-thresholds[...]`
 *   （`apps/chronogazer/core/src/rest/tag_thresholds.rs`）。
 * - 単体ブラウザのデモモード -> DB が無いので `DEMO_MODE_MESSAGE` で拒否。
 *
 * 2026-10-08 オーナー決定: しきい値 H / HH / L / LL は**タグ定義の属性ではなく、
 * 使う側（記録計）が持つ設定**。タグ（`Tag`）はもうしきい値を持たない。**既定は
 * 設定なし**（行が無いタグは 4 つとも `null`・版 0）。読み取りは viewer 以上、
 * 保存は editor 以上で、保存は全項目置換・楽観ロック（`expectedRevision`、
 * 食い違いは `409` = `isRevisionConflict`）。収集への反映は「収集を再起動」
 * （タグの保存と同じ）。
 */
import {
	demoModeError,
	httpRequest,
	invokeCommand,
	isTagRegistryAvailable,
	type Tag
} from './tagRegistryAdmin';
import { getBantoMode } from './setup';

/** しきい値の 4 つ（`null` = 設定なし）。監視画面・グループ設定画面の判定の入力。 */
export interface ThresholdFields {
	thresholdLl: number | null;
	thresholdL: number | null;
	thresholdH: number | null;
	thresholdHh: number | null;
}

/** Mirrors `chronogazer_core::tag_thresholds::TagThresholds`. */
export interface TagThresholds extends ThresholdFields {
	tagId: number;
	/** 楽観ロックの版。設定の無い（一度も保存していない）タグは 0。 */
	revision: number;
}

/** Mirrors `chronogazer_core::tag_thresholds::TagThresholdsPayload`（全項目置換）。 */
export interface TagThresholdsInput {
	thresholdLl: number | null;
	thresholdL: number | null;
	thresholdH: number | null;
	thresholdHh: number | null;
	/** 編集を始めたときの `revision`（設定が無ければ 0）。省略すると版を確かめない。 */
	expectedRevision?: number;
}

/** タグにしきい値を添えた形（監視画面・グループ設定画面が判定に使う）。 */
export type TagWithThresholds = Tag & ThresholdFields;

/** 設定なし。 */
export const NO_THRESHOLDS: Readonly<ThresholdFields> = Object.freeze({
	thresholdLl: null,
	thresholdL: null,
	thresholdH: null,
	thresholdHh: null
});

/**
 * タグの一覧に、しきい値の一覧（設定のあるタグだけが来る）を添える（純関数）。
 * 一覧に無いタグは設定なし。元の配列・要素は変えない。
 */
export function withThresholds(
	tags: readonly Tag[],
	thresholds: readonly TagThresholds[]
): TagWithThresholds[] {
	const byTag = new Map(thresholds.map((t) => [t.tagId, t]));
	return tags.map((tag) => {
		const t = byTag.get(tag.id);
		return {
			...tag,
			thresholdLl: t?.thresholdLl ?? null,
			thresholdL: t?.thresholdL ?? null,
			thresholdH: t?.thresholdH ?? null,
			thresholdHh: t?.thresholdHh ?? null
		};
	});
}

export async function listTagThresholds(): Promise<TagThresholds[]> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<TagThresholds[]>('tag_thresholds_list');
	return httpRequest<TagThresholds[]>('/api/tag-thresholds', { method: 'GET' });
}

export async function getTagThresholds(tagId: number): Promise<TagThresholds> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<TagThresholds>('tag_thresholds_get', { tagId });
	}
	return httpRequest<TagThresholds>(`/api/tag-thresholds/${tagId}`, { method: 'GET' });
}

export async function updateTagThresholds(
	tagId: number,
	input: TagThresholdsInput
): Promise<TagThresholds> {
	if (!isTagRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<TagThresholds>('tag_thresholds_update', { tagId, input });
	}
	return httpRequest<TagThresholds>(`/api/tag-thresholds/${tagId}`, {
		method: 'PUT',
		body: input
	});
}
