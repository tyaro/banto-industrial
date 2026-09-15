/**
 * `categories.ts` の `guardCategory` に対するユニットテスト（#359 relay-wright
 * 分）。純関数なので依存ゼロで直接 import できる - chronogazer
 * （`categories.test.ts`、#372）と同じ describe/it スタイル。relay-wright に
 * これまで vitest 設定が無かったため、chronogazer と同じ方法（`vite.config.ts`
 * をそのまま使い、`package.json` に `"test": "vitest run"` と `vitest`
 * devDependency を追加するだけ）で追加した。
 *
 * relay-wright は chronogazer と違い、`security` カテゴリの可視性が単純な
 * `tauri && canManageAuthMode()` ではなく、タグモニタ手動書き込み・アーム
 * 時限失効（H10、いずれも `isAdmin` のみが条件で `tauri` 不問）との OR
 * になっている（`+layout.ts` の doc comment参照）。そのため admin
 * セッションでは（本番同様の非デモバックエンドである限り）`security` は
 * ほぼ常に可視で、guardCategory の「非可視カテゴリへの直接遷移」分岐は
 * このリポジトリの E2E 環境でも実 DOM 固定できる（`e2e/tests-relay-wright/
 * relay-wright-user-settings-routes.spec.ts` 参照）。それでも、認証モード
 * （ログイン不要モードのエスケープハッチ）変更に伴う「security が可視 →
 * 非可視に変わる」ケース自体は non-admin セッション限定の経路で、
 * このリポジトリの E2E 環境（管理者アカウントのみで検証）では再現できない
 * ため、PR #372 Copilot レビュー指摘の回帰固定はここでも単体テストで代える。
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
const SECURITY: SettingsCategory = {
	id: 'security',
	path: '/settings/security',
	label: 'セキュリティ'
};
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

	it('認証モード OFF 後に invalidate("settings:categories") で再計算された categories（security が抜けた配列）でも先頭の可視カテゴリへ redirect する（chronogazer PR #372 Copilot レビュー指摘と同型の回帰固定）', () => {
		// `SecuritySection.svelte` の「認証」サブセクションが認証モードの
		// 変更成功後に呼ぶ `invalidate('settings:categories')` は
		// `+layout.ts` の `load` を再実行させる - relay-wright では
		// `security` の可視性はタグモニタ/アーム時限失効（isAdmin のみ）
		// との OR なので、この経路で実際に非可視化するのは non-admin
		// セッション（エスケープハッチで認証を管理していた viewer/editor）
		// 限定になる。ここではその「invalidate 後」の状態を直接再現する -
		// 実 DOM 固定は non-admin セッションでの認証モード変更自体が Tauri
		// デスクトップ backend を要求するため、このリポジトリの E2E 環境
		// （`relay-wright-serve`、LAN/REST モード、Tauri backend 無し）では
		// 作れない。
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
