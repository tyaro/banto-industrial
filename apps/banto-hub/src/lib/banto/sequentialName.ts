/**
 * T18-6b（TAG-UX-8、2026-08-27 オーナー指示「収集グループの作成／再設定を
 * Drawer に寄せる」）: T18-6a で `plcConnectionForm.ts::nextConnectionName`
 * として実装した「prefix+連番」プリフィルの採番ロジックを、収集グループの
 * `collectionGroupForm.ts::nextGroupName` と共有するために切り出した
 * 依存ゼロの純関数。ロジック自体は無改変の移設（`plcConnectionForm.ts` は
 * この関数を呼ぶ薄いラッパーになり、`plcConnectionForm.test.ts` は無改変で
 * 通る）。
 */

/**
 * `existingNames` のうち `^${prefix}(\d+)$` に厳密一致する名前だけを見て、
 * 使われていない**最小の正整数**を選び `${prefix}${n}` を返す（「次の連番」
 * ＝最大値+1 ではなく、歯抜けがあれば埋める - 例: `prefix1, prefix3` が
 * あれば `prefix2`）。`prefix` 部分の大文字小文字や前後の記号は区別しない
 * 緩い一致はしない（`prefix1-old` のような接尾辞付きは番号として扱わない）。
 */
export function nextSequentialName(existingNames: readonly string[], prefix: string): string {
	const escapedPrefix = prefix.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const pattern = new RegExp(`^${escapedPrefix}(\\d+)$`);
	const used = new Set<number>();
	for (const name of existingNames) {
		const match = pattern.exec(name);
		if (match) used.add(Number(match[1]));
	}
	let n = 1;
	while (used.has(n)) n++;
	return `${prefix}${n}`;
}
