/**
 * タグ登録の表の値の列（#1255 段階 ②）が使う WS 購読の状態。
 *
 * タグモニタ（`/monitor`、#1255 段階 ③ で `/tags` に吸収して削除）の仕組みを、`/tags` の中で
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

/** 値・品質が変わった行の一時的な強調の長さ（タグモニタの `FLASH_MS` と同じ）。 */
const FLASH_MS = 700;

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
	/**
	 * 値または品質が直前から変わった直後か（行の一時的な強調用、数百 ms で外れる）。
	 * スナップショット（接続・購読し直しの直後）では立てない。外部名ごとの小さな
	 * `$state` なので、変わった行のセルだけが追跡して描画し直す（列・行の定義は作り直さない）。
	 */
	isFlashing: (externalName: string) => boolean;
	/** close `1008` で止まった購読を、利用者の操作で再開する。 */
	resume: () => void;
}

export function createTagValueStream(sources: TagValueStreamSources): TagValueStream {
	let values = $state<Map<string, RowValue>>(new Map());
	let view = $state(initialStreamView());
	const mode = $derived(cellDisplayMode(view));
	const labels = $derived(monitorColumnLabels(mode));
	const status = $derived(streamStatusLine(view));

	/** 一時的な強調の最中の外部名 → タイマー。 */
	const flashTimers = new Map<string, ReturnType<typeof setTimeout>>();
	const flashing = $state<Record<string, boolean>>({});

	function triggerFlash(externalName: string): void {
		const running = flashTimers.get(externalName);
		if (running !== undefined) clearTimeout(running);
		flashing[externalName] = true;
		flashTimers.set(
			externalName,
			setTimeout(() => {
				flashTimers.delete(externalName);
				delete flashing[externalName];
			}, FLASH_MS)
		);
	}

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
						// 差分は、値・品質が前から変わったタグだけ一時的に強調する
						// （スナップショットは置き換えなので光らせない）。
						if (!snapshot) {
							for (const value of incoming) {
								const before = values.get(value.tag);
								if (!before || before.v !== value.v || before.q !== value.q) {
									triggerFlash(value.tag);
								}
							}
						}
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
			for (const timer of flashTimers.values()) clearTimeout(timer);
			flashTimers.clear();
			for (const key of Object.keys(flashing)) delete flashing[key];
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
		isFlashing: (externalName) => flashing[externalName] === true,
		resume: () => {
			dispatch({ type: 'resumed' });
			resumeSocket?.();
		}
	};
}
