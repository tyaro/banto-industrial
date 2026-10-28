/**
 * SQLite の `datetime('now')` が返す時刻文字列（`YYYY-MM-DD HH:MM:SS`、**UTC**、
 * タイムゾーン表記なし）を、他の画面（監視・状態）と同じ書式
 * （`toLocaleString('ja-JP')`、例 `2026/10/20 6:41:56`）のローカル時刻にする（#712）。
 *
 * - `new Date('2026-10-19 21:48:19')` は環境によりローカル時刻として読まれるので使わず、
 *   UTC として組み立てる。
 * - `Z`・オフセット付きの ISO 文字列はそのまま `Date` に渡す。
 * - 読めない文字列は元のまま返す（表示を空にしない）。
 */
const SQLITE_UTC = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?$/;

export function formatSqliteUtc(value: string): string {
	const m = SQLITE_UTC.exec(value);
	const date = m
		? new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]))
		: new Date(value);
	return Number.isNaN(date.getTime()) ? value : date.toLocaleString('ja-JP');
}
