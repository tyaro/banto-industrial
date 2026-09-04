/**
 * T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46「サーバー状態の拡充」）:
 * `hubStatus.ts` の `StatusResponse.system`（CPU%・バイト単位のメモリ値）を
 * 状態画面（`(app)/status/+page.svelte`）で人間可読に整形する純関数。
 *
 * サーバー側（`apps/banto-hub/core/src/system_info.rs`）は生値（バイト・
 * パーセント）しか返さない - 整形はこのファイルに切り出し、副作用を持たない
 * 純関数として vitest 対象にする（`+page.svelte` 自体は E2E 以外でテスト
 * しにくいため、整形ロジックだけをここへ抜き出す構成 - 他の `*Format.ts`
 * が無いのでこのファイルが最初の例）。
 */

/** 1024 進数の単位表。`formatBytes` はこの並びを大きい方から試す。 */
const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

/**
 * バイト数を人間可読な文字列へ整形する（例: `1536` → `"1.5 KB"`、
 * `0` → `"0 B"`）。1024 進数（KiB/MiB…の値だが表記は慣用的に
 * KB/MB/GB/TB とする - 他の管理 UI 画面のバイト表示に合わせる）。
 *
 * - `B` 単位は整数のまま表示する（小数点は付けない）。
 * - `KB` 以上は小数点1桁に丸める。
 * - 負数・`NaN`・`Infinity` は防御的に `"0 B"` として扱う（sysinfo が
 *   返す値は本来常に非負の有限数だが、表示関数が壊れた入力で例外を
 *   投げないようにする）。
 */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';

	let value = bytes;
	let unitIndex = 0;
	while (value >= 1024 && unitIndex < BYTE_UNITS.length - 1) {
		value /= 1024;
		unitIndex += 1;
	}

	const formatted = unitIndex === 0 ? String(Math.round(value)) : value.toFixed(1);
	return `${formatted} ${BYTE_UNITS[unitIndex]}`;
}

/**
 * CPU 使用率（%、`sysinfo::Process::cpu_usage` 由来 - 論理コア1個=100%
 * 換算）を小数点1桁の文字列へ整形する（例: `12.34` → `"12.3%"`）。
 * 負数・`NaN`・`Infinity` は `"0.0%"` として扱う（`formatBytes` と同じ
 * 防御的な扱い - プロセス起動直後最初のサンプルは `0` になりうるので、
 * それをそのまま表示できる必要がある）。
 */
export function formatPercent(percent: number): string {
	if (!Number.isFinite(percent) || percent < 0) return '0.0%';
	return `${percent.toFixed(1)}%`;
}
