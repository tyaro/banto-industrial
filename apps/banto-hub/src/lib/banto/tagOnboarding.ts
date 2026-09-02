/**
 * T18-2d（docs/banto-hub-t18-design.md「T18-2d 初回導線チェックリスト」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「初回導線と親設定の引継ぎ」）
 * で導入した、ツリー選択/URLクエリからのフォームプリセット決定を担う、
 * 依存ゼロの純関数群。`tagFormCarry.ts`/`tagDeleteImpact.ts` と同じ方針 -
 * Svelte 側（`tags/+page.svelte`）は `$state`/`$effect`/DOM 組み立てに
 * 専念させ、判定ロジックはここへ集約してユニットテストする。
 * `tagRegistryAdmin` からは型だけを取り込み（`import type`）、実行時の
 * 依存は無い。
 *
 * T19 S1-d（docs/banto-hub-t19-design.md UX-44、2026-09-03）: 初回
 * チェックリスト本体（PLC接続作成→収集グループ作成→タグ登録→収集開始→
 * モニタで値確認の完了判定・次工程算出、`computeOnboardingSteps`/
 * `nextOnboardingStep`/`isOnboardingComplete`/`connectionAwaitingGroup`/
 * `groupAwaitingTag`/`collectionGroupsHref`/`tagsHref`/`OnboardingStep`/
 * `OnboardingSnapshot`/`OnboardingStepId`）は撤去した（2026-09-02 オーナー
 * 決定「起動直後は何も出さない」）。唯一の呼び出し元だった
 * `status/+page.svelte` のチェックリスト UI ごと削除している。管理アカウント
 * 作成の経路は撤去していない（ロックダウンに必須 - ユーザー管理画面
 * `/users` が既に admin ロールで到達可能、設計 §2 UX-44 参照）。
 *
 * 残す {@link monitorHref}・プリセット解決系（{@link resolvePresetConnectionId}
 * 等）・{@link resolveRegistrationTarget} は初回チェックリストとは独立した
 * 機能（登録直後の確認導線 T18-4c、ツリー選択からのフォームプリセット
 * T18-2d/T19 S1-c）なので変更していない。
 */
import type { CollectionGroup, PlcConnection, TagKind } from './tagRegistryAdmin';

/** `PlcConnection.protocol === "virtual"`（`calc`/`mem`）かどうか。型だけの
 * 依存に留めるため、`tagRegistryAdmin.isVirtualConnection` は呼ばず同じ判定
 * をここに複製する（本ファイルの「依存ゼロ」方針 - 冒頭コメント参照）。 */
function isVirtual(connection: Pick<PlcConnection, 'protocol'>): boolean {
	return connection.protocol === 'virtual';
}

/** `tagRegistryAdmin.CALC_CONNECTION_NAME`/`MEM_CONNECTION_NAME` と同じ値を
 * ここに複製する（`isVirtual` と同じ「依存ゼロ」方針 - 冒頭コメント参照）。
 * {@link resolveRegistrationTarget} がグループの `tagKind` を確定するために
 * 使う - `protocol === 'virtual'` だけでは `computed`（calc配下）と
 * `internal`（mem配下）を区別できない。 */
const CALC_CONNECTION_NAME = 'calc';
const MEM_CONNECTION_NAME = 'mem';

/**
 * T18-4c（docs/banto-hub-t18-design.md「T18-4c 確認導線」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-H「新規／変更タグを『確認対象』
 * として値・品質・時刻へ1クリックで移動できるようにする」）: タグ登録
 * ページの各成功ハンドラから `/monitor` へ渡す遷移先を組み立てる。
 *
 * - `groupId` が指定されていれば `?group={id}`。`groupId` が無く
 *   `connectionId` があれば `?connection={id}`（**group と connection は
 *   同時指定しない・group を優先** - モニタのツリー絞り込みは「すべて／
 *   接続／グループ」のいずれか1つの粒度しか選べないため）。
 * - `focus`（対象タグの external_name 配列）が空でなければ `focus=` を
 *   付ける。複数要素はカンマ区切りで連結し、各要素は `encodeURIComponent`
 *   する（`URLSearchParams` は使わない - 呼び出し側で組み立てた「区切りの
 *   カンマは生のまま、要素の中身だけ encode 済み」の文字列をそのまま
 *   `URLSearchParams` へ渡すと、`,` まで含めて二重に percent-encode
 *   されてしまう）。group/connection パラメータがあれば `&` で連結する。
 * - `focus` が空配列・未指定なら `focus` パラメータ自体を付けない。
 * - 何も指定が無ければ `/monitor` をそのまま返す。
 */
export function monitorHref(opts: {
	groupId?: number | null;
	connectionId?: number | null;
	focus?: string[];
}): string {
	const params: string[] = [];
	if (opts.groupId !== null && opts.groupId !== undefined) {
		params.push(`group=${opts.groupId}`);
	} else if (opts.connectionId !== null && opts.connectionId !== undefined) {
		params.push(`connection=${opts.connectionId}`);
	}
	if (opts.focus && opts.focus.length > 0) {
		params.push(`focus=${opts.focus.map((name) => encodeURIComponent(name)).join(',')}`);
	}
	return params.length === 0 ? '/monitor' : `/monitor?${params.join('&')}`;
}

// --- 親設定プリセット（ツリー選択・URLクエリ → フォームへの引継ぎ） --------

/**
 * `?connectionId=` クエリの値を検証し、有効な実接続（virtual を除く）の ID
 * のみ返す。未指定・非数値・存在しない ID・virtual 接続はすべて `null`
 * （呼び出し側はプリセットせず既定のまま = 「選択してください」）。
 */
export function resolvePresetConnectionId(
	rawValue: string | null,
	connections: Pick<PlcConnection, 'id' | 'protocol'>[]
): number | null {
	if (rawValue === null) return null;
	const id = Number(rawValue);
	if (!Number.isInteger(id)) return null;
	const match = connections.find((c) => c.id === id);
	if (!match || isVirtual(match)) return null;
	return id;
}

/**
 * `?groupId=` クエリ（またはツリー選択のグループ ID）を検証し、有効な実
 * 収集グループ（接続が virtual でない）の ID のみ返す。`calc`/`mem` 配下の
 * グループは「選ばせない」（実装指示）ため、ここで弾く。
 */
export function resolvePresetGroupId(
	rawValue: string | null,
	groups: Pick<CollectionGroup, 'id' | 'plcConnectionId'>[],
	connections: Pick<PlcConnection, 'id' | 'protocol'>[]
): number | null {
	if (rawValue === null) return null;
	const id = Number(rawValue);
	if (!Number.isInteger(id)) return null;
	const group = groups.find((g) => g.id === id);
	if (!group) return null;
	const conn = connections.find((c) => c.id === group.plcConnectionId);
	if (!conn || isVirtual(conn)) return null;
	return id;
}

/** `tags/+page.svelte` の `TreeFilter` と同じ形（ここでは import せず構造的に受ける）。 */
export type TreeSelectionForPreset =
	{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };

/**
 * ツリーで選択中のノードから、単票/連続登録フォームへプリセットすべき
 * 収集グループ ID を決める。「すべて」ノードや接続ノード（グループが一意に
 * 決まらない）の選択時は `null`（プリセットしない）- 実装指示「Tree
 * 選択の接続／グループも登録フォームへプリセットする」のうち、確実に単一
 * 値へ決まるのはグループ選択時のみのため。virtual 接続配下のグループも
 * {@link resolvePresetGroupId} と同じ理由で除外する。
 */
export function resolveGroupIdFromTreeSelection(
	selection: TreeSelectionForPreset,
	groups: Pick<CollectionGroup, 'id' | 'plcConnectionId'>[],
	connections: Pick<PlcConnection, 'id' | 'protocol'>[]
): number | null {
	if (selection.type !== 'group') return null;
	return resolvePresetGroupId(String(selection.id), groups, connections);
}

/**
 * T19 S1-c（UX-33、docs/banto-hub-t19-design.md「タグ登録の起点」、2026-09-02
 * オーナー決定「グループ選択時に右画面を出し、そのグループに対して登録する」）:
 * 現在のツリー選択から、タグ登録操作（新規登録・連続登録）を提示してよいか、
 * 提示するなら対象グループは何かを1つの値にまとめる。`null` は「提示しない」
 * （呼び出し側はツールバーの登録ボタンを出さず、代わりに案内を出す） -
 * 「すべて」ノード・接続ノードはグループが一意に決まらないためいずれも
 * `null`（実装指示「接続ノードや『すべて』を選んでいる場合…グループが
 * 特定できない状態」）。
 *
 * {@link resolveGroupIdFromTreeSelection}（T18-2d、単票フォームへの
 * 「プリセット」用）とは違い、**virtual（calc/mem）配下のグループを除外
 * しない**。右クリックメニューの「グループ配下にタグを作成」
 * （`tagTreeContextMenu.ts::resolveTagTreeContextMenuAction`、T19 S1-a で
 * virtual 配下も常に許可するよう改めた）と同じ権限に揃えるべきだからである -
 * calc/mem 配下のグループもタグは必ずグループに属する通常の収集グループで
 * あり、選択すれば登録先は一意に決まる（`computed`/`internal` タグを
 * 作れないと、旧 UI が持っていた機能が失われる）。
 */
export interface RegistrationTarget {
	/** 登録先の収集グループ ID。 */
	groupId: number;
	/** 画面表示用のグループ名（「どのグループに登録されるか」を明示する）。 */
	groupName: string;
	/**
	 * このグループへ新規作成するタグの種別。グループが属する接続の名前で
	 * 一意に決まる（`calc` 配下は `computed`、`mem` 配下は `internal`、それ
	 * 以外は `plc` - `banto_tags::tag::validate_tag_kind_placement` と同じ
	 * 配置規則）。単票の新規登録フォームはこの値へ固定する。
	 */
	tagKind: TagKind;
	/**
	 * 連続登録フォームを提示してよいか。連続登録は PLC アドレスの算術
	 * （増分・桁上がり）を前提にした機能で `tagKind` は常に `'plc'`
	 * （`ContinuousFormState` 冒頭コメント参照）- `computed`/`internal` タグは
	 * アドレスを持てないため、virtual 配下のグループでは常に `false`。
	 */
	supportsContinuous: boolean;
}

export function resolveRegistrationTarget(
	selection: TreeSelectionForPreset,
	groups: Pick<CollectionGroup, 'id' | 'name' | 'plcConnectionId'>[],
	connections: Pick<PlcConnection, 'id' | 'name' | 'protocol'>[]
): RegistrationTarget | null {
	if (selection.type !== 'group') return null;
	const group = groups.find((g) => g.id === selection.id);
	if (!group) return null; // 通常起きない（選択できるのは実在するグループのみ）。
	const conn = connections.find((c) => c.id === group.plcConnectionId);
	if (!conn) return null; // 同上。
	const tagKind: TagKind =
		conn.name === CALC_CONNECTION_NAME
			? 'computed'
			: conn.name === MEM_CONNECTION_NAME
				? 'internal'
				: 'plc';
	return {
		groupId: group.id,
		groupName: group.name,
		tagKind,
		supportsContinuous: tagKind === 'plc'
	};
}
