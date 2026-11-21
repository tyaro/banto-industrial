/**
 * タグ登録の表の値の列（#1255 段階 ②）が使う WS 購読の状態。
 *
 * `/monitor`（`routes/(app)/monitor/+page.svelte`）と同じ仕組みを、`/tags` の中で
 * 使えるように 1 つの部品にまとめたもの: `connectTagStream`（1 本を維持）・
 * `subscriptionPatternsFor`（ツリーで選んだ範囲。「すべて」なら全タグ）・
 * `mergeTagValues`（最新値のマップ）・`monitorStreamView`（再接続中・バックプレッシャ
 * （close 1013）・止まっている間の表示）。購読は画面に見えている行ではなく、
 * ツリーで選んだ範囲（2026-11-21 オーナー決定。見えている行だけの購読は段階 3）。
 *
 * コンポーネントの初期化の中で 1 回呼ぶ（`$effect` を使う）。ページを離れると接続を閉じる。
 * 値は `external_name`（`{接続}.{グループ}.{タグ}`）をキーに持ち、行の状態は持たない
 * （表の行・選択・保留中の編集には触らない）。`values` は更新のたびに新しい `Map` へ
 * 差し替える（セルの描画が追跡できるように）。
 */
import { untrack } from 'svelte';
import { connectTagStream } from './tagMonitorAdmin';
import { subscriptionPatternsFor } from './monitorSubscription';
import type { MonitorTreeFilter } from './monitorFilter';
import { replaceOrMergeTagValues, type RowValue } from './monitorValues';
import {
	cellDisplayMode,
	initialStreamView,
	monitorColumnLabels,
	streamStatusLine,
	streamViewReducer,
	type CellDisplayMode,
	type StreamStatusLine,
	type StreamViewEvent
} from './monitorStreamView';
import {
	probeSessionAfterReconnectFailures,
	recheckSessionAfterStreamClose
} from './sessionRecheck';
import type { CollectionGroup, PlcConnection } from './tagRegistryAdmin';

export interface TagValueStreamSources {
	treeFilter: () => MonitorTreeFilter;
	connections: () => PlcConnection[];
	groups: () => CollectionGroup[];
}

export interface TagValueStream {
	/** 外部名 → 最新値。 */
	readonly values: ReadonlyMap<string, RowValue>;
	readonly mode: CellDisplayMode;
	readonly labels: { value: string; quality: string };
	readonly status: StreamStatusLine;
	/** close `1008` で止まった購読を、利用者の操作で再開する。 */
	resume: () => void;
}

export function createTagValueStream(sources: TagValueStreamSources): TagValueStream {
	let values = $state<Map<string, RowValue>>(new Map());
	let view = $state(initialStreamView());
	const mode = $derived(cellDisplayMode(view));
	const labels = $derived(monitorColumnLabels(mode));
	const status = $derived(streamStatusLine(view));

	let resubscribeSocket: (() => void) | null = null;
	let resumeSocket: (() => void) | null = null;

	/** 読み書きする effect が自分を起こし続けないよう、`view` を依存に加えない。 */
	function dispatch(event: StreamViewEvent): void {
		untrack(() => {
			view = streamViewReducer(view, event);
		});
	}

	$effect(() => {
		const { disconnect, resubscribe, resume } = connectTagStream(
			{
				onData: (incoming, { snapshot }) => {
					// スナップショットは置き換え（空でも）、差分はマージ。
					if (snapshot || incoming.length > 0) {
						values = replaceOrMergeTagValues(values, incoming, snapshot);
					}
					dispatch({ type: 'data' });
				},
				// 登録の画面は自分で一覧を読み直す（保留中の編集を巻き込まない）。
				onConfigChanged: () => {},
				onStatusChange: (connected, closeCode) => {
					dispatch(connected ? { type: 'connected' } : { type: 'disconnected', code: closeCode });
				},
				probeSession: probeSessionAfterReconnectFailures,
				onHalt: (action) => {
					dispatch({ type: 'halted', action });
					if (action.kind !== 'recheckSession') return;
					recheckSessionAfterStreamClose().then(
						() => {
							dispatch({ type: 'resumed' });
							resume();
						},
						() => {
							dispatch({ type: 'recheckFailed', reason: action.reason });
						}
					);
				}
			},
			() => subscriptionPatternsFor(sources.treeFilter(), sources.connections(), sources.groups())
		);
		resubscribeSocket = resubscribe;
		resumeSocket = resume;
		return () => {
			resubscribeSocket = null;
			resumeSocket = null;
			disconnect();
		};
	});

	// ツリーの選択（または接続・グループの一覧）でパターンが変わったら、既存のソケット上で
	// 購読し直す（再接続はしない）。一覧を読み終える前は「すべて」（`subscriptionPatternsFor` が
	// 引けない id を全件に倒す）なので、読み終えたときにも購読し直すよう、対象ではなくパターンを見る。
	const subscriptionKey = $derived(
		subscriptionPatternsFor(sources.treeFilter(), sources.connections(), sources.groups()).join(
			'\n'
		)
	);
	$effect(() => {
		void subscriptionKey;
		resubscribeSocket?.();
		dispatch({ type: 'resubscribed' });
	});

	return {
		get values() {
			return values;
		},
		get mode() {
			return mode;
		},
		get labels() {
			return labels;
		},
		get status() {
			return status;
		},
		resume: () => {
			dispatch({ type: 'resumed' });
			resumeSocket?.();
		}
	};
}
