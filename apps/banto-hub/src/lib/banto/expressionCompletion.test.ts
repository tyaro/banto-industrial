/**
 * #342 段階B: `expressionCompletion.ts`（式欄のセグメント補完の純関数）の
 * ユニットテスト。境界（先頭 / ドット直後 / 2セグメント目の途中 / 3セグメント
 * 完成後にさらにドット / 数値の途中 / 空文字）と、候補の前方一致・除外4種・
 * 関数の混在・大文字小文字の扱いを固定する。
 */
import { describe, expect, it } from 'vitest';
import {
	buildCompletionIndex,
	completionCandidates,
	completionContextAt,
	completionInsertion,
	type CompletionCandidate,
	type CompletionIndex
} from './expressionCompletion';
import type { ExpressionFunction } from './expressionCheck';

// --- completionContextAt -----------------------------------------------------

/** `text` の `|` の位置をキャレットとみなす簡易記法。 */
function contextAt(textWithCaret: string) {
	const caret = textWithCaret.indexOf('|');
	expect(caret, `テスト文字列に '|' がありません: ${textWithCaret}`).toBeGreaterThanOrEqual(0);
	return completionContextAt(textWithCaret.replace('|', ''), caret);
}

describe('completionContextAt', () => {
	it('空文字・行頭では第1セグメントを空の前方一致で補完する（Ctrl+Space で開ける）', () => {
		expect(completionContextAt('', 0)).toEqual({
			kind: 'segment1',
			prefix: '',
			replaceFrom: 0,
			replaceTo: 0
		});
	});

	it('識別子の途中は第1セグメント（prefix はその識別子）', () => {
		expect(contextAt('lin|')).toEqual({
			kind: 'segment1',
			prefix: 'lin',
			replaceFrom: 0,
			replaceTo: 3
		});
	});

	it('演算子・括弧の直後も第1セグメント（空白は跨がない）', () => {
		expect(contextAt('(a.b.c + li|')).toMatchObject({ kind: 'segment1', prefix: 'li' });
		expect(contextAt('1 + |')).toMatchObject({ kind: 'segment1', prefix: '' });
	});

	it('ドット直後は次のセグメントを空の前方一致で補完する', () => {
		expect(contextAt('line1.|')).toEqual({
			kind: 'segment2',
			prefix: '',
			replaceFrom: 6,
			replaceTo: 6,
			seg1: 'line1'
		});
		expect(contextAt('line1.fast.|')).toEqual({
			kind: 'segment3',
			prefix: '',
			replaceFrom: 11,
			replaceTo: 11,
			seg1: 'line1',
			seg2: 'fast'
		});
	});

	it('2セグメント目の途中では seg1 とその prefix を返す', () => {
		expect(contextAt('line1.fa|')).toEqual({
			kind: 'segment2',
			prefix: 'fa',
			replaceFrom: 6,
			replaceTo: 8,
			seg1: 'line1'
		});
	});

	it('3セグメント完成後にさらにドットを打つと補完しない（参照は3セグメントまで）', () => {
		expect(contextAt('line1.fast.tag.|')).toBeNull();
		expect(contextAt('line1.fast.tag.x|')).toBeNull();
	});

	it('数値の途中では補完しない', () => {
		expect(contextAt('12|')).toBeNull();
		expect(contextAt('1.5|')).toBeNull();
		expect(contextAt('1.|')).toBeNull();
	});

	it('先行セグメントが式に書けない名前なら補完しない', () => {
		// 第1セグメントの `true`/`false` は真偽値リテラルとして先に解釈される。
		expect(contextAt('true.|')).toBeNull();
		// 末尾ハイフン（lexer は `-` を識別子へ吸収しない形）。
		expect(contextAt('abc-.|')).toBeNull();
		// 第2・第3セグメントの `true` は普通の識別子なので通る。
		expect(contextAt('line1.true.|')).toMatchObject({ kind: 'segment3', seg2: 'true' });
	});

	it('識別子に吸収されるハイフンは prefix に含み、減算演算子の直後は含まない', () => {
		// lexer は「直前が識別子」のときだけ `-` を吸収する
		// （`tagDeleteImpact.ts::TAG_REF_PATTERN` の非対称な後読みと同じ規則）。
		expect(contextAt('line-1|')).toMatchObject({ kind: 'segment1', prefix: 'line-1' });
		// `1-x` の `-` は演算子なので、補完対象は `x` だけ。
		expect(contextAt('1-x|')).toMatchObject({ kind: 'segment1', prefix: 'x' });
	});

	it('空白を跨がない（`conn . group` は lexer 上は有効だが補完しない）', () => {
		expect(contextAt('line1 .|')).toBeNull();
		expect(contextAt('line1. |')).toMatchObject({ kind: 'segment1', prefix: '' });
	});

	it('キャレットより後ろは置換範囲に含めない', () => {
		const ctx = completionContextAt('line1abc', 5);
		expect(ctx).toEqual({ kind: 'segment1', prefix: 'line1', replaceFrom: 0, replaceTo: 5 });
	});
});

// --- buildCompletionIndex / completionCandidates -----------------------------

const FUNCTIONS: ExpressionFunction[] = [
	{ name: 'min', arity: 2, signature: 'min(値1, 値2)', description: '小さい方' },
	{ name: 'max', arity: 2, signature: 'max(値1, 値2)', description: '大きい方' },
	{ name: 'abs', arity: 1, signature: 'abs(値)', description: '絶対値' }
];

/**
 * 接続2・グループ2・タグ5 の小さなレジストリ。日本語名の接続/グループ/タグと
 * 文字列型タグを混ぜて、除外が効くことを見る。
 */
function sampleIndex(): CompletionIndex {
	return buildCompletionIndex(
		[
			{ id: 1, name: 'line1' },
			{ id: 2, name: 'calc' },
			{ id: 3, name: '製造ライン' }
		],
		[
			{ id: 10, name: 'fast', plcConnectionId: 1 },
			{ id: 11, name: '高速', plcConnectionId: 1 },
			{ id: 12, name: 'derived', plcConnectionId: 2 }
		],
		[
			{ id: 100, name: 'temp', collectionGroupId: 10, dataType: 'f32', unit: '℃' },
			{ id: 101, name: 'temp2', collectionGroupId: 10, dataType: 'i16', unit: null },
			{ id: 102, name: 'label', collectionGroupId: 10, dataType: 'string', unit: null },
			{ id: 103, name: '温度', collectionGroupId: 10, dataType: 'f32', unit: null },
			{ id: 104, name: 'avg', collectionGroupId: 12, dataType: 'f32', unit: null }
		]
	);
}

function labels(candidates: CompletionCandidate[]): string[] {
	return candidates.map((c) => c.label);
}

describe('buildCompletionIndex', () => {
	it('接続・グループ・タグをそれぞれ1パスで索引化する', () => {
		const index = sampleIndex();
		expect(index.connections).toEqual(['line1', 'calc', '製造ライン']);
		expect(index.groupsByConnection.get('line1')).toEqual(['fast', '高速']);
		expect(index.tagsByGroupPath.get('line1.fast')?.map((t) => t.name)).toEqual([
			'temp',
			'temp2',
			'label',
			'温度'
		]);
	});

	it('親が見つからないグループ・タグは索引に入らない', () => {
		const index = buildCompletionIndex(
			[],
			[{ id: 10, name: 'orphan', plcConnectionId: 999 }],
			[{ id: 100, name: 't', collectionGroupId: 10, dataType: 'f32', unit: null }]
		);
		expect(index.groupsByConnection.size).toBe(0);
		expect(index.tagsByGroupPath.size).toBe(0);
	});
});

describe('completionCandidates', () => {
	const index = sampleIndex();
	const noBlocks = new Map<number, string>();

	it('第1セグメントは接続候補のあとに関数候補を並べる', () => {
		const ctx = completionContextAt('', 0)!;
		const candidates = completionCandidates(ctx, index, FUNCTIONS, noBlocks);
		// 式で表せない接続名（`製造ライン`）は出さない。
		expect(labels(candidates)).toEqual(['line1', 'calc', 'min', 'max', 'abs']);
		expect(candidates[0].kind).toBe('connection');
		expect(candidates[2]).toEqual({ label: 'min', kind: 'function', detail: 'min(値1, 値2)' });
	});

	it('第1セグメントの前方一致は接続にも関数にも効く', () => {
		const ctx = completionContextAt('mi', 2)!;
		expect(labels(completionCandidates(ctx, index, FUNCTIONS, noBlocks))).toEqual(['min']);
	});

	it('前方一致は大文字小文字を無視するが、候補は登録名そのもの', () => {
		const ctx = completionContextAt('LIN', 3)!;
		expect(labels(completionCandidates(ctx, index, FUNCTIONS, noBlocks))).toEqual(['line1']);
	});

	it('第2セグメントは該当接続配下のグループだけ（式で表せない名前は出さない）', () => {
		const ctx = completionContextAt('line1.', 6)!;
		expect(labels(completionCandidates(ctx, index, FUNCTIONS, noBlocks))).toEqual(['fast']);
		// 関数は第1セグメントのときだけ混ぜる。
		expect(
			completionCandidates(ctx, index, FUNCTIONS, noBlocks).some((c) => c.kind === 'function')
		).toBe(false);
	});

	it('存在しない接続・グループの配下では候補が空になる', () => {
		expect(
			completionCandidates(completionContextAt('nope.', 5)!, index, FUNCTIONS, noBlocks)
		).toEqual([]);
		expect(
			completionCandidates(completionContextAt('line1.nope.', 11)!, index, FUNCTIONS, noBlocks)
		).toEqual([]);
	});

	it('第3セグメントはタグ候補（型・単位を detail に出す）', () => {
		const ctx = completionContextAt('line1.fast.', 11)!;
		const candidates = completionCandidates(ctx, index, FUNCTIONS, noBlocks);
		// `温度` は式で表せない名前なので出ない。`label`（string 型）は
		// 段階C の除外（`blockedInsertTargets`）を渡していないのでここでは残る。
		expect(labels(candidates)).toEqual(['temp', 'temp2', 'label']);
		expect(candidates[0]).toEqual({ label: 'temp', kind: 'tag', detail: 'f32・℃' });
		expect(candidates[1].detail).toBe('i16');
	});

	it('段階C の除外（自タグ・文字列型・循環など）に該当するタグは候補に出さない', () => {
		const ctx = completionContextAt('line1.fast.', 11)!;
		// `blockedInsertTargets` の戻り値そのまま（理由の文言は補完では使わず、
		// キーの有無だけを見る）。
		const blocked = new Map<number, string>([
			[102, '文字列型のタグは式から参照できません'],
			[101, '循環参照になるため挿入できません（このタグが編集中のタグを参照しています）']
		]);
		expect(labels(completionCandidates(ctx, index, FUNCTIONS, blocked))).toEqual(['temp']);
	});

	it('第3セグメントの前方一致', () => {
		const ctx = completionContextAt('line1.fast.temp', 15)!;
		expect(labels(completionCandidates(ctx, index, FUNCTIONS, noBlocks))).toEqual([
			'temp',
			'temp2'
		]);
	});

	it('関数表が空でも（取得に失敗しても）タグ候補は出る', () => {
		const ctx = completionContextAt('', 0)!;
		expect(labels(completionCandidates(ctx, index, [], noBlocks))).toEqual(['line1', 'calc']);
	});
});

describe('completionInsertion', () => {
	it('接続・グループは名前＋ドットを入れて次の階層を開く', () => {
		expect(completionInsertion({ label: 'line1', kind: 'connection', detail: null })).toEqual({
			text: 'line1.',
			reopen: true
		});
		expect(completionInsertion({ label: 'fast', kind: 'group', detail: null })).toEqual({
			text: 'fast.',
			reopen: true
		});
	});

	it('タグは名前だけ、関数は `name(` を入れて閉じる', () => {
		expect(completionInsertion({ label: 'temp', kind: 'tag', detail: 'f32' })).toEqual({
			text: 'temp',
			reopen: false
		});
		expect(completionInsertion({ label: 'min', kind: 'function', detail: 'min(a, b)' })).toEqual({
			text: 'min(',
			reopen: false
		});
	});
});
