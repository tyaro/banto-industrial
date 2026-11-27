/**
 * OPC UA の NodeId 文字列を組む（#1324 段 2、設計 opcua-client-design.md §4.2）。
 *
 * 形は `nsu=<namespace_uri>;s=<外部名>`。外部名は `{接続}.{グループ}.{タグ}` で、
 * 組み立ては呼び出し側（`/tags` の `externalNameForTag`）のものをそのまま使う。
 * `string` 型のタグは OPC UA に出ない（core の `opcua.rs::data_type` が None）ので除く。
 */
export interface NodeIdTarget {
	dataType: string;
	externalName: string;
}

/**
 * Namespace URI の部分だけを符号化する。`;` は nsu の区切りなので `%3B` に、
 * `%` は符号の印なので先に `%25` にする（先に `;` を置き換えると `%3B` の `%` まで二重に
 * 符号化される）。banto-opcua の `NodeAddress::Display` と同じ。`s=` の外部名は触らない。
 */
function encodeNamespaceUri(uri: string): string {
	return uri.replaceAll('%', '%25').replaceAll(';', '%3B');
}

export function opcUaNodeId(namespaceUri: string, externalName: string): string {
	return `nsu=${encodeNamespaceUri(namespaceUri)};s=${externalName}`;
}

/** 公開される（`string` でない）行の NodeId を 1 行 1 件で返す。 */
export function opcUaNodeIdLines(namespaceUri: string, targets: NodeIdTarget[]): string[] {
	return targets
		.filter((t) => t.dataType !== 'string')
		.map((t) => opcUaNodeId(namespaceUri, t.externalName));
}
