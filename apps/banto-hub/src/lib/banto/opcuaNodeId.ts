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

export function opcUaNodeId(namespaceUri: string, externalName: string): string {
	return `nsu=${namespaceUri};s=${externalName}`;
}

/** 公開される（`string` でない）行の NodeId を 1 行 1 件で返す。 */
export function opcUaNodeIdLines(namespaceUri: string, targets: NodeIdTarget[]): string[] {
	return targets
		.filter((t) => t.dataType !== 'string')
		.map((t) => opcUaNodeId(namespaceUri, t.externalName));
}
