/**
 * #342 段階B: `expressionCompletion.ts`（式欄のセグメント補完の純関数）の
 * ユニットテスト。境界（先頭 / ドット直後 / 2セグメント目の途中 / 3セグメント
 * 完成後にさらにドット / 数値の途中 / 空文字）と、候補の前方一致・除外4種・
 * 関数の混在・大文字小文字の扱いを固定する。
 */
import { describe, expect, it } from 'vitest';
import {
	buildCompletionIndex,
	clampCompletionIndex,
	completionCandidates,
	completionContextAt,
	completionContextMatchesCaret,
	completionInsertion,
	shouldOpenCompletion,
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
			replaceTo: 0,
			tokenStart: 0
		});
	});

	it('識別子の途中は第1セグメント（prefix はその識別子）', () => {
		expect(contextAt('lin|')).toEqual({
			kind: 'segment1',
			prefix: 'lin',
			replaceFrom: 0,
			replaceTo: 3,
			tokenStart: 0
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
			tokenStart: 0,
			seg1: 'line1'
		});
		expect(contextAt('line1.fast.|')).toEqual({
			kind: 'segment3',
			prefix: '',
			replaceFrom: 11,
			replaceTo: 11,
			tokenStart: 0,
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
			tokenStart: 0,
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

	it('ハイフンを2つ以上含む識別子も途中で切らない（#380 レビュー対応2）', () => {
		// `IDENT_SEGMENT` = `[A-Za-z_][A-Za-z0-9_]*(?:-[A-Za-z0-9_]+)*` なので
		// `line-1-2` / `line-1-foo` は丸ごと1つの識別子。以前は2つ目の `-` の
		// 左ラン（`1`）が英字始まりでないとして走査が止まり、prefix が `2`/`foo`
		// だけになっていた（確定すると末尾だけ置換して式を壊す）。
		expect(contextAt('line-1-2|')).toMatchObject({ kind: 'segment1', prefix: 'line-1-2' });
		expect(contextAt('line-1-foo|')).toMatchObject({ kind: 'segment1', prefix: 'line-1-foo' });
		// セグメントを跨いでも同じ。
		expect(contextAt('line-1-2.grp-a-b|')).toMatchObject({
			kind: 'segment2',
			prefix: 'grp-a-b',
			seg1: 'line-1-2'
		});
		// `1-line1` の `-` は左が数値なので演算子 - 補完対象は `line1` だけ。
		expect(contextAt('1-line1|')).toMatchObject({ kind: 'segment1', prefix: 'line1' });
		// `a--b` は lexer でも識別子にならない（`-` の右隣が継続文字でない）ので
		// 補完対象は `b` だけ。
		expect(contextAt('a--b|')).toMatchObject({ kind: 'segment1', prefix: 'b' });
		// 打鍵途中の末尾ハイフンは prefix に含める（次に継続文字を打つところ）。
		expect(contextAt('line-|')).toMatchObject({ kind: 'segment1', prefix: 'line-' });
	});

	it('数値の後ろの `-` は跨がない（判定順を lexer に合わせる。#380 レビュー対応2）', () => {
		// lexer は `1-a-b` を `1` `-` `a-b` と切る（`1` は ident-start でないので
		// 直後の `-` は演算子、`a` から始まる識別子が `-b` を吸収する）。以前は
		// 先に `-` を跨いで index 0 の `1` まで到達して false を返していたため、
		// 文脈が `b` だけになり、確定すると末尾の `b` だけを置換して式を壊した。
		expect(contextAt('1-a-b|')).toMatchObject({ kind: 'segment1', prefix: 'a-b' });
		expect(contextAt('1-a|')).toMatchObject({ kind: 'segment1', prefix: 'a' });
		// 既存の期待値が変わっていないこと。
		expect(contextAt('line-1-2|')).toMatchObject({ kind: 'segment1', prefix: 'line-1-2' });
		expect(contextAt('a--b|')).toMatchObject({ kind: 'segment1', prefix: 'b' });
		expect(contextAt('1-line1|')).toMatchObject({ kind: 'segment1', prefix: 'line1' });
	});

	it('空白を跨がない（`conn . group` は lexer 上は有効だが補完しない）', () => {
		expect(contextAt('line1 .|')).toBeNull();
		expect(contextAt('line1. |')).toMatchObject({ kind: 'segment1', prefix: '' });
	});

	it('トークンを終わらせる文字の直後は「prefix が空の segment1」になる（#380 レビュー対応1）', () => {
		// `line1.` の後に空白や先頭ハイフンを打つと、その参照はもう続かない。
		// `shouldOpenCompletion` はこの形を「区切り文字を打った直後」とみなして
		// 開いたままにしない（下の describe 参照）。
		expect(contextAt('line1. |')).toEqual({
			kind: 'segment1',
			prefix: '',
			replaceFrom: 7,
			replaceTo: 7,
			tokenStart: 7
		});
		expect(contextAt('line1.-|')).toMatchObject({ kind: 'segment1', prefix: '' });
	});

	it('キャレットより後ろは置換範囲に含めない', () => {
		const ctx = completionContextAt('line1abc', 5);
		expect(ctx).toEqual({
			kind: 'segment1',
			prefix: 'line1',
			replaceFrom: 0,
			replaceTo: 5,
			tokenStart: 0
		});
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
		// #380 レビュー対応1: 関数候補は `signature`（detail）だけでなく
		// `description`（API が返す1行説明）も持つ。
		expect(candidates[2]).toEqual({
			label: 'min',
			kind: 'function',
			detail: 'min(値1, 値2)',
			description: '小さい方',
			canonicalPrefix: null
		});
		// 接続・グループ・タグ候補は説明を持たない（ポップアップは2行目を描かない）。
		expect(candidates[0].description).toBeNull();
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

	it('親セグメントの引き当ても大文字小文字を無視する（#380 レビュー対応1）', () => {
		// 1段目が `LINE1` で出るのに2段目で消える、を防ぐ。
		const groupCtx = completionContextAt('LINE1.', 6)!;
		const groups = completionCandidates(groupCtx, index, FUNCTIONS, noBlocks);
		expect(labels(groups)).toEqual(['fast']);
		// 確定時に先行セグメントごと正式名へ直す（式言語は大文字小文字を区別する）。
		expect(groups[0].canonicalPrefix).toBe('line1.');

		const tagCtx = completionContextAt('LINE1.FAST.', 11)!;
		const tags = completionCandidates(tagCtx, index, FUNCTIONS, noBlocks);
		expect(labels(tags)).toEqual(['temp', 'temp2', 'label']);
		expect(tags[0].canonicalPrefix).toBe('line1.fast.');
	});

	it('大文字小文字違いの同名が併存しても、完全一致を優先して引く（#380 レビュー9回目）', () => {
		// レジストリの UNIQUE は完全一致なので `line1` と `LINE1` は併存しうる。
		const both = buildCompletionIndex(
			[
				{ id: 1, name: 'line1' },
				{ id: 2, name: 'LINE1' }
			],
			[
				{ id: 10, name: 'lower', plcConnectionId: 1 },
				{ id: 11, name: 'upper', plcConnectionId: 2 },
				// 3段目の曖昧さを作るため、両方の接続に大文字小文字違いの同名グループを置く。
				{ id: 12, name: 'grp', plcConnectionId: 1 },
				{ id: 13, name: 'GRP', plcConnectionId: 2 }
			],
			[
				{ id: 100, name: 'a', collectionGroupId: 10, dataType: 'f32', unit: null },
				{ id: 101, name: 'b', collectionGroupId: 11, dataType: 'f32', unit: null },
				{ id: 102, name: 'x', collectionGroupId: 12, dataType: 'f32', unit: null },
				{ id: 103, name: 'y', collectionGroupId: 13, dataType: 'f32', unit: null }
			]
		);
		// 完全一致があるときは必ずそれ（別の接続のグループを出さない）。
		expect(
			labels(completionCandidates(completionContextAt('line1.', 6)!, both, [], noBlocks))
		).toEqual(['lower', 'grp']);
		expect(
			labels(completionCandidates(completionContextAt('LINE1.', 6)!, both, [], noBlocks))
		).toEqual(['upper', 'GRP']);
		// どちらとも完全一致しない綴りは曖昧なので解決しない（候補を出さない）。
		expect(completionCandidates(completionContextAt('Line1.', 6)!, both, [], noBlocks)).toEqual([]);

		// 3段目は「接続.グループ」をまとめて引く。完全一致があればそれ。
		expect(
			labels(completionCandidates(completionContextAt('LINE1.GRP.', 10)!, both, [], noBlocks))
		).toEqual(['y']);
		expect(
			labels(completionCandidates(completionContextAt('line1.grp.', 10)!, both, [], noBlocks))
		).toEqual(['x']);
		// `line1.GRP` は小文字化すると2つ（`line1.grp` / `LINE1.GRP`）に当たるので
		// 解決しない（どちらの配下か決められないまま別物を挿さない）。
		expect(
			completionCandidates(completionContextAt('line1.GRP.', 10)!, both, [], noBlocks)
		).toEqual([]);
	});

	it('完全一致が無くても候補が1つならフォールバックで解決する', () => {
		// `sampleIndex()` の接続は `line1` だけなので `LINE1.` は一意に解決できる。
		const candidates = completionCandidates(
			completionContextAt('LINE1.', 6)!,
			index,
			FUNCTIONS,
			noBlocks
		);
		expect(labels(candidates)).toEqual(['fast']);
		expect(candidates[0].canonicalPrefix).toBe('line1.');
	});

	it('綴りが正式名と同じなら canonicalPrefix は null（置換範囲を広げない）', () => {
		const groupCtx = completionContextAt('line1.', 6)!;
		expect(
			completionCandidates(groupCtx, index, FUNCTIONS, noBlocks)[0].canonicalPrefix
		).toBeNull();
		const tagCtx = completionContextAt('line1.fast.', 11)!;
		expect(completionCandidates(tagCtx, index, FUNCTIONS, noBlocks)[0].canonicalPrefix).toBeNull();
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
		expect(candidates[0]).toEqual({
			label: 'temp',
			kind: 'tag',
			detail: 'f32・℃',
			description: null,
			canonicalPrefix: null
		});
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

describe('shouldOpenCompletion', () => {
	const at = (text: string) => completionContextAt(text, text.length)!;

	it('自動トリガーはドット直後と2文字以上の前方一致', () => {
		expect(shouldOpenCompletion(at('line1.'))).toBe(true);
		expect(shouldOpenCompletion(at('line1.fast.'))).toBe(true);
		expect(shouldOpenCompletion(at('li'))).toBe(true);
		// 1文字だけ・何も打っていないときは自動では開かない。
		expect(shouldOpenCompletion(at('l'))).toBe(false);
		expect(shouldOpenCompletion(at(''))).toBe(false);
	});

	it('force は無条件に開く（Ctrl+Space / Ctrl+.）', () => {
		expect(shouldOpenCompletion(at(''), { force: true })).toBe(true);
		expect(shouldOpenCompletion(at('1 + '), { force: true })).toBe(true);
	});

	it('開いている間は、いまのトークンが続いていれば開いたまま', () => {
		// 1文字だけに減らしても絞り込みを続けられる。
		expect(shouldOpenCompletion(at('l'), { alreadyOpen: true })).toBe(true);
		expect(shouldOpenCompletion(at('line1.'), { alreadyOpen: true })).toBe(true);
		expect(shouldOpenCompletion(at('line1.f'), { alreadyOpen: true })).toBe(true);
	});

	it('トークンを終わらせる区切り文字を打つと、開いていても閉じる（#380 レビュー対応1）', () => {
		// 以前は `alreadyOpen` だけで無条件に短絡していたため、ここで
		// 「prefix が空の segment1」＝無関係な接続候補へ切り替わっていた。
		expect(shouldOpenCompletion(at('line1. '), { alreadyOpen: true })).toBe(false);
		expect(shouldOpenCompletion(at('line1.-'), { alreadyOpen: true })).toBe(false);
		expect(shouldOpenCompletion(at('li + '), { alreadyOpen: true })).toBe(false);
		expect(shouldOpenCompletion(at('(min('), { alreadyOpen: true })).toBe(false);
	});
});

describe('completionContextMatchesCaret', () => {
	const context = completionContextAt('line1.fast.te', 13)!;

	it('キャレットが replaceTo に潰れていれば一致', () => {
		expect(completionContextMatchesCaret(context, 13, 13)).toBe(true);
	});

	it('キャレットが動いていたら一致しない（確定して式を壊さない）', () => {
		// `PageUp`/`Ctrl+A`/マウスクリックなど `input` を伴わない移動を想定。
		expect(completionContextMatchesCaret(context, 0, 0)).toBe(false);
		expect(completionContextMatchesCaret(context, 5, 5)).toBe(false);
	});

	it('選択範囲があるときも一致しない', () => {
		expect(completionContextMatchesCaret(context, 0, 13)).toBe(false);
		expect(completionContextMatchesCaret(context, 13, 20)).toBe(false);
	});
});

describe('clampCompletionIndex', () => {
	it('候補が減ったら末尾へ丸める（#380 レビュー対応B）', () => {
		// 6件で5番目を選んでいたところへ2件へ減った、という状況。
		expect(clampCompletionIndex(5, 2)).toBe(1);
		expect(clampCompletionIndex(1, 2)).toBe(1);
		expect(clampCompletionIndex(0, 2)).toBe(0);
	});

	it('負の添字・0件でも 0 に落ちる', () => {
		expect(clampCompletionIndex(-1, 3)).toBe(0);
		expect(clampCompletionIndex(3, 0)).toBe(0);
	});
});

describe('completionInsertion', () => {
	it('接続・グループは名前＋ドットを入れて次の階層を開く', () => {
		expect(
			completionInsertion({
				label: 'line1',
				kind: 'connection',
				detail: null,
				description: null,
				canonicalPrefix: null
			})
		).toEqual({ text: 'line1.', reopen: true });
		expect(
			completionInsertion({
				label: 'fast',
				kind: 'group',
				detail: null,
				description: null,
				canonicalPrefix: null
			})
		).toEqual({ text: 'fast.', reopen: true });
	});

	it('タグは名前だけ、関数は `name(` を入れて閉じる', () => {
		expect(
			completionInsertion({
				label: 'temp',
				kind: 'tag',
				detail: 'f32',
				description: null,
				canonicalPrefix: null
			})
		).toEqual({ text: 'temp', reopen: false });
		expect(
			completionInsertion({
				label: 'min',
				kind: 'function',
				detail: 'min(a, b)',
				description: '小さい方',
				canonicalPrefix: null
			})
		).toEqual({ text: 'min(', reopen: false });
	});
});
