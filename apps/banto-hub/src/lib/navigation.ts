/**
 * サイドバーナビゲーション定義。relay-wright/chronogazer の navigation.ts
 * と同型だが、項目は banto-hub 独自（状態(/status)・タグ登録・タグモニタ・
 * APIキー(admin限定)・ユーザー管理(admin限定)・監査ログ(admin限定)・
 * 書き込み監査(admin限定、T2-4)・設定）を書き下ろし。
 *
 * 状態モニタ(/status)を先頭に置くのは、このアプリの主用途が「タグサーバー
 * が正しく収集できているかを見る」ことだからで、ルート `/` からの
 * redirect 先にもなっている（routes/+page.ts）。
 *
 * T19 S1-d（UX-30、docs/banto-hub-t19-design.md、2026-09-03）: 旧
 * `PLC接続`（`/plc-connections`）・`収集グループ`（`/collection-groups`）の
 * 2エントリを削除した。設定の入口はタグ画面（`/tags`）へ一本化済み（S1-a〜
 * S1-c）で、両画面の固有機能（接続テスト・シミュレーション切替・
 * `word_order`・calc/mem 配下のグループ操作・viewer の閲覧手段）は既に
 * タグ画面のツリー右クリックメニュー・Drawer へ移設されている（設計 §7.1）。
 * `commands.ts` はこの配列（`navItems`）から自動生成するため、削除した
 * 2エントリはコマンドパレットからも同時に消える。
 *
 * #359 段階2（設定画面のカテゴリ別ルート化）: `設定` の遷移先を先頭カテゴリ
 * `/settings/appearance` に変えた（`/settings` 自体は redirect 専用になる
 * ため、ナビからは直接その先を指す）。ただし「現在地の判定」
 * （`pageTitle()`・`Sidebar.svelte` の `isActive`）は `/settings` 配下の
 * どのカテゴリを開いていても『設定』のまま出したい - `path` を素直に
 * prefix 判定に使うと `/settings/data` などで前方一致しなくなる
 * （`path='/settings/appearance'` は `/settings/data` の prefix ではない）
 * ため、判定専用の `activeMatch`（既定は `path` 自身、他の項目は今までと
 * 同じ挙動）を導入し、`設定` だけ `/settings` を明示している。
 *
 * banto v4.0.0（SvelteKit 3、#325）: admin-template v4.0.0 の `navigation.ts` の
 * `AppPath` と `resolveAppPath()` を写した（ChronoGazer の同名ファイルと同じ）。
 * 表の `path`・`activeMatch` は SvelteKit 3 が生成する `Path` から作る
 * `AppPath` で型付けし（存在しないルートは型エラー）、URL にするとき
 * （`href`・`goto()`・`redirect()`）は `resolveAppPath()` を通す。banto-hub は
 * `base` を使わない（axum がルート直下に配信する）ので今は `path` そのままと
 * 同じ URL になるが、文字列の連結で base を足し引きしない（上流の移行で、
 * 自動移行が `resolve('')` で「base を外す」処理に書き換えてガードが壊れた、
 * #325。SvelteKit 3 の `resolve('')` は `base` ではなく `base + '/'`）。
 */
import { resolve } from '$app/paths';
import type { Path, ResolvedPathname } from '$app/types';
import { APP_NAME } from '#lib/appName.js';

/**
 * An app route's pathname as written in the nav/category tables: a leading
 * `/` and no base path (e.g. `/status`, `/settings/appearance`). Typed from
 * SvelteKit 3's generated `Path` union, so a table entry pointing at a route
 * that does not exist is a type error.
 */
export type AppPath = `/${Path}`;

/**
 * `AppPath` -> the href / `goto()` / `redirect()` target with the base path
 * prefixed (SvelteKit 3 `resolve()`, which takes the pathname without its
 * leading `/`). Use this, not string concatenation, wherever an `AppPath`
 * leaves the app as a URL.
 */
export function resolveAppPath(path: AppPath): ResolvedPathname {
	return resolve(path.slice(1) as Path);
}

export interface NavItem {
	path: AppPath;
	label: string;
	/** Placeholder icon (emoji) until an icon set is decided. */
	icon: string;
	/** RBAC: only shown to the `admin` role. Undefined/false = visible to every role. */
	adminOnly?: boolean;
	/**
	 * 「現在地」判定（前方一致）に使う基準パス。省略時は `path` を使う。
	 * `path` がナビの遷移先（クリック時の実際の URL）で、`activeMatch` は
	 * それとは独立に「この項目が現在地としてハイライトされるべき URL
	 * prefix」を表す - 通常は同じだが、`設定` のようにクリック時の遷移先が
	 * サブパスでも、判定は親パス配下すべてを対象にしたい場合に分ける。
	 */
	activeMatch?: AppPath;
}

export const navItems: NavItem[] = [
	{ path: '/status', label: '状態', icon: '📡' },
	{ path: '/tags', label: 'タグ登録', icon: '🏷️' },
	{ path: '/monitor', label: 'タグモニタ', icon: '📈' },
	// S6（docs/banto-hub-external-db-design.md §5.2・§7 row S6）: DB Sink の
	// sink group 管理画面。`GET /api/sink/groups`はロール不問（閲覧は
	// viewer も可）・作成/更新/削除は editor 以上（`require_editor`）と
	// サーバー側の権限に合わせ、ここでは admin 限定にしない - ページ内の
	// 「サイドカー用 API キー」セクションだけを admin 限定で出す
	// （`/api-keys`自体が admin 限定なのと同じ理由、`sink/+page.svelte`参照）。
	{ path: '/sink', label: 'DB Sink', icon: '🗄️' },
	{ path: '/api-keys', label: 'APIキー', icon: '🔑', adminOnly: true },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true },
	{ path: '/write-audit', label: '書き込み監査', icon: '✍️', adminOnly: true },
	{ path: '/settings/appearance', label: '設定', icon: '⚙️', activeMatch: '/settings' }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find((entry) => {
		const match = entry.activeMatch ?? entry.path;
		return pathname === match || pathname.startsWith(match + '/');
	});
	return item?.label ?? APP_NAME;
}
