/**
 * `expressionInsert.ts`（#342 段階C「一覧から挿入」の除外判定）の
 * ユニットテスト。対象モジュールは `tagDeleteImpact.ts` しか import せず
 * （どちらも `$state` を含まない素の TypeScript）、`vi.mock` は不要。
 *
 * 実装指示の必須ケース: 自タグ / 文字列型 / 直接循環 / 間接循環（A→B→自）/
 * 循環しない computed / plc タグ。
 */
import { describe, expect, it } from 'vitest';
import {
	blockedInsertTargets,
	CYCLE_REFERENCE_REASON,
	extractTagRefs,
	insertionBlockReason,
	SELF_REFERENCE_REASON,
	STRING_REFERENCE_REASON,
	wouldCreateCycle,
	type InsertCandidateTag
} from './expressionInsert';

const CONN = 'line1';
const GROUP = 'fast';

function tag(
	id: number,
	name: string,
	overrides: Partial<InsertCandidateTag> = {}
): InsertCandidateTag {
	return {
		id,
		dataType: 'f32',
		tagKind: 'plc',
		expression: null,
		externalName: `${CONN}.${GROUP}.${name}`,
		...overrides
	};
}

/**
 * 依存関係:
 *   self(1, computed)      … 編集中のタグ
 *   plc(2, plc)            … 普通の PLC タグ（挿入可）
 *   str(3, plc/string)     … 文字列型（挿入不可）
 *   direct(4, computed)    … self を直接参照（循環）
 *   mid(5, computed)       … self を直接参照（間接循環の中継）
 *   indirect(6, computed)  … mid を参照 → self へ推移的に到達（循環）
 *   safe(7, computed)      … plc だけを参照（循環しない）
 */
function fixture(): InsertCandidateTag[] {
	const selfName = `${CONN}.${GROUP}.self`;
	const plcName = `${CONN}.${GROUP}.plc`;
	const midName = `${CONN}.${GROUP}.mid`;
	return [
		tag(1, 'self', { tagKind: 'computed', expression: `${plcName} * 2` }),
		tag(2, 'plc'),
		tag(3, 'str', { dataType: 'string' }),
		tag(4, 'direct', { tagKind: 'computed', expression: `${selfName} + 1` }),
		tag(5, 'mid', { tagKind: 'computed', expression: `${selfName} - 1` }),
		tag(6, 'indirect', { tagKind: 'computed', expression: `${midName} / 2` }),
		tag(7, 'safe', { tagKind: 'computed', expression: `${plcName} + 1` })
	];
}

describe('extractTagRefs', () => {
	it('3セグメントの参照だけを抽出する（`tagDeleteImpact` の正規表現を再利用）', () => {
		expect(extractTagRefs('line1.fast.a + line1.fast.b')).toEqual(['line1.fast.a', 'line1.fast.b']);
	});

	it('式が null / 空なら空配列', () => {
		expect(extractTagRefs(null)).toEqual([]);
		expect(extractTagRefs('')).toEqual([]);
	});
});

describe('wouldCreateCycle', () => {
	const tags = fixture();

	it('自タグ自身は循環扱い', () => {
		expect(wouldCreateCycle(tags, 1, 1)).toBe(true);
	});

	it('自タグを直接参照している computed は循環', () => {
		expect(wouldCreateCycle(tags, 1, 4)).toBe(true);
	});

	it('推移的に自タグへ到達する computed（A→B→自）は循環', () => {
		expect(wouldCreateCycle(tags, 1, 6)).toBe(true);
	});

	it('自タグへ到達しない computed は循環しない', () => {
		expect(wouldCreateCycle(tags, 1, 7)).toBe(false);
	});

	it('plc タグは式を持たないので循環しない', () => {
		expect(wouldCreateCycle(tags, 1, 2)).toBe(false);
	});

	it('新規作成（selfId = null）では循環しようがない', () => {
		expect(wouldCreateCycle(tags, null, 4)).toBe(false);
		expect(wouldCreateCycle(tags, null, 6)).toBe(false);
	});
});

describe('insertionBlockReason', () => {
	const tags = fixture();

	it('自タグは自己参照の理由を返す', () => {
		expect(insertionBlockReason(tags, 1, 1)).toBe(SELF_REFERENCE_REASON);
	});

	it('文字列型タグは文字列参照の理由を返す（新規作成でも同じ）', () => {
		expect(insertionBlockReason(tags, 1, 3)).toBe(STRING_REFERENCE_REASON);
		expect(insertionBlockReason(tags, null, 3)).toBe(STRING_REFERENCE_REASON);
	});

	it('直接循環・間接循環は循環の理由を返す', () => {
		expect(insertionBlockReason(tags, 1, 4)).toBe(CYCLE_REFERENCE_REASON);
		expect(insertionBlockReason(tags, 1, 6)).toBe(CYCLE_REFERENCE_REASON);
	});

	it('循環しない computed と plc タグはブロックしない', () => {
		expect(insertionBlockReason(tags, 1, 7)).toBeNull();
		expect(insertionBlockReason(tags, 1, 2)).toBeNull();
	});

	it('新規作成では循環になる computed もブロックしない（自タグがまだ無い）', () => {
		expect(insertionBlockReason(tags, null, 4)).toBeNull();
		expect(insertionBlockReason(tags, null, 6)).toBeNull();
	});

	it('一覧に無い id は判定対象外（null）', () => {
		expect(insertionBlockReason(tags, 1, 999)).toBeNull();
	});
});

describe('blockedInsertTargets', () => {
	it('編集中は 自タグ・文字列型・直接/間接循環 だけがブロック対象になる', () => {
		const blocked = blockedInsertTargets(fixture(), 1);
		expect([...blocked.keys()].sort((a, b) => a - b)).toEqual([1, 3, 4, 5, 6]);
		expect(blocked.get(1)).toBe(SELF_REFERENCE_REASON);
		expect(blocked.get(3)).toBe(STRING_REFERENCE_REASON);
		expect(blocked.get(5)).toBe(CYCLE_REFERENCE_REASON);
		expect(blocked.get(6)).toBe(CYCLE_REFERENCE_REASON);
	});

	it('新規作成（selfId = null）では文字列型だけがブロック対象', () => {
		const blocked = blockedInsertTargets(fixture(), null);
		expect([...blocked.keys()]).toEqual([3]);
	});

	it('`insertionBlockReason` を1件ずつ呼んだ結果と一致する（グラフ1回構築の最適化で結果が変わらない）', () => {
		const tags = fixture();
		const blocked = blockedInsertTargets(tags, 1);
		for (const t of tags) {
			expect(blocked.get(t.id) ?? null).toBe(insertionBlockReason(tags, 1, t.id));
		}
	});

	it('既存の式が循環している壊れたデータでも無限ループしない', () => {
		const a = `${CONN}.${GROUP}.a`;
		const b = `${CONN}.${GROUP}.b`;
		const tags = [
			tag(1, 'self', { tagKind: 'computed', expression: '1' }),
			tag(2, 'a', { tagKind: 'computed', expression: `${b} + 1` }),
			tag(3, 'b', { tagKind: 'computed', expression: `${a} + 1` })
		];
		expect(blockedInsertTargets(tags, 1).size).toBe(1); // 自タグのみ
	});
});
