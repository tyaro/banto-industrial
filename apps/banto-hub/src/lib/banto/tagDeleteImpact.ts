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
 * （実装指示）が、この字句規則を単純化した正規表現で1トークンずつ抽出し、
 * 前後が識別子継続文字（`A-Za-z0-9_.-`）でないことを境界条件として課す
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

/** banto-expr の識別子セグメント（ASCII、英字/`_`始まり、内部ハイフン許可の簡略版）。 */
const IDENT_SEGMENT = '[A-Za-z_][A-Za-z0-9_-]*';

/**
 * 式中の3セグメントのタグ参照トークン（`接続.グループ.タグ`）を検出する
 * グローバル正規表現。前後の境界チェック（識別子継続文字でないこと）で、
 * より長い識別子・より長いドット連結の一部を誤って切り出さないようにする。
 */
const TAG_REF_PATTERN = new RegExp(
	`(?<![A-Za-z0-9_.-])${IDENT_SEGMENT}\\.${IDENT_SEGMENT}\\.${IDENT_SEGMENT}(?![A-Za-z0-9_.-])`,
	'g'
);

/** 式中に現れる3セグメントのタグ参照トークンをすべて抽出する（重複含む）。 */
export function extractTagRefTokens(expression: string): string[] {
	return expression.match(TAG_REF_PATTERN) ?? [];
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
