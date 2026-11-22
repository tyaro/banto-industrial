import { redirect } from '@sveltejs/kit';
import { DEMO_NAV_PATHS, IS_DEMO_BUILD, resolveAppPath } from '#lib/navigation.js';

// ルートパスは分岐のみ: ゲストは /login へ、ログイン済みは /status へ
// （(app) レイアウトガードが認証チェックを担う）。banto-hub の主用途は
// タグサーバーの収集状態を見ることなので、実装指示どおり /status を
// 既定の着地点にする（relay-wright の /settings 相当）。
export function load(): never {
	// デモのビルドは対象画面（/tags）へ（docs/demo-pages-design.md §3.4）。
	redirect(307, resolveAppPath(IS_DEMO_BUILD ? DEMO_NAV_PATHS[0] : '/status'));
}
