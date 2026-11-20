/**
 * ドックの枠（`ThreePaneDock`）の配置を保存の口（`dockLayoutStore.ts`）へ流す `$effect`（#1253）。
 * `/monitor` と `/tags` が同じ形で使う。
 *
 * **`layoutSnapshot()` の読み取りを追跡する**（PR #1268 レビュー P2）。v7.9.0 の部品は、狭い
 * 表示の「既定に戻す」・復元で `dock.layout` を先に換え、`await` の後で保存の対象（控えた広い
 * 配置 `wideLayout`、`$state.raw`）を確定する。`dock.layout` だけを見て `layoutSnapshot()` を
 * `untrack` の内側で呼ぶと、先に走った effect が古い配置を読み、その後の確定を拾わない。
 * `layoutSnapshot()` は `narrowShown`・`wideLayout`・`dock.savedLayout()`（`dock.layout`）を
 * 読むので、effect の中で `untrack` の外で呼べば、そのどれが換わっても走り直す。`untrack` は
 * 保存（`store.save`）だけに掛ける（保存の口の中の読み書きを依存にしない）。
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

export function persistThreePaneLayout(
	pane: () => ThreePaneSnapshotSource | undefined,
	store: Pick<DockLayoutStore, 'save'>
): void {
	$effect(() => {
		const current = pane();
		if (!current) return;
		const snapshot = current.layoutSnapshot();
		untrack(() => store.save(snapshot));
	});
}
