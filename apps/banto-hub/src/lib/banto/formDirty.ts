/**
 * フォームの「未保存変更（dirty）」判定（TAG-UX-C 一部、
 * docs/banto-hub-desktop-plan.md §9.4「dirty 状態を持ち、Esc、背景、×、
 * 別行選択、画面移動で同じ破棄確認を行う」）。
 *
 * `tags/+page.svelte` の `FormState` はプレーンなオブジェクト
 * （文字列・真偽値のみ、ネストなし〜浅いネスト）で、Svelte 5 の
 * `$state` から生成される値であり、`baseline`/`current` は常に
 * 同じキー順で構築された同型オブジェクト同士を比較する前提。そのため
 * ここでは深い構造比較を自前実装せず `JSON.stringify` の文字列比較で
 * 十分（キー順序が異なるオブジェクト同士の比較は意図的にサポートしない
 * — 必要になったら値ベースの deep-equal に置き換える）。
 */

/**
 * フォームの現在値が、開いた/保存した時点の基準値（baseline）から
 * 変更されているかを判定する。
 *
 * `baseline`/`current` は同じ形のオブジェクト（例: フォーム用の
 * スナップショット同士）を渡すこと。`JSON.stringify` できない値
 * （`undefined` を含むプロパティ、`Date`、循環参照等）を含む場合は
 * 想定した比較にならない点に注意。
 */
export function isFormDirty(baseline: unknown, current: unknown): boolean {
	return JSON.stringify(baseline) !== JSON.stringify(current);
}
