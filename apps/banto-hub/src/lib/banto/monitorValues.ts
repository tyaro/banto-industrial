/**
 * ライブタグモニタ（`(app)/monitor/+page.svelte`）向けの、WS `data`
 * メッセージの値と catalog 行（`rows`）を結び付ける依存ゼロの純関数群。
 * `monitorFilter.ts`（絞り込み）と同じ「画面側は薄いグルーコードに徹し、
 * ロジック本体は純関数へ切り出す」方針。
 *
 * **2026-08-31 実機診断で特定した不具合（このファイルが直接の修正）**:
 * タグ行は1999件正しく表示され、WS も「接続中」と表示されるのに、値が
 * 全て `--`・品質が「陳腐化」のまま変わらない。サーバー側は実機で正常と
 * 確認済み（`/api/status`・`/api/values` は正しく値を返し、素の WebSocket
 * で `/api/tag-stream` に接続すると初期スナップショットが1件届く）。
 *
 * 原因は `+page.svelte` の初期化 `$effect` にあった:
 *
 * ```
 * $effect(() => {
 *   void reloadCatalog();      // catalog を非同期に取得して rows を作る
 *   void reloadAdmin();
 *   const { disconnect, resubscribe } = connectTagStream(...);  // 直後に WS を張る
 *   ...
 * });
 * ```
 *
 * `reloadCatalog()` は `await` されないまま次の行で `connectTagStream()` が
 * 呼ばれ、WS 接続が即座に張られる。WS 側は接続確立後ただちに
 * `subscribe` を送り、サーバーはその場で現在値の初期スナップショットを
 * 返す（`stream.rs` の `subscribe` 処理は同期的にメモリ上の現在値を読むだけ
 * で、DB を経由する catalog 構築より大幅に速い）。そのため、**catalog の
 * HTTP 応答（`rows` が空でなくなるタイミング）より先に WS の初期
 * スナップショットが届くレースが実機の速度では常態的に発生する**。
 *
 * 旧実装の `applyStreamData` は届いた値を `rows.map(...)` でその場の
 * `rows`（まだ空配列）に対して突き合わせていたため、レースに負けると
 * 届いた値は一致する行が無いままただ捨てられていた。しかも
 * `onData` ハンドラは突き合わせの成否に関わらず無条件で
 * `awaitingSnapshot = false` にしていたため、「初期スナップショット受信
 * 済み」の状態にはなるのに、肝心の値は `rows` へ一度も反映されない
 * まま止まる。加えて実機の PLC 値は全て `0` で変化しないため
 * `mode: 'on_change'` では以後の配信が一切無く、一度取りこぼすと
 * 二度と回復しない（このファイル冒頭の依頼メモの「取りこぼしは致命的」
 * はこの意味）。
 *
 * 修正方針: 「WS から届いた最新値」を `rows` とは独立したキャッシュ
 * （`external_name` → 値のマップ）として保持し、
 * - WS からの `data` 到着時は、このマップへ畳み込んだ上で `rows` に反映
 *   する（`mergeTagValues` + `applyTagValues`）。
 * - catalog 再取得（`reloadCatalog`）で行を作り直すときも、まずこの
 *   マップを見る（`applyTagValues` を再利用）。
 *
 * これにより、catalog 構築と WS 初期スナップショットのどちらが先に
 * 終わっても、両方が終わった時点で必ず値が反映される（順序に依存しない）。
 */

/** 1タグ分の最新値（WS `data` の1要素から `tag` を除いたもの）。 */
export interface RowValue {
	v: number | null;
	q: string;
	t: number;
}

/** `applyTagValues` が要求する最小限の行の形。`external_name` をキーに
 * 値を突き合わせ、`RowValue` の3フィールドを持つ。`+page.svelte` の
 * `Row`（`CatalogTagEntry` を拡張）はこれを満たすスーパーセット。 */
export type ValueBearingRow = RowValue & { external_name: string };

/**
 * WS `data` メッセージの `values` 配列を、外部名をキーにした最新値
 * マップへ畳み込む。既存マップは書き換えず新しい `Map` を返す（純関数） -
 * 呼び出し側が Svelte の `$state` 相当で参照差し替えによる再描画を
 * 期待している場合でも安全に使える。
 *
 * 同じ `tag` が配列内に複数回出現した場合は後勝ち（サーバーは1メッセージ
 * 内で同一タグを重複させない設計だが、念のため配列の並び順を正とする）。
 */
export function mergeTagValues<V extends { tag: string } & RowValue>(
	current: ReadonlyMap<string, RowValue>,
	values: readonly V[]
): Map<string, RowValue> {
	const next = new Map(current);
	for (const value of values) {
		next.set(value.tag, { v: value.v, q: value.q, t: value.t });
	}
	return next;
}

/**
 * 行配列へ最新値マップを適用する。マップに対応するエントリが無い行は
 * **同一参照のまま**返す（無駄な再レンダリング・`flash` 誤爆を避ける）。
 * 値が既存行と完全一致する場合も同一参照を返す。
 *
 * catalog 再構築時（`reloadCatalog`）・WS `data` 受信時（`applyStreamData`）
 * の両方から呼ばれる共通の突き合わせロジック - これを1箇所に切り出す
 * ことで、「catalog 構築と WS 初期スナップショットのどちらが先でも
 * 最終的に必ず値が反映される」という不変条件を1つの関数の責務として
 * 保証する。
 */
export function applyTagValues<T extends ValueBearingRow>(
	rows: readonly T[],
	values: ReadonlyMap<string, RowValue>
): T[] {
	return rows.map((row) => {
		const update = values.get(row.external_name);
		if (!update) return row;
		if (update.v === row.v && update.q === row.q && update.t === row.t) return row;
		return { ...row, v: update.v, q: update.q, t: update.t };
	});
}
