/**
 * タグ登録の表の値の列（#1255 段階 ②）が使う WS 購読の状態。
 *
 * タグモニタ（`/monitor`、#1255 段階 ③ で `/tags` に吸収して削除）の仕組みを、`/tags` の中で
 * 使えるように 1 つの部品にまとめたもの: `connectTagStream`（1 本を維持）・
 * `subscriptionPatternsFor`（ツリーで選んだ範囲。「すべて」なら全タグ）・
 * `mergeTagValues`（最新値のマップ）・`monitorStreamView`（再接続中・バックプレッシャ
 * （close 1013）・止まっている間の表示）。購読は画面に見えている行ではなく、
 * ツリーで選んだ範囲（2026-11-21 オーナー決定。見えている行だけの購読は段階 3）。
 * 値を見張る窓（#1254）は同じ部品を別のインスタンスで使い、`ids`・`enabled` で
 * 見張っているタグだけを窓が出ている間だけ購読する（ツリーの購読とは別の 2 本目のソケット。
 * 部品の状態（再接続・バックプレッシャ・強調）がインスタンスごとに独立で、ツリーの範囲を
 * 一時的に差し替える方式より単純で、片方の切断がもう片方に波及しない）。
 * 見張りは**タグ ID で購読し、値も ID で持つ**（2026-11-22 オーナー決定、PR #1282 の再レビュー。
 * WS の `subscribe` の `ids`、docs/hub/tag-server-design.md §5.2）。外部名で購読・照合すると、
 * 未知の具体名 1 件で購読全体が拒否され、ドット入りの既存の接続名・グループ名ではグループの
 * ワイルドカードの形が不正になり、外部名を別のタグに使い回すと旧タグの行に新タグの値が出る
 * ため。ID の集合が変わったら購読し直して新しいスナップショットを受ける（足したタグの今の値が、
 * 次に変わるまで出ないことが無いように）。スナップショットは置き換え（`replaceOrMergeTagValues`）
 * なので、購読し直すと、消えたタグの古い値は落ちる（`resubscribe()`。見張りは config_changed
 * で呼ぶ）。サーバーが購読を拒否した（`error` フレーム）ら、状態の行は拒否を出す
 * （`monitorStreamView.ts` の `rejected`。「再接続」で購読し直す）。
 *
 * コンポーネントの初期化の中で 1 回呼ぶ（`$effect` を使う）。ページを離れると接続を閉じる。
 * 値は `external_name`（`{接続}.{グループ}.{タグ}`）をキーに持つ（`ids` で購読したインスタンスは
 * `monitorValues.ts::tagIdValueKey` をキーに持つ）。行の状態は持たない
 * （表の行・選択・保留中の編集には触らない）。`values` は更新のたびに新しい `Map` へ
 * 差し替える（セルの描画が追跡できるように）。
 */
import { untrack } from 'svelte';
import { connectTagStream } from './tagMonitorAdmin';
import { subscriptionPatternsFor } from './monitorSubscription';
import type { MonitorTreeFilter } from './monitorFilter';
import { keyValuesById, replaceOrMergeTagValues, type RowValue } from './monitorValues';
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
	/**
	 * ツリーの選択ではなく、このタグ ID の一覧で購読する（値を見張る窓、#1254）。値は
	 * `monitorValues.ts::tagIdValueKey` をキーに持つ。空の一覧は送らない（`enabled` で止める）。
	 * 一覧の中身が変わったら購読し直す（新しいスナップショットで足したタグの今の値を受ける）。
	 */
	ids?: () => readonly number[];
	/**
	 * 接続しているか（既定は常に true）。false の間は接続を閉じる。見張りの窓は、画面に
	 * 出ていて一覧が空でない間だけ true にする。
	 */
	enabled?: () => boolean;
	/**
	 * 設定（タグ・接続・グループ）が別の所で変わったという WS の知らせ（#1255）。
	 * 一覧を読み直すかは呼び出し側が決める（編集の途中を巻き込まないため）。
	 */
	onConfigChanged?: () => void;
}

export interface TagValueStream {
	/** 外部名（`ids` で購読したインスタンスは `monitorValues.ts::tagIdValueKey`）→ 最新値。 */
	readonly values: ReadonlyMap<string, RowValue>;
	readonly mode: CellDisplayMode;
	readonly labels: { value: string; quality: string };
	readonly status: StreamStatusLine;
	/**
	 * 値または品質が直前から変わった直後か（行の一時的な強調用、数百 ms で外れる）。
	 * スナップショット（接続・購読し直しの直後）では立てない。外部名ごとの小さな
	 * `$state` なので、変わった行のセルだけが追跡して描画し直す（列・行の定義は作り直さない）。
	 */
	isFlashing: (key: string) => boolean;
	/**
	 * 止まった購読を、利用者の操作で再開する（状態の行の「再接続」）。close `1008` なら
	 * 接続し直し、購読が拒否されている（`rejected`）なら購読し直す。
	 */
	resume: () => void;
	/**
	 * 今のソケットの上で購読し直す（パターンが同じでも）。次のスナップショットで値の
	 * マップを置き換えるので、スナップショットに無い外部名の値は落ちる。ソケットが
	 * 開いていなければ何もしない（次に開いたときの購読がスナップショットを受ける）。
	 */
	resubscribe: () => void;
}

export function createTagValueStream(sources: TagValueStreamSources): TagValueStream {
	const byId = sources.ids !== undefined;
	/** 購読の範囲（ツリーの範囲のパターン、または見張りのタグ ID）。 */
	const targetNow = (): string[] | { ids: readonly number[] } =>
		sources.ids !== undefined
			? { ids: sources.ids() }
			: subscriptionPatternsFor(sources.treeFilter(), sources.connections(), sources.groups());
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
		if (!(sources.enabled?.() ?? true)) return;
		// 片付けた接続の遅れた知らせで、表示の状態・値を変えないための印。
		let alive = true;
		const { disconnect, resubscribe, resume } = connectTagStream(
			{
				onData: (received, { snapshot }) => {
					if (!alive) return;
					// ID で購読したインスタンスは ID をキーにする（外部名の使い回しに惑わされない）。
					const incoming = byId ? keyValuesById(received) : received;
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
				// 一覧の読み直しは呼び出し側（登録の画面）が決める。ここで読み直すと、
				// 作成・編集の途中や保留中の編集を巻き込むため（#1255）。
				onConfigChanged: () => {
					if (alive) sources.onConfigChanged?.();
				},
				onRejected: (code, detail) => {
					if (alive) dispatch({ type: 'rejected', code, detail });
				},
				onStatusChange: (connected, closeCode) => {
					if (!alive) return;
					dispatch(connected ? { type: 'connected' } : { type: 'disconnected', code: closeCode });
				},
				probeSession: probeSessionAfterReconnectFailures,
				onHalt: (action) => {
					if (!alive) return;
					dispatch({ type: 'halted', action });
					if (action.kind !== 'recheckSession') return;
					recheckSessionAfterStreamClose().then(
						() => {
							if (!alive) return;
							dispatch({ type: 'resumed' });
							resume();
						},
						() => {
							if (!alive) return;
							dispatch({ type: 'recheckFailed', reason: action.reason });
						}
					);
				}
			},
			() => untrack(targetNow)
		);
		resubscribeSocket = resubscribe;
		resumeSocket = resume;
		return () => {
			alive = false;
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
	// 見張りは、タグ ID の集合が変わったら購読し直す（`ids` の doc comment）。
	const subscriptionKey = $derived.by(() => {
		const target = targetNow();
		return Array.isArray(target)
			? target.join('\n')
			: `#ids:${[...target.ids].sort((a, b) => a - b).join(',')}`;
	});
	function resubscribe(): void {
		if (resubscribeSocket === null) return;
		resubscribeSocket();
		dispatch({ type: 'resubscribed' });
	}

	$effect(() => {
		void subscriptionKey;
		// 止めている間（見張りの窓: 一覧が空・窓が出ていない）は、空のパターンを送らない。
		if (!(sources.enabled?.() ?? true)) return;
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
		isFlashing: (key) => flashing[key] === true,
		resume: () => {
			if (untrack(() => view.rejected !== null && view.connected)) {
				resubscribe();
				return;
			}
			dispatch({ type: 'resumed' });
			resumeSocket?.();
		},
		resubscribe
	};
}
