/**
 * `categories.ts` の `guardCategory` に対するユニットテスト（#359 chronogazer
 * 分）。純関数なので依存ゼロで直接 import できる - `writableDefault.test.ts`
 * （banto-hub）と同じ describe/it スタイル。
 *
 * chronogazer には banto-hub のようなロックダウン専用 E2E プロジェクトが
 * 無く、「非可視カテゴリへの直接遷移が先頭の可視カテゴリへ redirect される」
 * ケースを実 DOM で固定する手段が乏しい（メインの E2E サーバーでは
 * `security` が `tauri` ゲートで常に非可視になるため、そちらは
 * `z-settings-routes.spec.ts` で拾える - が `guardCategory` 自体の分岐は
 * この単体テストで固定する）。`redirect()`（`@sveltejs/kit`）は例外を投げる
 * ため、`toThrow`/`try-catch` で status/location を検証する。
 */
import { describe, expect, it } from 'vitest';
import { isRedirect } from '@sveltejs/kit';
import { guardCategory, type SettingsCategory } from './categories';

const APPEARANCE: SettingsCategory = {
	id: 'appearance',
	path: '/settings/appearance',
	label: '外観'
};
const DATA: SettingsCategory = { id: 'data', path: '/settings/data', label: 'データ' };
const CATEGORIES: SettingsCategory[] = [APPEARANCE, DATA];

describe('guardCategory', () => {
	it('id が可視カテゴリに含まれる場合は何もしない（redirect しない）', () => {
		expect(() => guardCategory(CATEGORIES, 'data')).not.toThrow();
	});

	it('id が可視カテゴリに含まれない場合は先頭の可視カテゴリへ 307 redirect する', () => {
		try {
			guardCategory(CATEGORIES, 'security');
			expect.unreachable('guardCategory は redirect() を投げるはず');
		} catch (err) {
			if (!isRedirect(err)) throw err;
			expect(err.status).toBe(307);
			expect(err.location).toBe(APPEARANCE.path);
		}
	});

	it('可視カテゴリが空の場合は何もしない（先頭が無いので redirect しない、防御的分岐）', () => {
		expect(() => guardCategory([], 'appearance')).not.toThrow();
	});
});
