/**
 * T18-1（TAG-UX-C 5点目、docs/banto-hub-desktop-plan.md §9.4「削除前に演算
 * タグ等の参照影響と完全な外部名を表示する」）: タグ削除前の確認ダイアログ
 * 用ヘルパー。フロントのみの実装で新規 API/DB変更はない — サーバー側の
 * 削除 preflight（`apps/banto-hub/core/src/rest.rs::tags_delete` →
 * `preflight_transaction` → `banto_expr` の式コンパイルが参照切れで失敗する）
 * が正しさの最終バックストップであり続ける。ここでの目的は「ユーザーが
 * 削除前に影響を目視できる」という UX の改善であって、検出漏れがあっても
 * サーバー側が最終的に拒否する（クライアント側でハードブロックはしない
 * — 実装指示どおり）。
 *
 * **完全な外部名の組み立て**: バックエンド
 * `apps/banto-hub/core/src/hub.rs::build_catalog` の
 * `format!("{}.{}.{}", conn.name, group.name, tag.name)` と同じ規則
 * （{@link buildExternalName}）。
 *
 * **演算タグの式からのタグ参照検出**: banto-expr のタグ参照は必ず
 * ちょうど3セグメント（`crates/banto-expr/src/parser.rs::parse_tag_ref_rest`
 * が「3セグメント（接続.グループ.タグ）までです」を強制）で、各セグメントは
 * 字句規則上 ASCII の英字/`_`始まりで、英数字・`_`・内部の`-`のみを許す
 * （`crates/banto-expr/src/lexer.rs`）。完璧なレキサ移植はしない
 * （実装指示）が、この字句規則を写した正規表現で1トークンずつ抽出し、
 * 前後の境界条件（**`-` は減算演算子にも識別子の一部にもなるため非対称** -
 * {@link IDENT_SEGMENT} と `TAG_REF_PATTERN` の doc 参照）を課す
 * ことで、`a.b.c` が `a.b.c2` の一部として誤マッチすることを防ぐ
 * （境界チェックにより、正規表現のグリーディマッチが自然に全体の識別子を
 * 飲み込むため、部分文字列一致にはならない）。
 */
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';

/**
 * `{connection}.{group}.{tag}` の完全外部名。バックエンド
 * `hub.rs::build_catalog` の `format!("{}.{}.{}", ...)` と同じ組み立て規則。
 */
export function buildExternalName(
	connectionName: string,
	groupName: string,
	tagName: string
): string {
	return `${connectionName}.${groupName}.${tagName}`;
}

/**
 * banto-expr の識別子セグメント（ASCII）。**`crates/banto-expr/src/lexer.rs`
 * の識別子規則をそのまま写す**: 開始は英字か `_`、継続は英数字か `_`、
 * **`-` は直後に継続文字が続くときだけ吸収される**（同モジュール doc
 * 「識別子とハイフンの綱引き」）。
 *
 * #379 レビュー対応（2回目）で `[A-Za-z_][A-Za-z0-9_-]*` の簡略版から
 * ここまで厳密にした - 簡略版だと末尾・連続のハイフンまで貪欲に飲み込み、
 * `a.b.c--line1.fast.tag`（lexer では `a.b.c` と `line1.fast.tag` の2参照）で
 * `a.b.c` を落とす・`a.b.c--1` を1トークンとして拾う、といった食い違いが出る。
 */
const IDENT_SEGMENT = '[A-Za-z_][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)*';

/**
 * 「ハイフンを含まない識別子文字の連続」（`TAG_REF_PATTERN` の後読み専用）。
 * `IDENT_SEGMENT` をそのまま後読みに使うと、それ自身が `-` を含むため
 * `a.b.c--line1...` の `c--` にも一致して参照を落とす（#379 レビュー指摘）。
 * `-` の直前が**識別子の一部**かどうかだけを見たいので、run にはハイフンを
 * 含めない。
 */
const IDENT_RUN_NO_HYPHEN = '[A-Za-z_][A-Za-z0-9_]*';

/**
 * 式中の3セグメントのタグ参照トークン（`接続.グループ.タグ`）を検出する
 * グローバル正規表現。前後の境界チェックで、より長い識別子・より長い
 * ドット連結の一部を誤って切り出さないようにする。
 *
 * **`-` の扱い**（#379 レビュー対応。正は `crates/banto-expr/tests/compile.rs`
 * の `tag_ref_after_minus_operator_is_a_separate_reference` /
 * `hyphen_between_identifiers_is_absorbed_into_the_reference` で実 lexer に
 * 対して固定してある）: 旧実装は後読みを `(?<![A-Za-z0-9_.-])` として
 * **直前が `-` の候補を一律に拒否**していたが、lexer が `-` を識別子へ
 * 吸収するのは「直前が識別子のときだけ」（`crates/banto-expr/src/lexer.rs`
 * の「識別子とハイフンの綱引き」）。そのため `1-line1.fast.tag`・
 * `-line1.fast.tag`・`(a.b.c)-line1.fast.tag` はどれも `line1.fast.tag` を
 * 参照しているのに見落としていた（削除影響の検出漏れと、「一覧から挿入」の
 * 循環除外が効かない不具合）。後読みを2段に分けて lexer と一致させる:
 *
 * - `(?<![A-Za-z0-9_])(?<!\.\s*)` … 識別子の途中・ドット連結の途中から
 *   切り出さない
 * - `(?<!IDENT_RUN_NO_HYPHEN-)` … **識別子に吸収された `-`** の直後から
 *   切り出さない（`a-line1.fast.tag` の参照は `a-line1.fast.tag` であって
 *   `line1.fast.tag` ではない）。run にハイフンを含めない理由は
 *   {@link IDENT_RUN_NO_HYPHEN} 参照。
 *
 * 後ろ側も同じ非対称性を持つ（`(?![A-Za-z0-9_])(?!\s*\.)(?!-[A-Za-z0-9_])`）:
 * 続く `-` が識別子の一部なのは「その後ろに継続文字があるとき」だけなので、
 * `a.b.c--1` や `a.b.c--line1.fast.tag` の `a.b.c` はちゃんと参照として
 * 切り出される（`a.b.c-1` は1トークンのまま）。
 *
 * **`.` の周りの空白**（#379 レビュー対応。正は同 compile.rs の
 * `whitespace_around_dots_is_allowed_in_a_tag_reference`）: lexer は空白・
 * タブ・改行を捨て、parser はトークン列（識別子と `.`）しか見ないので、
 * `conn . group . tag` や改行を挟んだ形も**有効な3セグメント参照**。
 * セパレータを {@link DOT_WITH_SPACES} にして拾い、
 * {@link extractTagRefTokens} が空白を除いた canonical 形
 * （`conn.group.tag` - `referenced_tags()` が返すのと同じ形）へ正規化する。
 */
const DOT_WITH_SPACES = '\\s*\\.\\s*';

const TAG_REF_PATTERN = new RegExp(
	`(?<![A-Za-z0-9_])(?<!\\.\\s*)(?<!${IDENT_RUN_NO_HYPHEN}-)${IDENT_SEGMENT}${DOT_WITH_SPACES}${IDENT_SEGMENT}${DOT_WITH_SPACES}${IDENT_SEGMENT}(?![A-Za-z0-9_])(?!\\s*\\.)(?!-[A-Za-z0-9_])`,
	'g'
);

/**
 * 完全外部名がそのまま式中のタグ参照として書けるか（3セグメントすべてが
 * {@link IDENT_SEGMENT} に完全一致するか）。
 *
 * #342 段階C（#379 再レビュー対応）で追加: レジストリ側のタグ名・接続名・
 * グループ名の検証は「空でない・最大長」程度しか課しておらず、banto-expr の
 * 識別子文法より広い（日本語名・空白入り・末尾ハイフンなどが通る）。その
 * ようなタグは式から参照できないので、「一覧から挿入」の候補から外す
 * （`expressionInsert.ts::insertionBlockReason`）。判定は上の
 * {@link TAG_REF_PATTERN} と**同じ `IDENT_SEGMENT` を使う**（正規表現を
 * 二重に持たない）。`TAG_REF_PATTERN` が「文中から切り出す」ための境界
 * チェック付きなのに対し、こちらは「名前全体が参照トークンそのものか」を
 * 見るためアンカー（`^...$`）で完全一致させる。
 */
const FULL_TAG_REF_PATTERN = new RegExp(`^${IDENT_SEGMENT}\\.${IDENT_SEGMENT}\\.${IDENT_SEGMENT}$`);

/**
 * {@link FULL_TAG_REF_PATTERN} 参照。`abc-`（末尾ハイフン）や `a--b`
 * （連続ハイフン）が弾かれるのは、{@link IDENT_SEGMENT} が lexer の
 * 「`-` は直後に継続文字があるときだけ吸収」規則をそのまま写しているため
 * （#379 レビュー対応の2回目までは別の `DANGLING_HYPHEN_PATTERN` で後から
 * 弾いていたが、`IDENT_SEGMENT` 側を厳密にしたので不要になった）。
 */
export function isExpressionRepresentableName(externalName: string): boolean {
	return FULL_TAG_REF_PATTERN.test(externalName);
}

/**
 * 式中に現れる3セグメントのタグ参照トークンをすべて抽出する（重複含む）。
 *
 * 戻り値は**空白を除いた canonical 形**（`conn . group . tag` と書かれて
 * いても `conn.group.tag` を返す） - `CompiledExpr::referenced_tags()` が
 * 返す形と同じにするため（`TAG_REF_PATTERN` の doc comment「`.` の周りの
 * 空白」参照）。`isExpressionRepresentableName` は**名前**の判定なので
 * 空白を許さないままで、こちらとは非対称。
 */
export function extractTagRefTokens(expression: string): string[] {
	return (expression.match(TAG_REF_PATTERN) ?? []).map((token) => token.replace(/\s+/g, ''));
}

/** `expression` が `externalName` を（境界付きの）タグ参照として含むか。 */
export function expressionReferencesExternalName(
	expression: string,
	externalName: string
): boolean {
	return extractTagRefTokens(expression).includes(externalName);
}

/** 削除対象タグを参照している演算タグ1件の情報（確認ダイアログ表示用）。 */
export interface ReferencingTag {
	id: number;
	name: string;
	/** この参照元タグ自身の完全外部名。 */
	externalName: string;
	/** 削除対象を参照している式のソース全文。 */
	expression: string;
}

/**
 * ロード済みの `tags` のうち、`tagKind === 'computed'` かつ `expression` が
 * `targetExternalName` をタグ参照として含むものを探す。削除対象自身
 * （`targetTagId`）は除外する（自己参照は通常ないが、念のため）。
 */
export function findReferencingComputedTags(
	targetTagId: number,
	targetExternalName: string,
	tags: Tag[],
	groups: CollectionGroup[],
	connections: PlcConnection[]
): ReferencingTag[] {
	const referencing: ReferencingTag[] = [];
	for (const tag of tags) {
		if (tag.id === targetTagId) continue;
		if (tag.tagKind !== 'computed') continue;
		if (!tag.expression) continue;
		if (!expressionReferencesExternalName(tag.expression, targetExternalName)) continue;

		const group = groups.find((g) => g.id === tag.collectionGroupId);
		const connection = group ? connections.find((c) => c.id === group.plcConnectionId) : undefined;
		const externalName =
			group && connection ? buildExternalName(connection.name, group.name, tag.name) : tag.name;
		referencing.push({ id: tag.id, name: tag.name, externalName, expression: tag.expression });
	}
	return referencing;
}

/**
 * `window.confirm` に渡す削除確認メッセージ。参照が無くても完全外部名は
 * 必ず出す（実装指示: 「`${name} を削除しますか？` だけに戻さない」）。
 * 参照がある場合は一覧と、削除すると参照が壊れる／登録検証で失敗し得る旨の
 * 警告を追加する。
 */
export function formatDeleteConfirmMessage(
	targetExternalName: string,
	referencing: ReferencingTag[]
): string {
	const lines = [`${targetExternalName} を削除しますか？`];
	if (referencing.length > 0) {
		lines.push('');
		lines.push('次の演算タグの式がこのタグを参照しています:');
		for (const ref of referencing) {
			lines.push(`- ${ref.externalName}`);
		}
		lines.push('');
		lines.push(
			'削除すると参照が壊れます。これらの演算タグの式を先に修正しないと、削除自体がサーバー側の検証で失敗する可能性があります。'
		);
	}
	return lines.join('\n');
}
