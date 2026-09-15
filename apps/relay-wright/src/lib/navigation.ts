/**
 * Sidebar navigation definition.
 *
 * W1 (plan `luminous-discovering-goblet.md`): only banto's standard
 * settings/users/audit-log screens existed. ChronoGazer's domain screens
 * (監視/ヒストリカル/イベント) are NOT carried over - this app's own
 * monitoring/rule/write-target screens are new entries here, not a revival
 * of ChronoGazer's.
 *
 * W2 adds this app's own registry CRUD screens: write-targets (書き込み先)
 * and write-rules (書き込みルール). Both are editor-write/viewer-read (spec
 * M10), same as the rest of this list, so they are NOT `adminOnly` - the
 * page itself hides create/edit/delete controls for a viewer
 * (`canWriteResources`, see the two routes' `+page.svelte`).
 *
 * W4 adds the monitoring/operation screens: engine (エンジン制御・監視) near
 * the top since it is the operator's primary control surface, and
 * write-audit-log (書き込み監査ログ) alongside the other log/監査 screens.
 * Both are viewer+ reachable (status/log are viewer-readable) - the engine
 * page role-gates its arm/disarm/dry-run/reload controls internally
 * (`isAdmin`/`canWriteResources`, backend also enforces), so neither is
 * `adminOnly` here.
 *
 * R1-B adds the tag-registry CRUD screens: plc-connections (PLC接続) and
 * tags (タグ登録; 収集グループ management lives INSIDE that screen). They
 * sit ABOVE 書き込み先/書き込みルール so the registry reads top-down in
 * setup order: 接続 → タグ → 書き込み先 → ルール. Same viewer-read/
 * editor-write split as the W2 screens, so not `adminOnly`.
 *
 * feature/tag-monitor adds monitor (モニタ) directly below エンジン制御:
 * 接続→収集グループのツリー + 選択グループのタグのリアルタイム値（約1秒
 * 更新）+ 値セルクリックの即時手動書き込み（デバッグ用途・確認ダイアログ
 * なし。バックエンドで editor+ ゲート & manual_write 監査）。viewer は
 * 閲覧のみ（画面内で編集を出し分け）なので not `adminOnly`.
 *
 * qr-codes (QRコード) is a debug utility: 画面に表示したQRコードをタッチ
 * パネル（HMI）のQRリーダーでスキャンするための文字列リスト+表示画面。
 * PLC/レジストリ系とは独立なので、ログ系の下（管理者専用画面の手前）に
 * 置く。viewer は表示（スキャン）可能なので not `adminOnly`.
 *
 * project (プロジェクト, feature/project-file) は設定レジストリ全体を1つの
 * JSONプロジェクトファイルとして保存・読み込みする画面。エクスポートは
 * editor+、インポートは admin（画面側で権限に応じて操作を出し分け、backend も
 * 二経路対称に強制）なので、editor もエクスポート導線を見られるよう
 * `adminOnly` にはせず、管理者系の手前に置く。
 *
 * #359 relay-wright 分（設定画面のカテゴリ別ルート化）: `設定` の遷移先を
 * 先頭カテゴリ `/settings/appearance` に変えた（`/settings` 自体は redirect
 * 専用になるため、ナビからは直接その先を指す）。ただし「現在地の判定」
 * （`pageTitle()`・`Sidebar.svelte` の `isActive`）は `/settings` 配下の
 * どのカテゴリを開いていても『設定』のまま出したい - `path` を素直に
 * prefix 判定に使うと `/settings/data` などで前方一致しなくなる
 * （`path='/settings/appearance'` は `/settings/data` の prefix ではない）
 * ため、判定専用の `activeMatch`（既定は `path` 自身、他の項目は今までと
 * 同じ挙動）を導入し、`設定` だけ `/settings` を明示している（banto-hub・
 * chronogazer の navigation.ts と同じパターン）。
 */
import { APP_NAME } from '$lib/appName';

export interface NavItem {
	path: string;
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
	activeMatch?: string;
}

export const navItems: NavItem[] = [
	{ path: '/engine', label: 'エンジン制御・監視', icon: '🕹️' },
	{ path: '/monitor', label: 'モニタ', icon: '📟' },
	{ path: '/settings/appearance', label: '設定', icon: '⚙️', activeMatch: '/settings' },
	{ path: '/plc-connections', label: 'PLC接続', icon: '🔌' },
	{ path: '/tags', label: 'タグ登録', icon: '🏷️' },
	{ path: '/write-targets', label: '書き込み先', icon: '🎯' },
	{ path: '/write-rules', label: '書き込みルール', icon: '🧮' },
	{ path: '/write-audit-log', label: '書き込み監査ログ', icon: '📝' },
	{ path: '/qr-codes', label: 'QRコード', icon: '🔳' },
	{ path: '/project', label: 'プロジェクト', icon: '💾' },
	{ path: '/users', label: 'ユーザー管理', icon: '👤', adminOnly: true },
	{ path: '/audit-log', label: '監査ログ', icon: '🧾', adminOnly: true }
];

export function pageTitle(pathname: string): string {
	const item = navItems.find((entry) => {
		const match = entry.activeMatch ?? entry.path;
		return pathname === match || pathname.startsWith(match + '/');
	});
	return item?.label ?? APP_NAME;
}
