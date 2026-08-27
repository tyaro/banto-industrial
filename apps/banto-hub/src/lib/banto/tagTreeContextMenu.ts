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
 *
 * T18-6d（TAG-UX-7、2026-08-27 オーナー決定）追記: 上記は T18-2e 時点の
 * 「作成のみ・1項目」の挙動を固定する既存関数として変更せず残す（下の
 * `resolveTagTreeContextMenuAction` 単体テストが `.toEqual` でこの形を
 * 固定しているため）。ツリー右クリックから接続/グループの再設定・削除も
 * 行えるようにする本体は、この関数を土台にする `resolveTreeContextMenuItems`
 * （本ファイル下部）に別途実装する。
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

/**
 * T18-6d（TAG-UX-7、実装指示「タグ登録ページのツリー右クリックから PLC接続・
 * 収集グループを管理できるようにする」）: タグツリーの右クリックメニューに
 * 出す**全項目**を決める純関数。`resolveTagTreeContextMenuAction`（上記、
 * T18-2e で導入済み・既存ユニットテストが固定している「作成」1項目版）は
 * そのまま残し、ここではそれを土台に「接続/グループの再設定・削除」項目を
 * 追加する - 既存のタグ作成メニュー（`createTag`）を壊さず「同じメニューに
 * 項目を足す」（実装指示の制約）ため。virtual（`calc`/`mem`）配下でメニュー
 * 自体を出さない判定も `resolveTagTreeContextMenuAction` が `null` を返す
 * ことにそのまま乗せる（二重実装しない）。
 *
 * `createConnection`/`createGroup` は T18-2e 時点では他画面への `goto`
 * （`href`）だったが、T18-6a/6b で `ConnectionDrawer`/`CollectionGroupDrawer`
 * という自己完結部品がこのページからも開けるようになったため、T18-6d では
 * 「その場で Drawer を開く」に置き換える - よって `href` は使わず、ラベルも
 * 実装指示が指定する文言（接続名を冠さない「収集グループを作成」等）に
 * 差し替える。`createTag`（グループ配下にタグを作成）だけは T18-2e の挙動
 * （このページ自身の create Drawer をプリセット付きで開く）を変えないので
 * label も含めそのまま流用する。
 *
 * 呼び出し側（`tags/+page.svelte`）は、返ってきた各項目の `kind` に応じて
 * `ConnectionDrawer`/`CollectionGroupDrawer`/既存 create Drawer のどれを
 * 開くかだけを振り分ける - 判定ロジック自体はここに閉じ込め、ページ側は
 * DOM 組み立てと実行に専念させる（`tagOnboarding.ts`/上記関数と同じ方針）。
 */
export type TreeContextMenuItemAction =
	| { kind: 'createConnection'; label: string }
	| { kind: 'createGroup'; label: string; connectionId: number }
	| { kind: 'reconfigureConnection'; label: string; connectionId: number }
	| { kind: 'deleteConnection'; label: string; connectionId: number }
	| { kind: 'createTag'; label: string; groupId: number }
	| { kind: 'reconfigureGroup'; label: string; groupId: number }
	| { kind: 'deleteGroup'; label: string; groupId: number };

export function resolveTreeContextMenuItems(
	data: ConnectionTreeNodeData
): TreeContextMenuItemAction[] {
	const createAction = resolveTagTreeContextMenuAction(data);
	if (!createAction) return []; // virtual 配下 - メニュー自体を出さない。

	if (createAction.kind === 'createConnection') {
		return [{ kind: 'createConnection', label: createAction.label }];
	}
	if (createAction.kind === 'createGroup') {
		return [
			{ kind: 'createGroup', label: '収集グループを作成', connectionId: createAction.connectionId },
			{
				kind: 'reconfigureConnection',
				label: '接続を再設定',
				connectionId: createAction.connectionId
			},
			{ kind: 'deleteConnection', label: '接続を削除', connectionId: createAction.connectionId }
		];
	}
	// createAction.kind === 'createTag'
	return [
		{ kind: 'createTag', label: createAction.label, groupId: createAction.groupId },
		{ kind: 'reconfigureGroup', label: '収集グループを再設定', groupId: createAction.groupId },
		{ kind: 'deleteGroup', label: '収集グループを削除', groupId: createAction.groupId }
	];
}
