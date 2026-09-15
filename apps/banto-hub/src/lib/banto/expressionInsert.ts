/**
 * #342 段階C（docs/tag-server-design.md §4.2「一覧から挿入」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 追補）: 演算タグの式欄の
 * 「一覧から挿入」トグルが ON の間、タグ一覧グリッドの行クリックで
 * 挿入してよいかを判定する純関数群。
 *
 * **なぜ `.svelte.ts` ではなく素の TypeScript か**（`expressionCheck.ts`
 * 冒頭の doc comment と同じ理由）: このリポジトリの vitest は
 * `@sveltejs/vite-plugin-svelte` を導入しない最小構成で、`$state` を含む
 * モジュールを import すると `ReferenceError: $state is not defined` に
 * なる。除外判定は UI と無関係な純粋なグラフ探索なので、ここに素の
 * TypeScript として置き、`$state`/`$derived` は呼び出し元
 * （`(app)/tags/+page.svelte`）に残す。
 *
 * **この判定は UI の補助であり、正は段階 A のサーバチェック**
 * （`POST /api/tags/expression/check` が返す `error.kind` の `cycle` /
 * `string_ref` / `unknown_tag`、および保存時の preflight）**である**。
 * ここでの目的は「挿入した瞬間に必ず落ちる参照を、押す前に止めて理由を
 * 示す」ことであって、検出漏れ・過検出があってもサーバー側が最終的に
 * 受理／拒否を決める（クライアント側でタグ登録自体をハードブロックする
 * ものではない）。
 *
 * **式からのタグ参照抽出はここに複製しない**（issue #342 の方針「パーサを
 * 複製しない」）: 既に `tagDeleteImpact.ts` が同じ用途の近似正規表現
 * （3セグメント・ASCII 識別子・前後の境界チェック）を持っているので、
 * {@link extractTagRefs} はその `extractTagRefTokens` をそのまま使う薄い
 * 別名にとどめ、「その完全名を式に書けるか」の判定も同じ `IDENT_SEGMENT` を
 * 使う `isExpressionRepresentableName` に委ねる（#379 再レビュー対応。
 * 末尾ハイフンだけは近似では拾えない実害のある差なので、そちらの関数で
 * 追加で弾いている - `tagDeleteImpact.ts` の doc comment 参照）。
 */
import { extractTagRefTokens, isExpressionRepresentableName } from './tagDeleteImpact';

/**
 * 挿入候補1件。`tags`（`Tag[]`）から画面側が組み立てる - `externalName` は
 * `externalNameForTag`（`{接続}.{グループ}.{タグ}`）で埋める。
 * `dataType`/`tagKind` を `Tag` の union 型ではなく素の `string` で受けるのは、
 * この判定が「`'string'` かどうか」「`'computed'` かどうか」しか見ないため
 * （型の追加でこのモジュールが壊れない）。
 */
export interface InsertCandidateTag {
	id: number;
	dataType: string;
	tagKind: string;
	expression: string | null;
	/** 完全外部名（`{接続}.{グループ}.{タグ}`）。 */
	externalName: string;
}

/**
 * 式から参照できないデータ型。サーバー側 `computed::resolve_referenced_tag`
 * が `string_ref` エラーで拒否するのと同じ規則（`tagRegistryAdmin.ts` の
 * `TagDataType` に定数が無いためここで名前を付ける）。
 */
export const STRING_DATA_TYPE = 'string';

/** 挿入をブロックする理由の文言（トーストにそのまま出す）。 */
export const SELF_REFERENCE_REASON = '自分自身は式から参照できません';
export const STRING_REFERENCE_REASON = '文字列型のタグは式から参照できません';
export const CYCLE_REFERENCE_REASON =
	'循環参照になるため挿入できません（このタグが編集中のタグを参照しています）';
/**
 * #379 再レビュー対応: レジストリのタグ名・接続名・グループ名の検証は
 * 「空でない・最大長」程度で、banto-expr の識別子文法（ASCII の
 * `[A-Za-z_][A-Za-z0-9_-]*` をドットで3つ）より広い。日本語名・空白入り・
 * 末尾ハイフンなどのタグをそのまま挿入すると、直後の段階Aチェックと保存
 * preflight で必ず落ちるので、押す前に止める。
 */
export const UNREPRESENTABLE_NAME_REASON = '式で表せない名前のタグです（英数字・_・- 以外を含む）';

/**
 * 式中の3セグメントのタグ参照トークンをすべて抽出する。実体は
 * `tagDeleteImpact.ts::extractTagRefTokens`（このファイル冒頭の doc
 * comment「式からのタグ参照抽出はここに複製しない」参照）。
 */
export function extractTagRefs(expression: string | null): string[] {
	if (!expression) return [];
	return extractTagRefTokens(expression);
}

/**
 * `id -> そのタグの式が参照しているタグ id 集合` の依存グラフ。
 * `computed` 以外のタグは式を持たないので辺を持たない。
 */
type RefGraph = Map<number, number[]>;

function buildRefGraph(tags: InsertCandidateTag[]): RefGraph {
	const idByExternalName = new Map<string, number>();
	for (const tag of tags) idByExternalName.set(tag.externalName, tag.id);

	const graph: RefGraph = new Map();
	for (const tag of tags) {
		if (tag.tagKind !== 'computed') continue;
		const refs = extractTagRefs(tag.expression);
		if (refs.length === 0) continue;
		const targets: number[] = [];
		for (const ref of refs) {
			const id = idByExternalName.get(ref);
			// 未知の参照（未登録の外部名・タイプミス）はここでは無視する -
			// サーバー側チェックが `unknown_tag` として拾う。
			if (id !== undefined && !targets.includes(id)) targets.push(id);
		}
		if (targets.length > 0) graph.set(tag.id, targets);
	}
	return graph;
}

/** `candidateId` から辿って `selfId` に到達するか（深さ優先、訪問済み集合で無限ループを防ぐ）。 */
function reaches(graph: RefGraph, fromId: number, targetId: number): boolean {
	const visited = new Set<number>([fromId]);
	const stack = [...(graph.get(fromId) ?? [])];
	while (stack.length > 0) {
		const next = stack.pop() as number;
		if (next === targetId) return true;
		if (visited.has(next)) continue;
		visited.add(next);
		for (const child of graph.get(next) ?? []) stack.push(child);
	}
	return false;
}

/**
 * `candidateId` のタグを、編集中のタグ（`selfId`）の式へ参照として足すと
 * 循環になるか。「候補タグが（推移的に）編集中のタグを参照している」なら
 * 循環。`selfId === null`（新規作成 - まだ自タグが存在しない）のときは
 * 循環しようがないので常に `false`。
 *
 * **正は段階 A のサーバチェック**（`error.kind === 'cycle'`）である
 * （このファイル冒頭の doc comment 参照）。
 */
export function wouldCreateCycle(
	tags: InsertCandidateTag[],
	selfId: number | null,
	candidateId: number
): boolean {
	if (selfId === null) return false;
	if (candidateId === selfId) return true;
	return reaches(buildRefGraph(tags), candidateId, selfId);
}

/**
 * `candidateId` の行をクリックしたときに挿入をブロックすべき理由。
 * ブロックしないなら `null`。判定順は
 * 「自タグ → 式で表せない名前 → 文字列型 → 循環」
 * （名前が式で表せない時点で他の理由を見る意味が無いので先に出す）。
 */
export function insertionBlockReason(
	tags: InsertCandidateTag[],
	selfId: number | null,
	candidateId: number
): string | null {
	const candidate = tags.find((t) => t.id === candidateId);
	if (!candidate) return null;
	return reasonFor(candidate, selfId, () => buildRefGraph(tags));
}

/**
 * 一覧全件について {@link insertionBlockReason} を求めた `id -> 理由` の
 * Map。グリッドの `rowClass`（淡色表示）とクリック時のトーストで同じ判定を
 * 共有するために使う - `insertionBlockReason` を行ごとに呼ぶと依存グラフを
 * 行数ぶん組み立て直すことになるため、グラフを1回だけ作る入口をこちらに
 * 用意している（結果は同じ）。
 */
export function blockedInsertTargets(
	tags: InsertCandidateTag[],
	selfId: number | null
): Map<number, string> {
	let graph: RefGraph | null = null;
	const getGraph = (): RefGraph => (graph ??= buildRefGraph(tags));
	const blocked = new Map<number, string>();
	for (const tag of tags) {
		const reason = reasonFor(tag, selfId, getGraph);
		if (reason !== null) blocked.set(tag.id, reason);
	}
	return blocked;
}

/** 依存グラフの構築を遅延させた共通判定（グラフが要るのは循環チェックだけ）。 */
function reasonFor(
	candidate: InsertCandidateTag,
	selfId: number | null,
	getGraph: () => RefGraph
): string | null {
	if (selfId !== null && candidate.id === selfId) return SELF_REFERENCE_REASON;
	if (!isExpressionRepresentableName(candidate.externalName)) return UNREPRESENTABLE_NAME_REASON;
	if (candidate.dataType === STRING_DATA_TYPE) return STRING_REFERENCE_REASON;
	if (selfId === null) return null;
	if (candidate.tagKind !== 'computed') return null;
	if (reaches(getGraph(), candidate.id, selfId)) return CYCLE_REFERENCE_REASON;
	return null;
}
