/**
 * TAG-UX-C 追補（2026-09-15、Drawer/Modal 誤爆クローズ防止）: `Drawer.svelte`
 * と `Modal.svelte` が共有する「Esc・オーバーレイクリック・× のどの経路が
 * 実際に `requestClose()`（＝`onRequestClose` 経由の確認を経て `onclose`）を
 * 呼んでよいか」の判定ロジック。
 *
 * 両コンポーネントは Svelte コンポーネント自体をユニットテスト対象外と
 * している（`formDirty.test.ts` 冒頭コメント「H5: フロントテスト基盤...
 * Svelte コンポーネントは対象外」参照 - `vitest.config.ts` が
 * `@sveltejs/vite-plugin-svelte` を導入しない最小構成のため）。実 DOM
 * での確認は E2E（`e2e/tests-banto-hub/banto-hub-tags-drawer-accidental-close.spec.ts`）
 * に譲り、ここでは分岐そのものを純関数として切り出してユニットテストする。
 *
 * - `close-button`（×）経由は `dirty` に関わらず常に許可する - × は誤爆
 *   経路ではなく意図的な操作であり、開いたまま留めるかどうかの最終判断は
 *   引き続き呼び出し側の `onRequestClose`（dirty 破棄確認・busy 抑止）に
 *   委ねる。ここで判定するのはあくまで「`requestClose()` を呼ぶかどうか」
 *   だけで、`onRequestClose` 自体の可否判定はこの関数の範囲外。
 * - `escape`/`overlay` 経由は `dirty` の間は許可しない（誤爆防止 - 未保存の
 *   入力があるときに Esc やオーバーレイクリックだけで確認なしに閉じる事故
 *   を止める）。
 */

/** Drawer/Modal を閉じようとした経路。 */
export type DrawerCloseSource = 'escape' | 'overlay' | 'close-button';

/**
 * `source` 経由で `requestClose()` を呼んでよいか判定する。
 * `false` の場合、呼び出し側は `requestClose()` を呼ばず、未保存の入力が
 * あることを知らせる `onBlockedClose` だけを呼ぶ（案内は呼び出し側の
 * 責務 - `Drawer.svelte`/`Modal.svelte` 冒頭コメント「banto-hub の型・
 * ストアを一切 import しない」規約により、ここではトースト等は出さない）。
 */
export function isCloseAllowed(source: DrawerCloseSource, dirty: boolean): boolean {
	if (source === 'close-button') return true;
	return !dirty;
}
