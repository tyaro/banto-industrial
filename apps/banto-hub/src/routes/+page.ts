import { redirect } from '@sveltejs/kit';

// ルートパスは分岐のみ: ゲストは /login へ、ログイン済みは /status へ
// （(app) レイアウトガードが認証チェックを担う）。banto-hub の主用途は
// タグサーバーの収集状態を見ることなので、実装指示どおり /status を
// 既定の着地点にする（relay-wright の /settings 相当）。
export function load(): never {
	redirect(307, '/status');
}
