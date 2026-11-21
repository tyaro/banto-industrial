/**
 * ツリー選択の型（タグモニタの購読範囲の決定 `monitorSubscription.ts`・値の購読
 * `tagValueStream.svelte.ts` が使う）。
 *
 * T18-4a のクライアント側の絞り込み純関数 `filterMonitorRows` は、タグモニタ
 * （`/monitor`）を `/tags` に吸収して削除したとき（#1255 段階 ③）に一緒に消した。
 */

/** ツリー側の選択状態。tags ページの `TreeFilter` と同じ3値。 */
export type MonitorTreeFilter =
	{ type: 'all' } | { type: 'connection'; id: number } | { type: 'group'; id: number };
