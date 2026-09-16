/**
 * #342 段階B（docs/tag-server-design.md §4.2「セグメント補完」、
 * docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 追補）: 演算タグの式欄で
 * 「いま何を補完すべきか」を決め、候補を並べる**純関数**群。DOM には一切
 * 触らない（ポップアップの描画は `lib/components/CompletionPopup.svelte`、
 * キャレット座標は段階A の `.expr-mirror` を再利用する - 新しい座標計算を
 * 発明しない）。
 *
 * **なぜ `.svelte.ts` ではなく素の TypeScript か**（`expressionCheck.ts` /
 * `expressionInsert.ts` 冒頭の doc comment と同じ理由）: このリポジトリの
 * vitest は `@sveltejs/vite-plugin-svelte` を導入しない最小構成で、`$state`
 * を含むモジュールを import すると `ReferenceError: $state is not defined`
 * になる。補完の判定は UI と無関係な文字列処理なのでここに置き、
 * `$state`/`$derived` は呼び出し元（`(app)/tags/+page.svelte`）に残す。
 *
 * ## 式全体をパースしない（キャレット直前を後ろ向きに読むだけ）
 *
 * issue #342 の方針「パーサを複製しない」に従い、{@link completionContextAt}
 * は**キャレットの直前から後ろ向きに1回走査するだけ**で、式全体の構文木を
 * 作らない。そのぶん割り切りが2つある:
 *
 * - **空白を跨がない**。lexer は空白を捨てるので `conn . group . tag` も
 *   有効な3セグメント参照だが（`tagDeleteImpact.ts::TAG_REF_PATTERN` の
 *   「`.` の周りの空白」参照）、補完は**打鍵中の連続した字面**だけを見る。
 *   `conn .` まで打った状態では補完を出さない。空白入りで書く人は補完を
 *   使わずに書けるし、空白を跨ぐと「どこまでが1つの参照か」を決めるのに
 *   結局パーサが要る。
 * - **キャレットより後ろは見ない**。`line1|abc` のようにカーソルが語の
 *   途中にあるときは、置換範囲はキャレットまで（`abc` は残る）。
 *
 * ## `calc`/`mem` を特別扱いしない
 *
 * `calc`（演算タグ）/`mem`（内部タグ）は予約された仮想接続名だが、実体は
 * ふつうの接続なので索引にも候補にもそのまま入る（`tagRegistryAdmin.ts` の
 * 定数で分岐しない）。式から見ればどれも「第1セグメント＝接続名」でしかない。
 */
import { isExpressionRepresentableSegment } from './tagDeleteImpact';
import type { ExpressionFunction } from './expressionCheck';

// ---------------------------------------------------------------------------
// 1. キャレット位置の解釈（completionContextAt）
// ---------------------------------------------------------------------------

/** 補完する対象のセグメント。第1＝接続名、第2＝収集グループ名、第3＝タグ名。 */
export type CompletionKind = 'segment1' | 'segment2' | 'segment3';

/** {@link completionContextAt} の戻り値。 */
export interface CompletionContext {
	kind: CompletionKind;
	/** キャレット直前まで打たれている、そのセグメントの前方一致文字列（空もあり）。 */
	prefix: string;
	/** 確定時に置き換える範囲（`[replaceFrom, replaceTo)`）= `prefix` が占める範囲。 */
	replaceFrom: number;
	replaceTo: number;
	/** 第1セグメント（`kind` が `segment2`/`segment3` のときだけ）。 */
	seg1?: string;
	/** 第2セグメント（`kind` が `segment3` のときだけ）。 */
	seg2?: string;
}

/** ASCII 識別子の先頭に置ける文字（`crates/banto-expr/src/lexer.rs`）。 */
function isIdentStart(ch: string): boolean {
	return /[A-Za-z_]/.test(ch);
}

/** ASCII 識別子の継続文字（`-` は別扱い - {@link scanBack} 参照）。 */
function isIdentContinue(ch: string): boolean {
	return /[A-Za-z0-9_]/.test(ch);
}

/**
 * 打鍵途中のセグメント: **`tagDeleteImpact.ts::IDENT_SEGMENT` と同じ規則**
 * （`[A-Za-z_][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)*`）に、まだ確定していない
 * 末尾のハイフン1個（`line-` と打って次に `1` を打つところ）だけを足したもの。
 * 確定済みのセグメントは `isExpressionRepresentableSegment` で厳密に見る。
 *
 * #380 レビュー対応2 で `[A-Za-z_][A-Za-z0-9_-]*` から厳密化した - 緩いままだと
 * `a--b` のような lexer では識別子にならない形まで prefix として通ってしまい、
 * {@link scanBack} の判定と食い違う。
 */
const PARTIAL_SEGMENT_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)*-?$/;

/** 1つの参照に許される最大セグメント数（`接続.グループ.タグ`）。 */
const MAX_SEGMENTS = 3;

/**
 * `text[hyphenIndex]`（`-`）が識別子の一部か、それとも減算演算子か。
 * **正は `tagDeleteImpact.ts::IDENT_SEGMENT`**
 * （`[A-Za-z_][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)*`、lexer の「識別子とハイフンの
 * 綱引き」をそのまま写したもの）。条件は2つ:
 *
 * - **右隣が継続文字**であること（`a--b` の1つ目の `-` や `a-` の末尾は
 *   識別子に入らない）。ただし `hyphenIndex + 1 === caret` の場合だけは
 *   **打鍵途中の末尾ハイフン**（`line-` と打って次に `1` を打つところ）と
 *   みなして許す。
 * - **左側が識別子**であること: `-` と継続文字の連なりを左へ辿り、その先頭が
 *   英字か `_` であること（`1-x` の `-` は左が数値なので演算子）。
 *
 * #380 レビュー対応2: 以前は「`-` の直前にある**英数字ラン1つ**の先頭」だけを
 * 見ていたため、`line-1-2` / `line-1-foo` のような**ハイフンを2つ以上含む
 * 有効な識別子**で2つ目の `-` の左ラン（`1`）が英字始まりでないとして走査が
 * 止まり、prefix が `2`/`foo` だけになっていた（候補が消え、確定すると末尾
 * だけを置換して式を壊す）。ハイフンを跨いで左へ辿るように直した。
 */
function hyphenIsPartOfIdentifier(text: string, hyphenIndex: number, caret: number): boolean {
	const next = hyphenIndex + 1;
	if (next !== caret && !isIdentContinue(text[next])) return false;

	// 左へ: 継続文字と「右隣が継続文字である `-`」の連なりを辿る。
	let i = hyphenIndex;
	while (i > 0) {
		const prev = text[i - 1];
		if (isIdentContinue(prev)) {
			i -= 1;
			continue;
		}
		// `text[i]` は今いる位置の文字 = その `-` の右隣。継続文字でなければ
		// （`a--b` のように `-` が続いていれば）そこで識別子は切れている。
		if (prev === '-' && isIdentContinue(text[i])) {
			i -= 1;
			continue;
		}
		break;
	}
	return i < hyphenIndex && isIdentStart(text[i]);
}

/**
 * `caret` の直前から後ろ向きに「識別子文字・`.`・識別子に吸収される `-`」を
 * 拾って、その開始位置を返す。空白・演算子・括弧で止まる。
 *
 * **`-` の扱いは lexer に合わせる**（`crates/banto-expr/src/lexer.rs`
 * 「識別子とハイフンの綱引き」、`tagDeleteImpact.ts::TAG_REF_PATTERN` の
 * 非対称な後読みと同じ規則）- 判定は {@link hyphenIsPartOfIdentifier}。
 */
function scanBack(text: string, caret: number): number {
	let i = caret;
	while (i > 0) {
		const ch = text[i - 1];
		if (isIdentContinue(ch) || ch === '.') {
			i -= 1;
			continue;
		}
		if (ch === '-' && hyphenIsPartOfIdentifier(text, i - 1, caret)) {
			i -= 1;
			continue;
		}
		break;
	}
	return i;
}

/**
 * キャレット位置から「いま何を補完すべきか」を返す。補完を出すべきでない
 * ところ（数値の途中・4セグメント目・不正な先行セグメント）では `null`。
 *
 * 判定はこのファイル冒頭の doc comment のとおり**後ろ向きの1回走査だけ**で、
 * 式全体はパースしない。空文字・行頭・演算子の直後は「第1セグメントを
 * 空の前方一致で補完する」扱いにする（Ctrl+Space / Ctrl+. で任意位置から
 * 一覧を開けるようにするため）。
 */
export function completionContextAt(text: string, caret: number): CompletionContext | null {
	const position = Math.max(0, Math.min(caret, text.length));
	const start = scanBack(text, position);
	const raw = text.slice(start, position);
	const segments = raw.split('.');
	if (segments.length > MAX_SEGMENTS) return null;

	const prefix = segments[segments.length - 1];
	if (prefix !== '' && !PARTIAL_SEGMENT_PATTERN.test(prefix)) return null;

	// 先行セグメント（確定済み）は式に書ける識別子でなければならない -
	// `1.5` の `1` のような数値リテラルの途中はここで落ちる。
	const leading = segments.slice(0, -1);
	for (const [index, segment] of leading.entries()) {
		if (!isExpressionRepresentableSegment(segment, { first: index === 0 })) return null;
	}

	const base = {
		prefix,
		replaceFrom: position - prefix.length,
		replaceTo: position
	};
	if (segments.length === 1) return { kind: 'segment1', ...base };
	if (segments.length === 2) return { kind: 'segment2', ...base, seg1: segments[0] };
	return { kind: 'segment3', ...base, seg1: segments[0], seg2: segments[1] };
}

/**
 * 確定する直前に、ポップアップを開いたときの文脈が**まだキャレットと一致して
 * いるか**を確かめる（#380 レビュー対応5）。
 *
 * ポップアップが開いている間に `input` を伴わずキャレット・選択範囲だけが動く
 * 経路（`PageUp`/`PageDown`、`Ctrl+A`、マウスでのクリック・ドラッグ、外部から
 * の `setSelectionRange` など）はいくらでもある。そのまま Enter/Tab を押すと
 * 古い `replaceFrom`/`replaceTo` の位置へ挿入して**式を壊す**。閉じるキーを
 * 列挙して塞ぐのは漏れるので、**確定の直前にこの1箇所で検証**して、ずれて
 * いたら挿入せずに閉じる。
 *
 * 一致の条件は「キャレットが潰れている（選択が無い）」かつ「その位置が
 * `replaceTo`（＝補完を計算したときのキャレット）と同じ」。
 */
export function completionContextMatchesCaret(
	context: CompletionContext,
	selectionStart: number,
	selectionEnd: number
): boolean {
	return selectionStart === selectionEnd && selectionStart === context.replaceTo;
}

// ---------------------------------------------------------------------------
// 2. 候補の索引（buildCompletionIndex）
// ---------------------------------------------------------------------------

/** 索引の入力（画面が持つフラット3配列のうち、補完に要るフィールドだけ）。 */
export interface CompletionConnectionInput {
	id: number;
	name: string;
}
export interface CompletionGroupInput {
	id: number;
	name: string;
	plcConnectionId: number;
}
export interface CompletionTagInput {
	id: number;
	name: string;
	collectionGroupId: number;
	dataType: string;
	unit: string | null;
}

/** 索引に入るタグ1件（第3セグメントの候補）。 */
export interface CompletionIndexTag {
	id: number;
	name: string;
	dataType: string;
	unit: string | null;
}

/**
 * 接続名 → グループ名 → タグ の索引。**毎キーストロークで組み直さない**
 * 前提（呼び出し元の `$derived` で一覧が変わったときだけ作る）。
 */
export interface CompletionIndex {
	/** 接続名（入力順）。 */
	connections: string[];
	/** 接続名 → その配下のグループ名（入力順）。 */
	groupsByConnection: Map<string, string[]>;
	/** `"接続名.グループ名"` → その配下のタグ（入力順）。 */
	tagsByGroupPath: Map<string, CompletionIndexTag[]>;
}

/** {@link CompletionIndex.tagsByGroupPath} のキー。 */
function groupPath(connectionName: string, groupName: string): string {
	return `${connectionName}.${groupName}`;
}

/**
 * フラットな3配列（`connections`/`groups`/`tags`）から索引を1パスで組む
 * （`connectionTreeBuild.ts` の Map 集計と同じ形 - O(C+G+T)）。
 *
 * 同名の接続・グループは作れない（`name` は UNIQUE、
 * `crates/banto-tags/migrations/0001,0002`）が、**タグ名は
 * グループ内でのみ一意**なので `tagsByGroupPath` のキーを完全パスにしている。
 */
export function buildCompletionIndex(
	connections: CompletionConnectionInput[],
	groups: CompletionGroupInput[],
	tags: CompletionTagInput[]
): CompletionIndex {
	const connectionNameById = new Map<number, string>();
	const connectionNames: string[] = [];
	for (const connection of connections) {
		connectionNameById.set(connection.id, connection.name);
		connectionNames.push(connection.name);
	}

	const groupsByConnection = new Map<string, string[]>();
	const groupPathById = new Map<number, string>();
	for (const group of groups) {
		const connectionName = connectionNameById.get(group.plcConnectionId);
		// 接続が見つからないグループ（一覧の取得タイミングのずれ）は無視する -
		// 完全名を組み立てられない以上、候補にしても挿入できない。
		if (connectionName === undefined) continue;
		const existing = groupsByConnection.get(connectionName);
		if (existing) existing.push(group.name);
		else groupsByConnection.set(connectionName, [group.name]);
		groupPathById.set(group.id, groupPath(connectionName, group.name));
	}

	const tagsByGroupPath = new Map<string, CompletionIndexTag[]>();
	for (const tag of tags) {
		const path = groupPathById.get(tag.collectionGroupId);
		if (path === undefined) continue;
		const entry: CompletionIndexTag = {
			id: tag.id,
			name: tag.name,
			dataType: tag.dataType,
			unit: tag.unit
		};
		const existing = tagsByGroupPath.get(path);
		if (existing) existing.push(entry);
		else tagsByGroupPath.set(path, [entry]);
	}

	return { connections: connectionNames, groupsByConnection, tagsByGroupPath };
}

// ---------------------------------------------------------------------------
// 3. 候補の算出（completionCandidates）
// ---------------------------------------------------------------------------

/** 候補1件。 */
export interface CompletionCandidate {
	/** 挿入する文字列そのもの（登録名 / 関数名）。 */
	label: string;
	kind: 'connection' | 'group' | 'tag' | 'function';
	/**
	 * 1行目の右側に薄く出す**主**の補足（タグなら型・単位、関数なら呼び出し形）。
	 * 無ければ `null`。タグと関数で同じスロットを使うので、行の見た目は揃う。
	 */
	detail: string | null;
	/**
	 * 2行目に薄く出す**副**の補足。現状は組み込み関数の1行説明だけが持つ
	 * （#380 レビュー対応1: `GET /api/tags/expression/functions` が
	 * `description` を返し docs にも「シグネチャと一緒に出す」と書いてあるのに、
	 * 候補へ渡していなかった）。`detail` に連結せず別フィールドにしたのは、
	 * 説明文（日本語の一文）を型・単位と同じスロットへ入れると、タグ候補の
	 * 「`f32・℃`」と並んだときに1行目の見た目が崩れるため。持たない候補は
	 * `null` で、そのときポップアップは2行目自体を描かない。
	 */
	description: string | null;
}

/**
 * 前方一致は**大文字小文字を無視**する（`LINE1` と打っても `line1` が出る）。
 * 挿入されるのは常に登録名そのもの（`label`）なので、式の識別子が
 * 大文字小文字を区別することとは矛盾しない - 探すときだけ緩める。
 */
function matchesPrefix(name: string, prefix: string): boolean {
	if (prefix === '') return true;
	return name.toLowerCase().startsWith(prefix.toLowerCase());
}

/** タグ候補の `detail`（型・単位）。 */
function tagDetail(tag: CompletionIndexTag): string {
	return tag.unit ? `${tag.dataType}・${tag.unit}` : tag.dataType;
}

/**
 * `context` に対する候補一覧。並びは索引の入力順（＝一覧の並び）で、
 * `segment1` のときだけ**接続のあとに関数を並べる**（式の項の先頭は接続名も
 * 関数名も同じ「識別子」なので両方出すが、タグを書きたい人の方が多いので
 * 関数は後ろ）。
 *
 * **除外**:
 * - 式で表せない名前（日本語名・末尾ハイフン等）は
 *   `isExpressionRepresentableSegment` で各セグメント単位に落とす
 *   （第1セグメントだけ `true`/`false` を予約語として追加で落とす）。
 * - 第3セグメント（タグ）では段階C の `blockedInsertTargets` が返す4種
 *   （自タグ・式で表せない名前・文字列型・循環）を**候補に出さない**。
 *   段階C の行クリックは淡色＋クリック時トーストだが、補完は**打鍵の続き**
 *   なので、選べない候補を出す方が邪魔になる（矢印キーで踏む・Enter を
 *   吸われる）。除外理由を知りたい人には一覧側の淡色表示が残っている。
 *
 * @param blockedTagIds 段階C の `blockedInsertTargets(tags, selfId)` の結果
 *   （`id -> 理由`）。補完では理由の文言は使わず、キーの有無だけを見る。
 */
export function completionCandidates(
	context: CompletionContext,
	index: CompletionIndex,
	functions: ExpressionFunction[],
	blockedTagIds: ReadonlyMap<number, string>
): CompletionCandidate[] {
	if (context.kind === 'segment1') {
		const connections: CompletionCandidate[] = index.connections
			.filter(
				(name) =>
					isExpressionRepresentableSegment(name, { first: true }) &&
					matchesPrefix(name, context.prefix)
			)
			.map((name) => ({ label: name, kind: 'connection', detail: null, description: null }));
		const builtins: CompletionCandidate[] = functions
			.filter((fn) => matchesPrefix(fn.name, context.prefix))
			.map((fn) => ({
				label: fn.name,
				kind: 'function',
				detail: fn.signature,
				description: fn.description
			}));
		return [...connections, ...builtins];
	}

	if (context.kind === 'segment2') {
		const groups = index.groupsByConnection.get(context.seg1 ?? '') ?? [];
		return groups
			.filter(
				(name) => isExpressionRepresentableSegment(name) && matchesPrefix(name, context.prefix)
			)
			.map((name) => ({ label: name, kind: 'group', detail: null, description: null }));
	}

	const tags = index.tagsByGroupPath.get(groupPath(context.seg1 ?? '', context.seg2 ?? '')) ?? [];
	return tags
		.filter(
			(tag) =>
				isExpressionRepresentableSegment(tag.name) &&
				!blockedTagIds.has(tag.id) &&
				matchesPrefix(tag.name, context.prefix)
		)
		.map((tag) => ({ label: tag.name, kind: 'tag', detail: tagDetail(tag), description: null }));
}

// ---------------------------------------------------------------------------
// 4. 確定時に挿入する文字列（completionInsertion）
// ---------------------------------------------------------------------------

/** {@link completionInsertion} の戻り値。 */
export interface CompletionInsertion {
	/** `replaceFrom`〜`replaceTo` を置き換える文字列。 */
	text: string;
	/** 挿入直後に次の階層の補完を開き直すか。 */
	reopen: boolean;
}

/**
 * 候補を確定したときに挿入する文字列と、続けて次の階層を開くか。
 *
 * - 接続・グループ → **名前 + `.`** を入れて、そのまま次の階層を開く
 *   （`line1.` → `line1.fast.` → タグ名、と打鍵を止めずに降りられる）。
 * - タグ → 名前だけ入れて閉じる（参照が完成した）。
 * - 関数 → **`name(`** を入れて閉じる（引数はユーザーが打つ。閉じ括弧は
 *   入れない - 既に `)` がある位置での二重入力を避けるため）。
 */
export function completionInsertion(candidate: CompletionCandidate): CompletionInsertion {
	switch (candidate.kind) {
		case 'connection':
		case 'group':
			return { text: `${candidate.label}.`, reopen: true };
		case 'function':
			return { text: `${candidate.label}(`, reopen: false };
		default:
			return { text: candidate.label, reopen: false };
	}
}
