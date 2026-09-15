/**
 * 設定カテゴリの単一情報源（#359 段階2）。route の `path` とナビ用ラベルを
 * ここに1本化し、次から参照する:
 * - `+layout.ts`（現在のセッションで見えるカテゴリだけに絞り込み、
 *   `categories` として返す）。
 * - `+layout.svelte`（絞り込み済みのカテゴリからナビを描画する）。
 * - `+page.ts`（`/settings` を `categories[0].path` へ redirect する）。
 * - 各カテゴリ自身の `+page.ts`（`guardCategory` 参照）。
 *
 * 上流テンプレート v1.6.0 の settings/categories.ts と同じ役割・ファイル名。
 * banto-hub は paraglide を使っていないため `labelKey` ではなく日本語の
 * `label` を直接持たせている。
 */
import { redirect } from '@sveltejs/kit';

export type SettingsCategoryId = 'appearance' | 'account' | 'connectivity' | 'data' | 'security';

export interface SettingsCategory {
	id: SettingsCategoryId;
	path: string;
	label: string;
}

/** 表示され得る全カテゴリ（ナビ順）。`+layout.ts` がこれを現在のセッションで見える subset に絞り込む。 */
export const SETTINGS_CATEGORIES: SettingsCategory[] = [
	{ id: 'appearance', path: '/settings/appearance', label: '外観' },
	{ id: 'account', path: '/settings/account', label: 'アカウント' },
	{ id: 'connectivity', path: '/settings/connectivity', label: '接続' },
	{ id: 'data', path: '/settings/data', label: 'データ' },
	{ id: 'security', path: '/settings/security', label: 'セキュリティ' }
];

/**
 * 各カテゴリ自身の `+page.ts` から呼ぶガード: `+layout.ts` が計算した
 * 可視カテゴリに `id` が含まれていなければ、先頭の可視カテゴリへ
 * redirect する（権限の無いカテゴリへの直接 URL アクセス対策 -
 * `users/+page.ts` と同じ「ナビから隠す = 直接アクセスも弾く」方針）。
 */
export function guardCategory(categories: SettingsCategory[], id: SettingsCategoryId): void {
	if (categories.some((category) => category.id === id)) return;
	const first = categories[0];
	if (first) redirect(307, first.path);
}
