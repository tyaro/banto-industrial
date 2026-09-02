/**
 * `writableDefault.ts`（T19 S1-b UX-34）に対するユニットテスト。
 * `tagFormLayout.test.ts`/`slmpDeviceTable.test.ts` と同じスタイル
 * （describe/it、依存ゼロの純関数を直接 import）。
 *
 * **2026-09-02 オーナー判断（S1-b0 分離）**: アドレス領域（Modbus
 * `1xxxx`/`3xxxx` 読み取り専用）による判定は本モジュールに実装しない -
 * `writableArea` という外部供給フラグの差し込み口だけを用意する
 * （`writableDefault.ts` の doc comment 参照）。したがってここでは
 * 「`writableArea` を渡さない（＝現状の呼び出し）」経路と「将来 S1-b0 が
 * `writableArea: false`/`true` を渡すようになったときの契約」の両方を
 * 固定する - 後者は今はどこからも実際に呼ばれないが、シグネチャが
 * 壊れていないことをテストで保証しておく。
 */
import { describe, expect, it } from 'vitest';
import { canDefaultWritable, writableDefaultBlockedReason } from './writableDefault';

describe('canDefaultWritable', () => {
	it('plc タグ + writableArea 未指定（現状の呼び出し、S1-b0 未配線）は true', () => {
		expect(canDefaultWritable('plc')).toBe(true);
		expect(canDefaultWritable('plc', undefined)).toBe(true);
	});

	it('computed タグは常に false', () => {
		expect(canDefaultWritable('computed')).toBe(false);
	});

	it('internal タグは常に false（design の適用条件が plc タグ限定のため）', () => {
		expect(canDefaultWritable('internal')).toBe(false);
	});

	it('S1-b0 契約: plc タグ + writableArea=false は false（将来の配線用、現状はどこからも渡されない）', () => {
		expect(canDefaultWritable('plc', false)).toBe(false);
	});

	it('S1-b0 契約: plc タグ + writableArea=true は true', () => {
		expect(canDefaultWritable('plc', true)).toBe(true);
	});
});

describe('writableDefaultBlockedReason', () => {
	it('computed タグは理由を返す', () => {
		expect(writableDefaultBlockedReason('computed')).toMatch(/computed タグ/);
	});

	it('internal タグは理由を返さない（禁止ではなく単に適用対象外なため）', () => {
		expect(writableDefaultBlockedReason('internal')).toBeNull();
	});

	it('plc タグ + writableArea 未指定（現状の呼び出し）は理由なし（null）', () => {
		expect(writableDefaultBlockedReason('plc')).toBeNull();
		expect(writableDefaultBlockedReason('plc', undefined)).toBeNull();
	});

	it('S1-b0 契約: plc タグ + writableArea=false は理由を返す（将来の配線用）', () => {
		expect(writableDefaultBlockedReason('plc', false)).toMatch(/読み取り専用/);
	});

	it('S1-b0 契約: plc タグ + writableArea=true は理由なし', () => {
		expect(writableDefaultBlockedReason('plc', true)).toBeNull();
	});
});
