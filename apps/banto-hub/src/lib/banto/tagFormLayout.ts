/**
 * T18-2a（docs/banto-hub-t18-design.md「T18-2a 単票フォーム刷新」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-B）: `tags/+page.svelte` の
 * 単票 create/edit フォーム（`tagFields` スニペット）刷新に必要な、
 * 依存ゼロの純関数・定数だけを切り出す。`tagFormNumeric.ts`/
 * `tagDeleteImpact.ts`/`formDirty.ts` と同じ方針 — Svelte 側は `$state`/
 * `$effect`/DOM 組み立てに専念させ、ここに置く関数はスナップショット値
 * だけを引数に取り、テストしやすく保つ。
 *
 * 本モジュールが担う2つの役割:
 *
 * 1. **詳細セクションの自動展開判定**（TAG-UX-B「詳細側にエラー時は自動
 *    展開する」）: `<details class="detail-group">` 3セクション
 *    （表示・スケーリング／しきい値／書き込み安全設定）のどれにエラーが
 *    出ているかを判定する {@link hasFieldError} と、そのためのフィールド
 *    集合定数（{@link DISPLAY_SCALING_FIELDS} など）。
 * 2. **保存前の固定確認領域**（TAG-UX-B「保存前に最終外部名
 *    `{connection}.{group}.{tag}`、実機／SIM、書き込み許可を固定領域で
 *    確認できるようにする」）: {@link buildConfirmExternalName}・
 *    {@link environmentLabel}・{@link writePermissionLabel}。
 *    `+page.svelte` 側は `groups`/`connections` から接続・グループ名を
 *    引いてこれらへ渡すだけの薄いラッパーを持つ（ページ側の `FormState`/
 *    `groupsFor` 等に依存させないため、ここでは素の文字列・真偽値だけを
 *    受け取る）。
 *
 * 上記いずれも `FormState`（ページ側の型）を直接知らなくても成立するため
 * ここへ切り出すことでユニットテスト対象にできる（`+page.svelte` 自体には
 * 自動テストが無い）。
 */
import type { TagKind } from './tagRegistryAdmin';

/** 「表示・スケーリング」詳細セクションに属するフィールド名。 */
export const DISPLAY_SCALING_FIELDS = ['decimals', 'rawLo', 'rawHi', 'engLo', 'engHi'] as const;

/** 「しきい値」詳細セクションに属するフィールド名。 */
export const THRESHOLD_FIELDS = ['thresholdH', 'thresholdHh', 'thresholdL', 'thresholdLl'] as const;

/** 「書き込み安全設定」詳細セクションに属するフィールド名。 */
export const WRITE_SAFETY_FIELDS = ['writable'] as const;

/**
 * `errors`（フィールド名 → エラーメッセージのマップ）のうち、`fields` に
 * 含まれるいずれかのキーが真値を持つか。`<details bind:open>` を強制的に
 * 開く条件の判定に使う（TAG-UX-B「詳細側にエラー時は自動展開する」）。
 */
export function hasFieldError(errors: Record<string, string>, fields: readonly string[]): boolean {
	return fields.some((field) => Boolean(errors[field]));
}

/**
 * 保存前確認領域の「外部名」行 `{connection}.{group}.{tag}` を組み立てる。
 * `tagDeleteImpact.ts::buildExternalName` と異なり、こちらは**保存前・
 * 入力途中**のフォーム値が対象なので、接続/グループ未選択やタグ名未入力を
 * エラーにせず、それぞれ分かりやすいプレースホルダで埋める。
 */
export function buildConfirmExternalName(input: {
	connectionName?: string;
	groupName?: string;
	tagName: string;
}): string {
	const conn =
		input.connectionName && input.connectionName.trim() !== '' ? input.connectionName : '(未選択)';
	const group = input.groupName && input.groupName.trim() !== '' ? input.groupName : '(未選択)';
	const trimmedName = input.tagName.trim();
	const name = trimmedName !== '' ? trimmedName : '(未入力)';
	return `${conn}.${group}.${name}`;
}

/**
 * 保存前確認領域の「実機 / SIM」行。`simulation` はフォームが参照する
 * 収集グループの接続（`PlcConnection.simulation`）— グループ未選択の間は
 * `undefined` になる。
 */
export function environmentLabel(simulation: boolean | undefined): string {
	if (simulation === undefined) return '-';
	return simulation ? 'シミュレーション（SIM）' : '実機';
}

/**
 * 保存前確認領域の「書き込み許可」行。`computed` タグは式が値を決めるため
 * 常に書き込み不可（`toInput` が送信直前にも強制する規則と同じ、
 * `+page.svelte` の `toInput` コメント参照）— フォームのチェックボックス値
 * に関わらずここでも「不許可」で表示を揃える。
 */
export function writePermissionLabel(tagKind: TagKind, writable: boolean): string {
	if (tagKind === 'computed') return '不許可（演算タグは書き込み不可）';
	return writable ? '許可' : '不許可';
}
