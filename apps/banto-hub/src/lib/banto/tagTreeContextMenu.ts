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
 *
 * T19 S1-a（docs/banto-hub-t19-design.md §7.1、2026-09-02）追記: T18-2e〜
 * T18-6d 時点は `calc`/`mem`（`protocol: 'virtual'`）接続、およびその配下の
 * グループを丸ごと対象外（`null`）にしていたが、これは行き過ぎだった。
 * 旧 `collection-groups` 画面のドロップダウンは virtual 接続を含んでおり、
 * calc/mem 配下の収集グループは作成・再設定・削除できていた（画面を消すと
 * この手段が失われる、設計 §7.1）。**禁止すべきは「virtual 接続そのものの
 * 編集・削除」だけ**（レジストリ側 `plc_connection.rs` が予約接続として
 * 拒否する、T19 の対象外）であり、「virtual 接続配下で何かを作る/配下の
 * グループを操作する」ことまで禁止する理由は無い。したがってこの関数
 * （「作成」1項目版）は接続・グループの `protocol` を一切見ず、**常に**
 * 作成アクションを返すよう改めた（`null` を返すのはノード種別が
 * `ConnectionTreeNodeData` の既知の3種以外に増えた場合に備えた構造のみ
 * 残し、現状は到達しない）。virtual 接続そのものの再設定・削除を禁止する
 * 判定は、この関数の出力を土台にする `resolveTreeContextMenuItems`
 * （本ファイル下部）側で「接続ノードの reconfigure/delete 項目だけ」に
 * 絞って行う - **何に対する操作か（配下を作る／対象そのものを変える）で
 * 分ける**のが筋であり、ノードの virtual 判定の位置をここから動かさない
 * ことは変えていない。
 *
 * T18-6d（TAG-UX-7、2026-08-27 オーナー決定）で導入した「接続/グループの
 * 再設定・削除」項目を追加する本体は、この関数を土台にする
 * `resolveTreeContextMenuItems`（本ファイル下部）に実装している。
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
 * データから、出すべきコンテキストメニュー項目を決める。`null` を返すのは
 * `ConnectionTreeNodeData` が現在の3種（`all`/`connection`/`group`）以外に
 * 増えた場合に備えた構造で、現状はどのノードでも作成アクションを返す
 * （T19 S1-a、上のモジュール doc comment 参照 - virtual 接続配下でも
 * グループ/タグの作成メニューは出す）。
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
		// T19 S1-a: virtual（calc/mem）接続配下でもグループ作成は許可する
		// （上のモジュール doc comment 参照）。virtual 接続そのものの
		// 再設定・削除の禁止は `resolveTreeContextMenuItems` 側で行う。
		return {
			kind: 'createGroup',
			label: `${data.connection.name} 配下に収集グループを作成`,
			connectionId: data.connection.id,
			href: collectionGroupsHref(data.connection.id)
		};
	}
	// data.kind === 'group'（virtual 接続配下でもタグ作成は許可する）
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
 * T18-2e で導入済みの「作成」1項目版）を土台に「接続/グループの再設定・
 * 削除」項目を追加する - 既存のタグ作成メニュー（`createTag`）を壊さず
 * 「同じメニューに項目を足す」（実装指示の制約）ため。`if (!createAction)
 * return []` は `resolveTagTreeContextMenuAction` が `null` を返す構造
 * そのものに乗せてあり（二重実装しない）、T19 S1-a で変更していない。
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
 *
 * T19 S1-a（docs/banto-hub-t19-design.md §7.1、2026-09-02）追記: virtual
 * （calc/mem）接続配下の収集グループは作成・再設定・削除のすべてを許可する
 * よう改めた（旧画面のドロップダウンが virtual 接続を含んでいたのと同じ
 * 権限に揃える）。**禁止のまま維持するのは「接続ノードそのものの再設定・
 * 削除」だけ**（レジストリ側 `plc_connection.rs` が拒否する予約接続。T19 の
 * 対象外）で、これは「接続ノードの `createGroup` 分岐」の中で `data.kind
 * === 'connection'` かつ virtual のときだけ `reconfigureConnection`/
 * `deleteConnection` を足さないという形にした - **isVirtual の判定位置を
 * 動かすのではなく、「配下に作る」操作と「対象そのものを変える」操作を
 * 別々に扱う**（実装指示の要求どおり）。グループノード側（`createTag`
 * 分岐）は接続の virtual 判定を一切見ない - グループ自体は virtual では
 * なく、通常の収集グループと同じ権限（作成・再設定・削除すべて許可）で
 * 扱ってよいため。
 */
export type TreeContextMenuItemAction =
	| { kind: 'createConnection'; label: string }
	| { kind: 'createGroup'; label: string; connectionId: number }
	| { kind: 'reconfigureConnection'; label: string; connectionId: number }
	| { kind: 'deleteConnection'; label: string; connectionId: number }
	| { kind: 'createTag'; label: string; groupId: number }
	| { kind: 'reconfigureGroup'; label: string; groupId: number }
	| { kind: 'deleteGroup'; label: string; groupId: number }
	| { kind: 'viewConnection'; label: string; connectionId: number }
	| { kind: 'viewGroup'; label: string; groupId: number };

export function resolveTreeContextMenuItems(
	data: ConnectionTreeNodeData
): TreeContextMenuItemAction[] {
	const createAction = resolveTagTreeContextMenuAction(data);
	// resolveTagTreeContextMenuAction が null を返す構造そのものに乗せる
	// ガード（現状 all/connection/group のどれでも非 null - 上記 doc comment
	// 参照。将来ノード種別が増えて null になった場合に空配列でメニュー
	// 非表示にする）。
	if (!createAction) return [];

	if (createAction.kind === 'createConnection') {
		return [{ kind: 'createConnection', label: createAction.label }];
	}
	if (createAction.kind === 'createGroup') {
		const items: TreeContextMenuItemAction[] = [
			{ kind: 'createGroup', label: '収集グループを作成', connectionId: createAction.connectionId }
		];
		// T19 S1-a: virtual（calc/mem）接続そのものの再設定・削除は禁止の
		// まま（上記 doc comment 参照）。配下のグループ作成（上の1項目）は
		// virtual でも常に出す。
		if (data.kind === 'connection' && !isVirtual(data.connection)) {
			items.push(
				{
					kind: 'reconfigureConnection',
					label: '接続を再設定',
					connectionId: createAction.connectionId
				},
				{ kind: 'deleteConnection', label: '接続を削除', connectionId: createAction.connectionId }
			);
		}
		return items;
	}
	// createAction.kind === 'createTag'（グループは virtual 接続配下でも
	// 通常のグループと同じ権限 - 作成・再設定・削除すべて許可する）
	return [
		{ kind: 'createTag', label: createAction.label, groupId: createAction.groupId },
		{ kind: 'reconfigureGroup', label: '収集グループを再設定', groupId: createAction.groupId },
		{ kind: 'deleteGroup', label: '収集グループを削除', groupId: createAction.groupId }
	];
}

/**
 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「viewer ロールからの接続・
 * グループ詳細の閲覧」）: 書き込み権限が無い利用者（viewer）向けの右クリック
 * メニュー項目を決める純関数。`resolveTreeContextMenuItems`（上、書き込み
 * 権限がある利用者向け）とは意図的に別関数にする - 「配下に作る／対象を
 * 変える」という書き込み系の判定ロジックに「閲覧」を混ぜると、書き込み系の
 * 既存テストが固定している出力形（`.toEqual`）に予期しない影響が出るため。
 * 呼び出し側（`tags/+page.svelte`）は `canWrite` でどちらの関数を呼ぶかを
 * 切り替えるだけで、判定ロジック自体はここに閉じ込める。
 *
 * 旧 `plc-connections`/`collection-groups` 画面のグリッドは全ロールに表示
 * されていた（クリックによる編集だけが `canWrite` 制限）。ツリー一本化後は
 * `host`/`port`/`unit_id`/`word_order`/`period_ms` を表示する場所が無くなる
 * ため、viewer にも「詳細を表示」の1項目だけを返す（`ConnectionDrawer`/
 * `CollectionGroupDrawer` を読み取り専用モードで開く - 新規に閲覧用の画面は
 * 作らない）。**virtual（calc/mem）接続でも制限しない** - 閲覧は書き込みと
 * 異なり calc/mem を特別扱いする理由が無い（本ファイル冒頭の isVirtual は
 * ここでは参照しない）。ルート（"すべて"）ノードは閲覧対象を持たないので
 * 空配列を返す。
 */
export function resolveReadOnlyTreeContextMenuItems(
	data: ConnectionTreeNodeData
): TreeContextMenuItemAction[] {
	if (data.kind === 'connection') {
		return [{ kind: 'viewConnection', label: '詳細を表示', connectionId: data.connection.id }];
	}
	if (data.kind === 'group') {
		return [{ kind: 'viewGroup', label: '詳細を表示', groupId: data.group.id }];
	}
	return []; // data.kind === 'all' - 閲覧対象が無い。
}

/**
 * T19 S1-a 追記（コードレビュー指摘、2026-09-02）: `resolveTreeContextMenuItems`
 * （書き込み権限あり）と `resolveReadOnlyTreeContextMenuItems`（書き込み
 * 権限なし）のどちらを呼ぶかを `canWrite` で切り替える、依存ゼロの純関数。
 * 元は `tags/+page.svelte::handleTreeContextMenu` に直接書いていた三項演算
 * （`canWrite ? resolveTreeContextMenuItems(node.data) :
 * resolveReadOnlyTreeContextMenuItems(node.data)`）をここへ抽出した -
 * 「viewer には書き込み系メニューが絶対に出ない」という分岐そのものを
 * ページ側の実装から切り離し、依存ゼロの単体テストで固定できるようにする
 * ため（`tags/+page.svelte` は Svelte コンポーネントで DOM 実 E2E でしか
 * 検証できないが、この分岐だけは純関数として抜き出せる）。
 *
 * `canWrite` は呼び出し側（`$lib/permissions.ts::canWriteResources(role)`）
 * が計算済みの真偽値をそのまま渡す - このファイルは「依存ゼロ」方針
 * （冒頭 doc comment 参照）のため `permissions.ts` を import せず、role の
 * 意味（'admin'/'editor'/'viewer'）そのものはここでは扱わない。
 */
export function resolveTreeContextMenuItemsForRole(
	data: ConnectionTreeNodeData,
	canWrite: boolean
): TreeContextMenuItemAction[] {
	return canWrite ? resolveTreeContextMenuItems(data) : resolveReadOnlyTreeContextMenuItems(data);
}
