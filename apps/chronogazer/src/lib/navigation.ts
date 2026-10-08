/**
 * Sidebar navigation definition.
 *
 * From M2, entries for CRUD pages are derived from resource definitions
 * (spec §3.1); manual entries like the ones below remain possible.
 *
 * #359 chronogazer 分（設定画面のカテゴリ別ルート化）: `設定` の遷移先を
 * 先頭カテゴリ `/settings/appearance` に変えた（`/settings` 自体は redirect
 * 専用になるため、ナビからは直接その先を指す）。ただし「現在地の判定」
 * （`pageTitle()`・`Sidebar.svelte` の `isActive`）は `/settings` 配下の
 * どのカテゴリを開いていても『設定』のまま出したい - `path` を素直に
 * prefix 判定に使うと `/settings/data` などで前方一致しなくなる
 * （`path='/settings/appearance'` は `/settings/data` の prefix ではない）
 * ため、判定専用の `activeMatch`（既定は `path` 自身、他の項目は今までと
 * 同じ挙動）を導入し、`設定` だけ `/settings` を明示している
 * （banto-hub の navigation.ts と同じパターン）。
 *
 * #383 段階2a / R1-B: `/tags`（タグ設定）を追加。位置は
 * recorder-requirements.md §6 の画面順（監視・ヒストリカル・タグ設定・…）に
 * 合わせ、ヒストリカルの直後・イベントの手前に置く。`adminOnly` は付けない -
 * viewer も閲覧できる（R0 §3.6: 読み取りは viewer 以上、書き込みは editor
 * 以上）。
 *
 * #393（#524 の段階 1）: `/groups`（グループ設定 = 表示グループの定義）を
 * タグ設定の直後に追加（§6 の画面順: 監視・ヒストリカル・タグ設定・グループ
 * 設定・イベント）。タグ設定と同じく viewer も閲覧でき、閲覧公開のセッションには
 * 出さない（設定画面のため）。コマンドパレットにはこの表から自動で載る。
 *
 * I2b（2026-10-04 オーナー決定: ChronoGazer でも閲覧公開を使う）: banto v3.0.0
 * の admin-template `navigation.ts` の `NavItem.publicViewer` と
 * `publicNavItems()` を写した。閲覧公開のセッションに見せるのは、計測値を
 * 見るだけの画面（監視・ヒストリカル・イベント）。タグ設定（PLC の接続先・
 * アドレスを含む設定画面）・ユーザー管理・監査ログ・設定は出さない
 * （admin-template が「ツリー」と設定を出さないのと同じ考え方）。
 *
 * banto v4.0.0（SvelteKit 3、#325）: admin-template v4.0.0 の `navigation.ts` の
 * `AppPath` と `resolveAppPath()` を写した。表の `path`・`activeMatch` は
 * SvelteKit 3 が生成する `Path` から作る `AppPath` で型付けし（存在しない
 * ルートは型エラー）、URL にするとき（`href`・`goto()`・`redirect()`）は
 * `resolveAppPath()` を通す。ChronoGazer は `base` を使わない（ルート直下に
 * 配信する）ので今は `path` そのままと同じ URL になるが、比べる側
 * （`(app)/+layout.ts` の閲覧公開のガード）も `resolveAppPath()` を通した
 * 値と `url.pathname` を比べる形にしておく（上流の移行で、自動移行が
 * `resolve('')` で「base を外す」処理に書き換えてガードが壊れた、#325）。
 */
import { resolve } from '$app/paths';
import type { Path, ResolvedPathname } from '$app/types';

/**
 * An app route's pathname as written in the nav/category tables: a leading
 * `/` and no base path (e.g. `/monitor`, `/settings/appearance`). Typed from
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
	/** Spec M10 RBAC: only shown to the `admin` role. Undefined/false = visible to every role. */
	adminOnly?: boolean;
	/**
	 * 「現在地」判定（前方一致）に使う基準パス。省略時は `path` を使う。
	 * `path` がナビの遷移先（クリック時の実際の URL）で、`activeMatch` は
	 * それとは独立に「この項目が現在地としてハイライトされるべき URL
	 * prefix」を表す - 通常は同じだが、`設定` のようにクリック時の遷移先が
	 * サブパスでも、判定は親パス配下すべてを対象にしたい場合に分ける。
	 */
	activeMatch?: AppPath;
	/**
	 * Opt-in allowlist for the LAN "viewer-public" session
	 * (viewer-public-plan §3.1-6, ADR-0012): `sessionStore.publicViewer`
	 * sessions see ONLY entries with `publicViewer: true` in the sidebar, and
	 * `(app)/+layout.ts`'s guard redirects any other path to the first such
	 * entry. This narrows the SCREEN surface only - the actual data boundary
	 * is RBAC's `viewer` role (ADR-0012 §帰結), so this flag must never be
	 * treated as an authorization check. Undefined/false = hidden from a
	 * public-viewer session.
	 */
	publicViewer?: boolean;
}

export const navItems: NavItem[] = [
	{ path: '/monitor', label: '監視', icon: '📈', publicViewer: true },
	{ path: '/historical', label: 'ヒストリカル', icon: '🕰️', publicViewer: true },
	{ path: '/tags', label: 'タグ設定', icon: '🏷️' },
	{ path: '/groups', label: 'グループ設定', icon: '🗂️' },
	{ path: '/events', label: 'イベント', icon: '🔔', publicViewer: true },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true },
	{ path: '/settings/appearance', label: '設定', icon: '⚙️', activeMatch: '/settings' }
];

/** `navItems` entries visible to a LAN "viewer-public" session (see `NavItem.publicViewer`'s doc comment). Order preserved - the first entry is the guard's redirect target. */
export function publicNavItems(): NavItem[] {
	return navItems.filter((item) => item.publicViewer);
}

export function pageTitle(pathname: string): string {
	const item = navItems.find((entry) => {
		const match = entry.activeMatch ?? entry.path;
		return pathname === match || pathname.startsWith(match + '/');
	});
	return item?.label ?? 'ChronoGazer';
}
