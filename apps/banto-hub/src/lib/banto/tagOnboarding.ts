/**
 * T18-2d（docs/banto-hub-t18-design.md「T18-2d 初回導線チェックリスト」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「初回導線と親設定の引継ぎ」）:
 * 初回チェックリスト（PLC接続作成→収集グループ作成→タグ登録→収集開始→
 * モニタで値確認）の完了判定・次工程算出と、ツリー選択/URLクエリからの
 * フォームプリセット決定を担う、依存ゼロの純関数群。`tagFormCarry.ts`/
 * `tagDeleteImpact.ts` と同じ方針 - Svelte 側（`status`/`plc-connections`/
 * `collection-groups`/`tags` の各 `+page.svelte`）は `$state`/`$effect`/
 * DOM 組み立てに専念させ、判定ロジックはここへ集約してユニットテストする。
 * `tagRegistryAdmin`/`hubStatus` からは型だけを取り込み（`import type`）、
 * 実行時の依存は無い。
 *
 * **2026-08-31 オーナー指摘による工程の見直し（実機での使用感から判明）**:
 * 実際の試運転の流れは「PLC接続の設定→収集グループ作成→タグ登録→収集を
 * 開始してPLCにアクセスできているか確認」であり、以前の5工程
 * （connection→connectionTest→group→tag→simValue）には2つの問題があった。
 *
 * 1. 「接続テストの成功」が `connections` の `status: "connected"` で
 *    判定されていたが、これは収集エンジンが実際にセッションを張っている
 *    ライブ状態であり、対象接続に収集グループ・タグがあり、かつ収集が
 *    `RunMode::Configured` で稼働していない限り絶対に `"connected"` に
 *    ならない。つまりこの工程は「収集グループ作成」「タグ登録」より前の
 *    2番目に置かれていたにもかかわらず、実際にはそれらの**後**、かつ
 *    「収集を開始した後」でないと達成不可能だった - 手順として矛盾して
 *    いた。さらに当時は収集を開始する UI 導線自体が無かった
 *    （`POST /api/collection/start` を叩けるのは API のみ）ため、この
 *    工程は事実上どの画面からも完了させられなかった。
 * 2. 最終工程「SIM値の確認」という名前が「全PLCシミュレーションを挟む
 *    ことが必須」であるかのような誤解を招いていた。実際の判定条件
 *    （`values` に `q === "good"` が1件でもあるか）はシミュレーションかどうか
 *    を問わないが、収集を開始する手段が無かった当時は事実上シミュレーション
 *    経由でしか満たせなかった。実機が目の前にある試運転では SIM を挟む
 *    必然性が無い（オーナー指摘）ため、工程名を「モニタで値確認」へ改め、
 *    実機由来の値でも達成できることを明確にした。
 *
 * これを受け、新しい5工程は connection→group→tag→collectionStart→
 * monitorValue。「接続テストの成功」は独立した工程として廃止し、その意図
 * （実際に接続できているかの確認）は新設した「収集の開始」
 * （`RunMode::Configured` で稼働中/稼働試行中かどうか）と「モニタで値確認」
 * の2工程に引き継いだ - 収集を実際に開始して値が読めることを確認する、と
 * いう一連の流れそのものが「接続テスト」の実質だからである。
 *
 * **区別注意（触っていないもの）**: 接続単位のシミュレーション
 * （`PlcConnection.simulation`、T9-2、接続の Drawer のチェックボックス）は
 * このオーナー指摘とは無関係で一切変更していない。「全PLCシミュレーション」
 * （`RunMode::AllSimulation`、`POST /api/collection/start-all-simulation`）
 * だけを必須動線から外した - 前者は接続ごとに実機/SIMを選ぶ機能、後者は
 * 運転モード全体を切り替える機能で、混同しないこと
 * （`collectionControlAdmin.ts` 冒頭のdoc comment も参照）。
 *
 * 判定元（実データ判定。画面訪問や操作ログでは判定しない）:
 * - **PLC接続の作成**: `calc`/`mem`（`protocol: "virtual"`、自動プロビジョニ
 *   ング）を除く `PlcConnection` が1件以上存在するか。
 * - **収集グループの作成**: virtual接続配下を除く `CollectionGroup` が1件
 *   以上存在するか。
 * - **タグの登録**: `tagKind === "plc"` の `Tag` が1件以上存在するか
 *   （`computed`/`internal` はこのチェックリストが案内する「PLCタグ収集」
 *   の導線とは無関係なので数えない）。
 * - **収集の開始**: `GET /api/status`（`hubStatus.ts`参照）の
 *   `collection_mode === "configured"` かつ `collection_state !== "stopped"`
 *   か - 「全PLCシミュレーション」（`collection_mode === "all_simulation"`）
 *   だけでは達成できない（実機に接続する操作を経たことを要求するため、
 *   上記のオーナー指摘に忠実）。
 * - **モニタで値確認**: `GET /api/values`（同上）の `values` に
 *   `q === "good"` が1件でもあるか（シミュレーション/実機を問わない）。
 *
 * 各工程の CTA リンク先（`href`）は「まだ埋まっていない親」を優先して選ぶ
 * （{@link connectionAwaitingGroup}/{@link groupAwaitingTag}）- 単に
 * 先頭要素を指すと、複数接続/グループがある環境で既に子を持つ親へ誘導して
 * しまい、「グループが無い接続」「タグが無いグループ」に辿り着けない。
 */
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import type { ValueEntry } from './hubStatus';

/** `PlcConnection.protocol === "virtual"`（`calc`/`mem`）かどうか。型だけの
 * 依存に留めるため、`tagRegistryAdmin.isVirtualConnection` は呼ばず同じ判定
 * をここに複製する（本ファイルの「依存ゼロ」方針 - 冒頭コメント参照）。 */
function isVirtual(connection: Pick<PlcConnection, 'protocol'>): boolean {
	return connection.protocol === 'virtual';
}

/** virtual（`calc`/`mem`）を除いた実接続。 */
function realConnections(connections: PlcConnection[]): PlcConnection[] {
	return connections.filter((c) => !isVirtual(c));
}

/** virtual接続配下を除いた実収集グループ（`plc` タグ用の導線が対象のグループ）。 */
function realGroups(
	groups: CollectionGroup[],
	connections: Pick<PlcConnection, 'id' | 'protocol'>[]
): CollectionGroup[] {
	const virtualConnectionIds = new Set(connections.filter((c) => isVirtual(c)).map((c) => c.id));
	return groups.filter((g) => !virtualConnectionIds.has(g.plcConnectionId));
}

/**
 * まだ収集グループを持たない実接続を優先して返す（無ければ先頭の実接続）。
 * チェックリストの「収集グループの作成」CTA が「次に埋めるべき接続」へ
 * 誘導するために使う。実接続が1件も無ければ `null`。
 */
export function connectionAwaitingGroup(
	connections: PlcConnection[],
	groups: Pick<CollectionGroup, 'plcConnectionId'>[]
): PlcConnection | null {
	const real = realConnections(connections);
	if (real.length === 0) return null;
	return real.find((c) => !groups.some((g) => g.plcConnectionId === c.id)) ?? real[0];
}

/**
 * まだ `plc` タグを持たない実収集グループを優先して返す（無ければ先頭の
 * 実収集グループ）。チェックリストの「タグの登録」CTA 用。実収集グループが
 * 1件も無ければ `null`。
 */
export function groupAwaitingTag(
	groups: CollectionGroup[],
	connections: Pick<PlcConnection, 'id' | 'protocol'>[],
	tags: Pick<Tag, 'collectionGroupId' | 'tagKind'>[]
): CollectionGroup | null {
	const real = realGroups(groups, connections);
	if (real.length === 0) return null;
	return (
		real.find((g) => !tags.some((t) => t.collectionGroupId === g.id && t.tagKind === 'plc')) ??
		real[0]
	);
}

/** `/collection-groups` への遷移先。`connectionId` があればプリセット用クエリを付ける。 */
export function collectionGroupsHref(connectionId: number | null): string {
	return connectionId === null
		? '/collection-groups'
		: `/collection-groups?connectionId=${connectionId}`;
}

/** `/tags` への遷移先。`groupId` があればプリセット用クエリを付ける。 */
export function tagsHref(groupId: number | null): string {
	return groupId === null ? '/tags' : `/tags?groupId=${groupId}`;
}

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

export type OnboardingStepId = 'connection' | 'group' | 'tag' | 'collectionStart' | 'monitorValue';

export interface OnboardingStep {
	id: OnboardingStepId;
	/** チェックリスト表示用の日本語ラベル（工程名）。 */
	label: string;
	/** 実データから判定した完了フラグ（冒頭コメントの判定元を参照）。 */
	done: boolean;
	/** この工程を進めるための遷移先（プリセットクエリ込み）。 */
	href: string;
	/** CTA ボタン/リンクの文言。 */
	ctaLabel: string;
}

/** {@link computeOnboardingSteps} の入力 - 各画面が個別に取得済みの一覧/状態を束ねるだけ。 */
export interface OnboardingSnapshot {
	connections: PlcConnection[];
	groups: CollectionGroup[];
	tags: Tag[];
	/** `GET /api/status` の `collection_state`（`hubStatus.ts`参照）。 */
	collectionState: string;
	/** `GET /api/status` の `collection_mode`（同上、2026-08-31 新設）。 */
	collectionMode: string;
	values: ValueEntry[];
}

/**
 * 5工程チェックリストの完了状態と次工程 CTA を組み立てる。工程の順序は
 * 常に `connection → group → tag → collectionStart → monitorValue` で固定
 * （2026-08-31 オーナー指摘 - 冒頭 doc comment 参照。旧 `connectionTest`/
 * `simValue` からの変更理由もそこに記載）。
 */
export function computeOnboardingSteps(snapshot: OnboardingSnapshot): OnboardingStep[] {
	const real = realConnections(snapshot.connections);
	const hasConnection = real.length > 0;

	const groups = realGroups(snapshot.groups, snapshot.connections);
	const hasGroup = groups.length > 0;

	const hasTag = snapshot.tags.some((t) => t.tagKind === 'plc');

	// 「全PLCシミュレーション」（collectionMode === "all_simulation"）だけでは
	// 完了させない - 実PLCへの接続を試みたことを要求する（冒頭 doc comment
	// 「収集の開始」参照）。starting/running/stopping/faulted はいずれも
	// 「開始操作を経た」ことの証跡として扱う。
	const hasCollectionStart =
		snapshot.collectionMode === 'configured' && snapshot.collectionState !== 'stopped';

	const hasGoodValue = snapshot.values.some((v) => v.q === 'good');

	const targetConnection = connectionAwaitingGroup(snapshot.connections, snapshot.groups);
	const targetGroup = groupAwaitingTag(snapshot.groups, snapshot.connections, snapshot.tags);

	return [
		{
			id: 'connection',
			label: 'PLC接続の作成',
			done: hasConnection,
			href: '/plc-connections',
			ctaLabel: 'PLC接続を作成'
		},
		{
			id: 'group',
			label: '収集グループの作成',
			done: hasGroup,
			href: collectionGroupsHref(targetConnection?.id ?? null),
			ctaLabel: '収集グループを作成'
		},
		{
			id: 'tag',
			label: 'タグの登録',
			done: hasTag,
			href: tagsHref(targetGroup?.id ?? null),
			ctaLabel: 'タグを登録'
		},
		{
			id: 'collectionStart',
			label: '収集の開始',
			done: hasCollectionStart,
			href: '/status#collection-control',
			ctaLabel: '収集を開始'
		},
		{
			id: 'monitorValue',
			label: 'モニタで値確認',
			done: hasGoodValue,
			href: '/monitor',
			ctaLabel: 'モニタで確認'
		}
	];
}

/** 未完了の最初の工程（先頭から順に判定）。全完了なら `null`。 */
export function nextOnboardingStep(steps: OnboardingStep[]): OnboardingStep | null {
	return steps.find((s) => !s.done) ?? null;
}

/** 全工程が完了しているか（空配列は「未完了」扱い - データ未取得と区別するため）。 */
export function isOnboardingComplete(steps: OnboardingStep[]): boolean {
	return steps.length > 0 && steps.every((s) => s.done);
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
