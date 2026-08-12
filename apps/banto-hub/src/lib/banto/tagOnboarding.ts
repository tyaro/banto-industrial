/**
 * T18-2d（docs/banto-hub-t18-design.md「T18-2d 初回導線チェックリスト」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-A「初回導線と親設定の引継ぎ」）:
 * 初回チェックリスト（PLC接続作成→接続テスト→収集グループ作成→タグ登録→
 * SIM値確認）の完了判定・次工程算出と、ツリー選択/URLクエリからのフォーム
 * プリセット決定を担う、依存ゼロの純関数群。`tagFormCarry.ts`/
 * `tagDeleteImpact.ts` と同じ方針 - Svelte 側（`status`/`plc-connections`/
 * `collection-groups`/`tags` の各 `+page.svelte`）は `$state`/`$effect`/
 * DOM 組み立てに専念させ、判定ロジックはここへ集約してユニットテストする。
 * `tagRegistryAdmin`/`hubStatus` からは型だけを取り込み（`import type`）、
 * 実行時の依存は無い。
 *
 * 設計判断（TAG-UX-A「完了判定は画面訪問でなく実データで判定」に対応する
 * 具体的な判定元がスコープ指示にあるため、それに従う）:
 * - **PLC接続の作成**: `calc`/`mem`（`protocol: "virtual"`、自動プロビジョニ
 *   ング）を除く `PlcConnection` が1件以上存在するか。
 * - **接続テストの成功**: 単発のテストボタン結果（クリック操作のログ）では
 *   なく、`GET /api/v1/status` の `connections`（`ConnectionStatusEntry`、
 *   実際に収集エンジンが張っているライブ接続状態）に `status: "connected"`
 *   の非virtual接続が1件でもあるか - 「画面訪問でなく実データ」により忠実
 *   （テストボタンを押した記憶ではなく、実際に繋がっているという事実）。
 * - **収集グループの作成**: virtual接続配下を除く `CollectionGroup` が1件
 *   以上存在するか。
 * - **タグの登録**: `tagKind === "plc"` の `Tag` が1件以上存在するか
 *   （`computed`/`internal` はこのチェックリストが案内する「PLCタグ収集」
 *   の導線とは無関係なので数えない）。
 * - **SIM値の確認**: `GET /api/v1/values` の `values` に `q === "good"` が
 *   1件でもあるか。
 *
 * 各工程の CTA リンク先（`href`）は「まだ埋まっていない親」を優先して選ぶ
 * （{@link connectionAwaitingGroup}/{@link groupAwaitingTag}）- 単に
 * 先頭要素を指すと、複数接続/グループがある環境で既に子を持つ親へ誘導して
 * しまい、「グループが無い接続」「タグが無いグループ」に辿り着けない。
 */
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';
import type { ConnectionStatusEntry, ValueEntry } from './hubStatus';

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

export type OnboardingStepId = 'connection' | 'connectionTest' | 'group' | 'tag' | 'simValue';

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
	connectionStatuses: ConnectionStatusEntry[];
	values: ValueEntry[];
}

/**
 * 5工程チェックリストの完了状態と次工程 CTA を組み立てる。工程の順序は
 * 常に `connection → connectionTest → group → tag → simValue` で固定
 * （TAG-UX-A の連続導線と一致）。
 */
export function computeOnboardingSteps(snapshot: OnboardingSnapshot): OnboardingStep[] {
	const real = realConnections(snapshot.connections);
	const hasConnection = real.length > 0;

	const connectedIds = new Set(
		snapshot.connectionStatuses.filter((s) => s.status === 'connected').map((s) => s.id)
	);
	const hasConnectionTest = real.some((c) => connectedIds.has(c.id));

	const groups = realGroups(snapshot.groups, snapshot.connections);
	const hasGroup = groups.length > 0;

	const hasTag = snapshot.tags.some((t) => t.tagKind === 'plc');

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
			id: 'connectionTest',
			label: '接続テストの成功',
			done: hasConnectionTest,
			href: '/plc-connections',
			ctaLabel: '接続テストを行う'
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
			id: 'simValue',
			label: 'SIM値の確認',
			done: hasGoodValue,
			href: '/monitor',
			ctaLabel: 'SIM値を確認'
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
