/**
 * T18-2e（docs/banto-hub-t18-design.md「T18-2e T13-3 移管（右クリック作成
 * ＋常時表示作成）」、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「T13-3
 * の右クリック作成に加え、キーボード・タッチでも使える常時表示の作成操作を
 * 必ず用意する」）: タグ登録画面のツリー（`ConnectionTree.svelte`）で右
 * クリック（またはキーボードのコンテキストメニューキー、`TreeView.svelte`
 * 側で `Shift+F10`/メニューキーを同じ `oncontextmenu` コールバックへ変換
 * 済み）されたノードから、コンテキストメニューに出す**唯一の作成アクション**
 * の種別・遷移先/プリセットを決める、依存ゼロの純関数。`tagOnboarding.ts`
 * と同じ方針 - `tags/+page.svelte` 側は DOM 組み立て・実際の遷移
 * （`goto`）/create Drawer 起動に専念させ、判定ロジックはここへ集約して
 * ユニットテストする。あくまで**補助操作**（実装指示 T18-2e 冒頭）であり、
 * 常時表示の「新規登録」等の主操作を置き換えるものではない。
 *
 * 階層ルール（実装指示 T18-2e スコープ1点目）:
 * - ルート（"すべて" ノード = 空ツリーでも常にある `ConnectionTreeNodeData`
 *   の `{ kind: 'all' }`）→ PLC接続の新規作成（`/plc-connections` へ遷移
 *   - このページ自体は接続作成フォームを持たないため、遷移一択）。
 * - 接続ノード → その接続配下に収集グループを新規作成
 *   （`/collection-groups?connectionId=` へ遷移。プリセットの組み立ては
 *   `tagOnboarding.ts::collectionGroupsHref` をそのまま再利用 - T18-2d の
 *   「作成した親 ID が次工程へ自動設定される」導線と同じ仕組み）。
 * - グループノード → そのグループ配下にタグを新規作成（このページ自身が
 *   タグの CRUD を持つため、遷移ではなく `groupId` だけを返す - 呼び出し側
 *   が `treeFilter` をこのグループへ合わせたうえで既存の create Drawer
 *   （`openCreateDrawer`、`resolveGroupIdFromTreeSelection` 経由で選択中
 *   ノードからプリセットする既存 T18-2d ロジック）をそのまま開けばよい）。
 * - `calc`/`mem`（`protocol: 'virtual'`）接続、およびその配下のグループは
 *   対象外（`null` = メニューを出さない）。virtual 接続はサーバー起動時の
 *   自動プロビジョニング専用で、ユーザーが手動で配下にグループ/タグを
 *   新規作成する導線ではない（実装指示「calc/mem（virtual）配下では作成
 *   メニューを出さない」、`tagRegistryAdmin.groupsFor` が同じ接続を
 *   `computed`/`internal` 種別専用として扱っているのと同じ理由）。
 */
import { collectionGroupsHref } from './tagOnboarding';
import type { ConnectionTreeNodeData } from '$lib/components/connectionTreeTypes';

/**
 * `PlcConnection.protocol === 'virtual'` 判定。`tagOnboarding.ts::isVirtual`
 * と同じ理由（本ファイルの「依存ゼロ」方針 - 冒頭コメント参照）で
 * `tagRegistryAdmin.isVirtualConnection` は呼ばず、同じ判定をここに複製
 * する。
 */
function isVirtual(connection: { protocol: string }): boolean {
	return connection.protocol === 'virtual';
}

export type TagTreeContextMenuAction =
	| { kind: 'createConnection'; label: string; href: string }
	| { kind: 'createGroup'; label: string; connectionId: number; href: string }
	| { kind: 'createTag'; label: string; groupId: number };

/**
 * 右クリック（またはキーボードのコンテキストメニューキー）されたノードの
 * データから、出すべきコンテキストメニュー項目を決める。作成操作を提供
 * しないノード（virtual 接続・その配下のグループ）は `null` - 呼び出し側
 * はメニュー自体を表示しない。
 *
 * `label` はメニュー項目の表示文言まで含めてここで固定する - 既存 e2e
 * spec のトースト「作成しました」/「更新しました」/「削除しました」や
 * ボタン名「新規登録」「新規作成」「登録して次へ」「登録して閉じる」等と
 * 部分文字列としても被らない（T18-2d の教訓、PR #135 CI 回帰。メニュー
 * 項目は role="menuitem" でボタンの role="button" とは区別されるため
 * `getByRole` は元々衝突しないが、`getByText` は role を見ないので文言
 * 自体も安全側に倒す）文言にする - 「〜を作成」だけに留め「新規」
 * 「登録」の語は使わない。
 */
export function resolveTagTreeContextMenuAction(
	data: ConnectionTreeNodeData
): TagTreeContextMenuAction | null {
	if (data.kind === 'all') {
		return {
			kind: 'createConnection',
			label: 'PLC接続を作成',
			href: '/plc-connections'
		};
	}
	if (data.kind === 'connection') {
		if (isVirtual(data.connection)) return null;
		return {
			kind: 'createGroup',
			label: `${data.connection.name} 配下に収集グループを作成`,
			connectionId: data.connection.id,
			href: collectionGroupsHref(data.connection.id)
		};
	}
	// data.kind === 'group'
	if (isVirtual(data.connection)) return null;
	return {
		kind: 'createTag',
		label: `${data.group.name} 配下にタグを作成`,
		groupId: data.group.id
	};
}
