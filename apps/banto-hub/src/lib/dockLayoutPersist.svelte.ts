/**
 * ドックの枠（`ThreePaneDock`）の配置を保存の口（`dockLayoutStore.ts`）へ流す `$effect`（#1253）。
 * `/tags` が使う（もとは `/monitor` と共用）。
 *
 * **`layoutSnapshot()` の読み取りを追跡する**（PR #1268 レビュー P2）。v7.9.0 の部品は、狭い
 * 表示の「既定に戻す」・復元で `dock.layout` を先に換え、`await` の後で保存の対象（控えた広い
 * 配置 `wideLayout`、`$state.raw`）を確定する。`dock.layout` だけを見て `layoutSnapshot()` を
 * `untrack` の内側で呼ぶと、先に走った effect が古い配置を読み、その後の確定を拾わない。
 * `layoutSnapshot()` は `narrowShown`・`wideLayout`・`dock.savedLayout()`（`dock.layout`）を
 * 読むので、effect の中で `untrack` の外で呼べば、そのどれが換わっても走り直す。`untrack` は
 * 保存（`store.save`）だけに掛ける（保存の口の中の読み書きを依存にしない）。
 *
 * **広い ⇄ 狭いの切り替えの途中は保存しない**（PR #1268 再レビュー P2）。部品の `switchNarrow`
 * は、行き先の木を `DockHost.replaceLayout` で先に `dock.layout` へ当て、その `await`（フォーカス
 * が枠の中にあれば `tick()` を待ってフォーカスを直す）の後で `wideLayout`・`narrowShown` を確定
 * する。広い → 狭いの間は `narrowShown` がまだ false なので、`layoutSnapshot()` は狭い表示の一時
 * の木を返してしまう（狭い → 広いの間は控えた広い配置を返すが、確定前の値には違いないので同じく
 * 見送る）。部品の公開の口に「確定した」印は無いので、求める表示（`narrowOverride` に渡す値）と
 * 実際の表示（`bind:narrow`。部品の中で `narrowShown` を `$effect.pre` で写した値）が食い違って
 * いる間は保存しない。確定すると `narrow` が換わって effect が走り直し、確定後の
 * `layoutSnapshot()` を渡す（変わっていなければ保存の口が同じ値として書かない）。
 *
 * 切り替えが断られた（`beforePanelChange`。今のページは使っていない）・入力の変換中で
 * 待たされているときも食い違ったままになり、その間の配置の変更は保存しない。次に境界をまたぐか
 * 切り替えが通れば、その時点の配置を保存する（断られた表示は求める表示ではないので、その間の
 * 変更を見送るのは許す）。
 *
 * `beforeBaseline` は、最初の保存（保存の口が基準として受け取るだけの 1 回）の直前に 1 回だけ
 * 呼ぶ。マウントした既定の配置をアプリの規則で直す（低い画面ではログを帯で始める、
 * `dockLogBand.svelte.ts`）ための口で、直した配置が基準になり、利用者の変更としては書かない。
 * 直すのに待ちがある（狭い表示で部品の控えを置き換える）ときは、`holding()` が真の間は基準も
 * 変更も取らない。真から偽に変わると effect が走り直し、直した後の配置を基準（または変更）にする。
 * `rebaseline()` が真なら（サーバーの保存値が使えず部品が既定へ戻し、それを直した後）、その配置を
 * 送らずに基準へ置き直す（`store.rebase`。利用者の変更ではないので書かない）。
 *
 * コンポーネントの初期化中（`$effect` を張れる所）に呼ぶ。
 */
import { untrack } from 'svelte';
import type { DockLayoutStore } from './dockLayoutStore';
import type { StoredDockLayout } from './dockLayout';

/** 保存に要る部品の口（`ThreePaneDock` の `layoutSnapshot`）。 */
export interface ThreePaneSnapshotSource {
	layoutSnapshot(): StoredDockLayout;
}

/** 狭い表示の、求める値と実際の値（食い違っている間は切り替えの途中）。 */
export interface NarrowDisplay {
	/** 部品の `narrowOverride` に渡している値（`undefined` = 部品が枠の幅で決める）。 */
	wanted(): boolean | undefined;
	/** 部品の `bind:narrow`（いま狭い表示を出しているか）。 */
	shown(): boolean;
}

/** 最初の保存の基準を直す口（`dockLogBand.svelte.ts` の `lowScreenLogBand`）。 */
export interface BaselineHook {
	/** 最初の保存（基準）の直前に 1 回。 */
	beforeBaseline(): void;
	/** 直している途中か（反応する値。真の間は保存しない）。 */
	holding(): boolean;
	/** 直した後の配置を、送らずに基準へ置き直すか（1 回だけ真）。 */
	rebaseline?(): boolean;
}

export function persistThreePaneLayout(
	pane: () => ThreePaneSnapshotSource | undefined,
	store: Pick<DockLayoutStore, 'save' | 'rebase'>,
	narrow?: NarrowDisplay,
	baseline?: BaselineHook
): void {
	let prepared = false;
	$effect(() => {
		const current = pane();
		if (!current) return;
		if (narrow) {
			const wanted = narrow.wanted();
			if (wanted !== undefined && wanted !== narrow.shown()) return;
		}
		if (!prepared) {
			prepared = true;
			if (baseline) untrack(() => baseline.beforeBaseline());
		}
		if (baseline?.holding()) return;
		const snapshot = current.layoutSnapshot();
		untrack(() => {
			if (baseline?.rebaseline?.()) store.rebase(snapshot);
			else store.save(snapshot);
		});
	});
}
