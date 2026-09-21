/**
 * `categories.ts` の `guardCategory` に対するユニットテスト（#359 chronogazer
 * 分）。純関数なので依存ゼロで直接 import できる - `writableDefault.test.ts`
 * （banto-hub）と同じ describe/it スタイル。
 *
 * chronogazer には banto-hub のようなロックダウン専用 E2E プロジェクトが
 * 無く、「非可視カテゴリへの直接遷移が先頭の可視カテゴリへ redirect される」
 * ケースを実 DOM で固定する手段が乏しい（メインの E2E サーバーでは
 * `security` が `tauri` ゲートで常に非可視になるため、そちらは
 * `e2e/tests/user-settings-routes.spec.ts` で拾える - が `guardCategory` 自体の分岐は
 * この単体テストで固定する）。`redirect()`（`@sveltejs/kit`）は例外を投げる
 * ため、`toThrow`/`try-catch` で status/location を検証する。
 *
 * PR #372 Copilot レビュー指摘（認証モード変更後に `invalidate
 * ('settings:categories')` で categories を再計算する修正）の回帰固定も
 * ここに含める: `security` を含む可視カテゴリから `security` が抜けた
 * 状態で `guardCategory` を呼ぶケース。実 DOM での固定は `security`
 * セクションの表示・トグル自体が Tauri デスクトップ backend を要求し、
 * このリポジトリの E2E 環境（`banto-serve`、LAN/REST モード）では作れない
 * ため、この単体テストで代える（該当 `it` の doc comment参照）。
 */
import { describe, expect, it } from 'vitest';
import { isRedirect } from '@sveltejs/kit';
import { guardCategory, SETTINGS_CATEGORIES, type SettingsCategory } from './categories';

const APPEARANCE: SettingsCategory = {
	id: 'appearance',
	path: '/settings/appearance',
	label: '外観'
};
const DATA: SettingsCategory = { id: 'data', path: '/settings/data', label: 'データ' };
const SECURITY: SettingsCategory = {
	id: 'security',
	path: '/settings/security',
	label: 'セキュリティ'
};
const CATEGORIES: SettingsCategory[] = [APPEARANCE, DATA];

describe('SETTINGS_CATEGORIES', () => {
	it('id・path・ラベルが重複しない（ナビと route が1対1で対応する）', () => {
		const ids = SETTINGS_CATEGORIES.map((category) => category.id);
		const paths = SETTINGS_CATEGORIES.map((category) => category.path);
		const labels = SETTINGS_CATEGORIES.map((category) => category.label);
		expect(new Set(ids).size).toBe(SETTINGS_CATEGORIES.length);
		expect(new Set(paths).size).toBe(SETTINGS_CATEGORIES.length);
		expect(new Set(labels).size).toBe(SETTINGS_CATEGORIES.length);
	});

	it('#383 段階2b（C-3b）で足した「収集」カテゴリが末尾にあり、既存の並びを崩していない', () => {
		expect(SETTINGS_CATEGORIES.map((category) => category.id)).toEqual([
			'appearance',
			'account',
			'connectivity',
			'data',
			'security',
			'hub',
			'collect'
		]);
		expect(SETTINGS_CATEGORIES.at(-1)).toEqual({
			id: 'collect',
			path: '/settings/collect',
			label: '収集'
		});
	});
});

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

	it('認証モード OFF 後に invalidate("settings:categories") で再計算された categories（security が抜けた配列）でも先頭の可視カテゴリへ redirect する（PR #372 Copilot レビュー指摘の回帰固定）', () => {
		// `SecuritySection.svelte` が認証モードの変更成功後に呼ぶ
		// `invalidate('settings:categories')` は `+layout.ts` の `load` を
		// 再実行させ、`security/+page.ts` はその再実行後の新しい `categories`
		// （security を含まない）で `guardCategory` を呼ぶ - ここではその
		// 「invalidate 後」の状態を直接再現する。実 DOM 固定は `security`
		// セクションの表示・トグル自体が Tauri デスクトップ backend
		// （src-tauri の `auth_config_apply` コマンド）を要求するため、この
		// リポジトリの E2E 環境（`banto-serve`、LAN/REST モード、Tauri
		// backend 無し）では作れない（`e2e/playwright.config.ts` に
		// banto-hub の `chromium-locked-down` に相当する Tauri 相当の
		// project も無い）。
		const beforeInvalidate: SettingsCategory[] = [APPEARANCE, SECURITY];
		const afterInvalidate = beforeInvalidate.filter((category) => category.id !== 'security');

		try {
			guardCategory(afterInvalidate, 'security');
			expect.unreachable('guardCategory は redirect() を投げるはず');
		} catch (err) {
			if (!isRedirect(err)) throw err;
			expect(err.status).toBe(307);
			expect(err.location).toBe(APPEARANCE.path);
		}
	});
});
