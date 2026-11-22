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
 * #383 段階2a / R1-B: `/tags`（記録）を追加。位置は
 * recorder-requirements.md §6 の画面順（監視・ヒストリカル・記録・…）に
 * 合わせ、ヒストリカルの直後・イベントの手前に置く。`adminOnly` は付けない -
 * viewer も閲覧できる（R0 §3.6: 読み取りは viewer 以上、書き込みは editor
 * 以上）。
 *
 * #393（#524 の段階 1）: `/groups`（表示グループの定義）を
 * 記録の直後に追加（§6 の画面順: 監視・ヒストリカル・記録・表示グループ・
 * イベント）。記録と同じく viewer も閲覧でき、閲覧公開のセッションには
 * 出さない（設定画面のため）。コマンドパレットにはこの表から自動で載る。
 *
 * I2b（2026-10-04 オーナー決定: ChronoGazer でも閲覧公開を使う）: banto v3.0.0
 * の admin-template `navigation.ts` の `NavItem.publicViewer` と
 * `publicNavItems()` を写した。閲覧公開のセッションに見せるのは、計測値を
 * 見るだけの画面（監視・ヒストリカル・イベント）。記録（PLC の接続先・
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
 *
 * #1159（banto v7.2.0 のメニューバー、nav-menubar-plan.md §4.10・§4.11、Hub の
 * `navigation.ts` と同じ形）: `visibleNavItems()`（RBAC と閲覧公開の許可リストの絞り込みを
 * 純粋関数に切り出した）、`navBarEntries()`（グループを最初のメンバーの位置に集める）、
 * `isNavItemActive()` を足した。メニューバー（`ShellMenubar.svelte`）・ドック
 * （`ShellLauncher.svelte`）・サイドバーが同じ関数で並べる。並びは改善案
 * （docs/chronogazer/chronogazer-ui-improvement-proposal.md）に合わせ「監視 / ヒストリカル /
 * イベント / 記録 / 表示グループ / 管理 ▾（ユーザー管理・監査ログ、admin だけ）/ 設定」。
 * `adminOnly` の項目は 1 つの「管理」グループとして**最初の管理項目の位置**に集める
 * （`group` の指定は要らない）。表の順はサイドバー・コマンドパレットの並びでもある。
 */
import { resolve } from '$app/paths';
import type { Path, ResolvedPathname } from '$app/types';
import { isAdmin, type Role } from '#lib/permissions.js';

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
	/**
	 * この項目が属するグループ（メニューバーでは 1 つのドロップダウンになる、
	 * nav-menubar-plan.md §4.10）。同じキーの項目は最初のメンバーの位置に集まる。
	 * `adminOnly` の項目は指定にかかわらず `ADMIN_GROUP_KEY` に入る。未指定 = 単独の項目。
	 * 今の ChronoGazer に管理以外のグループは無い。
	 */
	group?: NavGroupKey;
}

/** グループのキー。表示名は `NAV_GROUP_LABELS` で引く（ChronoGazer は i18n を使わず日本語を直書き）。 */
export type NavGroupKey = string;

/** `adminOnly` の項目をまとめるグループのキー。 */
export const ADMIN_GROUP_KEY: NavGroupKey = 'admin';

/** グループの表示名（メニューバーのドロップダウンのボタン）。 */
export const NAV_GROUP_LABELS: Record<NavGroupKey, string> = { [ADMIN_GROUP_KEY]: '管理' };

/**
 * デモ（GitHub Pages、docs/demo-pages-design.md §3.4・2026-11-23 決定）の対象画面。
 * デモのビルド（`VITE_BANTO_DEMO=1`）ではメニューに**この画面だけ**を出し、対象外の画面へ
 * アプリの中から着いた場合は `DemoOutOfScope.svelte` の案内を出す（直に開く・読み直すは
 * 静的な配信の 404）。ビルドの時点で定数なので、製品のビルドでは分岐ごと消える。
 */
export const DEMO_NAV_PATHS: readonly AppPath[] = ['/monitor', '/tags'];

/** このビルドがデモか（ビルド時の定数）。純粋関数には引数で渡す（試験で切り替えられる）。 */
export const IS_DEMO_BUILD: boolean = import.meta.env.VITE_BANTO_DEMO === '1';

/** `pathname`（base 付きの URL の path）がデモの対象画面か。末尾の `/` は区別しない。 */
export function isDemoTargetPath(pathname: string): boolean {
	const trimmed = pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname;
	return DEMO_NAV_PATHS.some((path) => {
		const match = resolveAppPath(path);
		return trimmed === match || trimmed.startsWith(match + '/');
	});
}

export const navItems: NavItem[] = [
	{ path: '/monitor', label: '監視', icon: '📈', publicViewer: true },
	{ path: '/free-trends', label: '自由トレンド', icon: '📉', publicViewer: true },
	{ path: '/historical', label: 'ヒストリカル', icon: '🕰️', publicViewer: true },
	{ path: '/daily-report', label: '日報', icon: '🗒️', publicViewer: true },
	{ path: '/events', label: 'イベント', icon: '🔔', publicViewer: true },
	{ path: '/tags', label: '記録', icon: '📝' },
	{ path: '/groups', label: '表示グループ', icon: '🗂️' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true },
	{ path: '/api-keys', label: 'API キー', icon: '🔑', adminOnly: true },
	{ path: '/settings/appearance', label: '設定', icon: '⚙️', activeMatch: '/settings' }
];

/** `navItems` entries visible to a LAN "viewer-public" session (see `NavItem.publicViewer`'s doc comment). Order preserved - the first entry is the guard's redirect target. */
export function publicNavItems(): NavItem[] {
	return navItems.filter((item) => item.publicViewer);
}

/**
 * セッションに見せるナビ項目（navItems の順のまま）。閲覧公開のセッションは
 * `publicViewer` の許可リストだけ（役割によらない、I2b・ADR-0012）。それ以外は管理限定の
 * 項目を無効化せず隠す（RBAC、画面側のガードと同じ）。純粋関数。
 */
export function visibleNavItems(
	session: { role: Role; publicViewer?: boolean },
	demo: boolean = IS_DEMO_BUILD
): NavItem[] {
	const items = session.publicViewer
		? publicNavItems()
		: navItems.filter((item) => !item.adminOnly || isAdmin(session.role));
	// デモのビルドは対象画面だけ（DEMO_NAV_PATHS）。
	return demo ? items.filter((item) => DEMO_NAV_PATHS.includes(item.path)) : items;
}

/** グループ化したバーの 1 スロット: 単独の項目、または最初のメンバーの位置に集めたグループ。 */
export type NavBarEntry =
	{ kind: 'item'; item: NavItem } | { kind: 'group'; key: NavGroupKey; items: NavItem[] };

/**
 * `items` をバーのスロットに並べる（nav-menubar-plan.md §4.10）。グループは最初の
 * メンバーの位置に置き、後のメンバーも元の順で集める。`adminOnly` の項目は
 * `ADMIN_GROUP_KEY` の 1 グループにする。純粋関数 - 絞り込み済みの一覧
 * （`visibleNavItems()`）に適用する。
 */
export function navBarEntries(items: NavItem[]): NavBarEntry[] {
	const entries: NavBarEntry[] = [];
	const groups = new Map<NavGroupKey, NavItem[]>();
	for (const item of items) {
		const key = item.adminOnly ? ADMIN_GROUP_KEY : item.group;
		if (!key) {
			entries.push({ kind: 'item', item });
			continue;
		}
		const members = groups.get(key);
		if (members) {
			members.push(item);
		} else {
			const created = [item];
			groups.set(key, created);
			entries.push({ kind: 'group', key, items: created });
		}
	}
	return entries;
}

/**
 * 「現在地」判定（前方一致）。`activeMatch`（無ければ `path`）を base 付きの URL にして
 * 比べる（`設定` は遷移先が `/settings/appearance` でも `/settings` 配下ならハイライト）。
 */
export function isNavItemActive(item: NavItem, pathname: string): boolean {
	const match = resolveAppPath(item.activeMatch ?? item.path);
	return pathname === match || pathname.startsWith(match + '/');
}

export function pageTitle(pathname: string): string {
	const item = navItems.find((entry) => {
		const match = entry.activeMatch ?? entry.path;
		return pathname === match || pathname.startsWith(match + '/');
	});
	return item?.label ?? 'ChronoGazer';
}

/**
 * 監視画面で表示グループを選んだ URL（R1-D）。選択は `?group=<id>` で URL に
 * 載せる（ブックマーク・共有・戻る操作で同じグループに戻れる）。タブと
 * コマンドパレットの「グループ: ◯◯ を表示」の両方がこれを使う。
 */
export function monitorGroupHref(groupId: number): string {
	return `${resolveAppPath('/monitor')}?group=${groupId}`;
}
