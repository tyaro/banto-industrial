/**
 * `tagDeleteImpact.ts` に対するユニットテスト（`formDirty.test.ts`/
 * `tagCsv.test.ts` と同じスタイル、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	buildExternalName,
	expressionReferencesExternalName,
	extractTagRefTokens,
	findReferencingComputedTags,
	formatDeleteConfirmMessage,
	isExpressionRepresentableName,
	type ReferencingTag
} from './tagDeleteImpact';
import type { CollectionGroup, PlcConnection, Tag } from './tagRegistryAdmin';

// --- テスト用フィクスチャ ----------------------------------------------------

function makeConnection(overrides: Partial<PlcConnection>): PlcConnection {
	return {
		id: 1,
		name: 'line1',
		protocol: 'modbus-tcp',
		host: '127.0.0.1',
		port: 502,
		unitId: 1,
		enabled: true,
		simulation: true,
		wordOrder: 'low_high',
		database: null,
		username: null,
		passwordSet: false,
		...overrides
	};
}

function makeGroup(overrides: Partial<CollectionGroup>): CollectionGroup {
	return {
		id: 1,
		name: 'fast',
		plcConnectionId: 1,
		periodMs: 1000,
		enabled: true,
		defaultWritable: true,
		querySql: null,
		...overrides
	};
}

function makeTag(overrides: Partial<Tag>): Tag {
	return {
		id: 1,
		name: 'temp01',
		collectionGroupId: 1,
		address: '40001',
		dataType: 'f32',
		stringLength: null,
		stringEncoding: 'utf8',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
		thresholdH: null,
		thresholdHh: null,
		thresholdL: null,
		thresholdLl: null,
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 1,
		...overrides
	};
}

// --- buildExternalName -------------------------------------------------------

describe('buildExternalName', () => {
	it('接続名.グループ名.タグ名 を組み立てる', () => {
		expect(buildExternalName('line1', 'fast', 'temp01')).toBe('line1.fast.temp01');
	});
});

// --- extractTagRefTokens / expressionReferencesExternalName -----------------

describe('extractTagRefTokens', () => {
	it('単純な参照を1件抽出する', () => {
		expect(extractTagRefTokens('line1.fast.temp01')).toEqual(['line1.fast.temp01']);
	});

	it('四則演算に混じった複数の参照を抽出する', () => {
		expect(extractTagRefTokens('(line1.fast.a + line1.fast.b) / 2')).toEqual([
			'line1.fast.a',
			'line1.fast.b'
		]);
	});

	it('ハイフンを含む識別子セグメントも抽出する', () => {
		expect(extractTagRefTokens('line-1.grp_a.tag-name_1')).toEqual(['line-1.grp_a.tag-name_1']);
	});

	it('参照が無ければ空配列', () => {
		expect(extractTagRefTokens('1 + 2 * 3')).toEqual([]);
	});

	it('より長い識別子の一部として誤マッチしない（a.b.c2 は a.b.c と別トークン）', () => {
		expect(extractTagRefTokens('a.b.c2')).toEqual(['a.b.c2']);
		expect(expressionReferencesExternalName('a.b.c2', 'a.b.c')).toBe(false);
	});

	it('より長いドット連結の一部として誤マッチしない（4セグメントは3セグメント部分一致にしない）', () => {
		// banto-expr のタグ参照は必ず3セグメント（構文エラーになる式だが、
		// クライアント側の検出でも a.b.c を誤って参照ありと報告しない）。
		expect(expressionReferencesExternalName('a.b.c.d', 'a.b.c')).toBe(false);
	});

	it('前に別の識別子が連結していると誤マッチしない（xa.b.c は a.b.c と別トークン）', () => {
		expect(expressionReferencesExternalName('xa.b.c', 'a.b.c')).toBe(false);
	});

	/**
	 * #379 レビュー対応: `-` は減算演算子にも識別子の一部にもなる。期待値は
	 * 実 lexer に対して `crates/banto-expr/tests/compile.rs` の
	 * `tag_ref_after_minus_operator_is_a_separate_reference` /
	 * `hyphen_between_identifiers_is_absorbed_into_the_reference` で固定して
	 * あり、ここはその写し。
	 */
	it('演算子としての `-` の直後の参照を見落とさない（#379、正は banto-expr のテスト）', () => {
		expect(extractTagRefTokens('1-line1.fast.tag')).toEqual(['line1.fast.tag']);
		expect(extractTagRefTokens('-line1.fast.tag')).toEqual(['line1.fast.tag']);
		expect(extractTagRefTokens('(a.b.c)-line1.fast.tag')).toEqual(['a.b.c', 'line1.fast.tag']);
		expect(extractTagRefTokens('a.b.c - d.e.f')).toEqual(['a.b.c', 'd.e.f']);
		expect(expressionReferencesExternalName('1-line1.fast.tag', 'line1.fast.tag')).toBe(true);
	});

	it('吸収されないハイフン連続の前後どちらの参照も落とさない（#379、正は banto-expr のテスト）', () => {
		// 連続ハイフンは1つも識別子へ吸収されないので、前後とも独立した参照。
		// 旧実装は後読みに `IDENT_SEGMENT`（`-` を含む）を使っていたため
		// `c--` に一致し、前側の `a.b.c` を落としていた。
		expect(extractTagRefTokens('a.b.c--line1.fast.tag')).toEqual(['a.b.c', 'line1.fast.tag']);
		expect(extractTagRefTokens('a.b.c---line1.fast.tag')).toEqual(['a.b.c', 'line1.fast.tag']);
		expect(extractTagRefTokens('a.b.c-d--line1.fast.tag')).toEqual(['a.b.c-d', 'line1.fast.tag']);
		expect(extractTagRefTokens('line-1.grp.tag--other.g.t')).toEqual([
			'line-1.grp.tag',
			'other.g.t'
		]);
		// `a.b.c-1` は1トークン、`a.b.c--1` は `a.b.c` と減算×2。
		expect(extractTagRefTokens('a.b.c--1')).toEqual(['a.b.c']);
		expect(expressionReferencesExternalName('a.b.c--line1.fast.tag', 'a.b.c')).toBe(true);
	});

	it('識別子へ吸収された `-` の直後は別トークンにしない（#379、同上）', () => {
		// lexer は `a-line1` を1つの識別子として最長一致で吸収するので、
		// 参照は `a-line1.fast.tag` であって `line1.fast.tag` ではない。
		expect(extractTagRefTokens('a-line1.fast.tag')).toEqual(['a-line1.fast.tag']);
		expect(extractTagRefTokens('x1-line1.fast.tag')).toEqual(['x1-line1.fast.tag']);
		expect(extractTagRefTokens('a.b.c-1')).toEqual(['a.b.c-1']);
		expect(expressionReferencesExternalName('a-line1.fast.tag', 'line1.fast.tag')).toBe(false);
	});

	/**
	 * #379 レビュー対応: lexer は空白・タブ・改行を捨てるので、`.` の周りに
	 * 空白があっても有効な3セグメント参照。期待値は
	 * `crates/banto-expr/tests/compile.rs` の
	 * `whitespace_around_dots_is_allowed_in_a_tag_reference` で実 lexer に
	 * 対して固定してあり、`referenced_tags()` と同じ canonical 形を返す。
	 */
	it('`.` の周りに空白がある参照も拾い、空白を除いた形で返す（#379、正は banto-expr のテスト）', () => {
		expect(extractTagRefTokens('conn . group . tag + 1')).toEqual(['conn.group.tag']);
		expect(extractTagRefTokens('conn .\n group . tag')).toEqual(['conn.group.tag']);
		expect(extractTagRefTokens('conn.\tgroup .tag')).toEqual(['conn.group.tag']);
		// canonical 形で返すので、完全外部名との突き合わせもそのまま通る。
		expect(expressionReferencesExternalName('conn . group . tag * 2', 'conn.group.tag')).toBe(true);
		// CR+LF も lexer が読み飛ばす4種のうち。
		expect(extractTagRefTokens('conn\r\n.group.tag')).toEqual(['conn.group.tag']);
	});

	/**
	 * #379 レビュー対応: lexer が読み飛ばすのは 空白・タブ・LF・CR の4種だけ
	 * （`crates/banto-expr/src/lexer.rs:89`）。垂直タブ・フォームフィード・
	 * 全角空白を挟んだ式はコンパイルできない（compile.rs の
	 * `only_space_tab_lf_cr_are_skipped_as_whitespace`）ので、そこから参照を
	 * 拾ってはいけない - 正規表現の `\s` のままだと拾ってしまっていた。
	 */
	it('lexer が空白として扱わない文字を挟んだものは参照として拾わない（#379）', () => {
		expect(extractTagRefTokens('conn.group.tag')).toEqual([]);
		expect(extractTagRefTokens('conn.group.tag')).toEqual([]);
		expect(extractTagRefTokens('conn　.group.tag')).toEqual([]);
		expect(expressionReferencesExternalName('conn　.group.tag', 'conn.group.tag')).toBe(false);
	});
});

// --- isExpressionRepresentableName -------------------------------------------

describe('isExpressionRepresentableName', () => {
	/**
	 * #379 レビュー対応: parser は `true`/`false` を真偽値リテラルとして先に
	 * 解釈するので（`crates/banto-expr/src/parser.rs:313-318`）、第1セグメントが
	 * それらの完全名は式に書けない。正は compile.rs の
	 * `true_and_false_as_the_first_segment_are_not_tag_references`。
	 */
	it('第1セグメントが `true`/`false` の名前は式で表せない（#379）', () => {
		expect(isExpressionRepresentableName('true.grp.tag')).toBe(false);
		expect(isExpressionRepresentableName('false.grp.tag')).toBe(false);
		// 前方一致や第2・第3セグメントは通常の識別子。
		expect(isExpressionRepresentableName('trueish.grp.tag')).toBe(true);
		expect(isExpressionRepresentableName('grp.true.tag')).toBe(true);
		expect(isExpressionRepresentableName('grp.grp.false')).toBe(true);
		// 関数名は予約語ではない（`(` が続くときだけ関数呼び出し）。
		expect(isExpressionRepresentableName('if.grp.tag')).toBe(true);
		expect(isExpressionRepresentableName('bit.grp.tag')).toBe(true);
		expect(isExpressionRepresentableName('clamp.grp.tag')).toBe(true);
	});
});

describe('expressionReferencesExternalName', () => {
	it('完全一致する参照があれば true', () => {
		expect(expressionReferencesExternalName('line1.fast.temp01 * 2', 'line1.fast.temp01')).toBe(
			true
		);
	});

	it('参照が無ければ false', () => {
		expect(expressionReferencesExternalName('line1.fast.other * 2', 'line1.fast.temp01')).toBe(
			false
		);
	});
});

// --- findReferencingComputedTags ---------------------------------------------

describe('findReferencingComputedTags', () => {
	const plcConnection = makeConnection({ id: 1, name: 'line1' });
	const calcConnection = makeConnection({ id: 2, name: 'calc', protocol: 'virtual' });
	const plcGroup = makeGroup({ id: 1, name: 'fast', plcConnectionId: 1 });
	const calcGroup = makeGroup({ id: 2, name: 'calc-group', plcConnectionId: 2 });
	const groups = [plcGroup, calcGroup];
	const connections = [plcConnection, calcConnection];

	const targetTag = makeTag({ id: 10, name: 'temp01', collectionGroupId: 1, tagKind: 'plc' });
	const targetExternalName = buildExternalName('line1', 'fast', 'temp01');

	it('`1-x.y.z` のように演算子の `-` に隣接して参照している computed タグも検出する（#379）', () => {
		// 旧実装は「直前が `-`」を一律に除外していたため、この形の参照元を
		// 見落としていた（削除しても壊れないと誤って案内していた）。
		const expression = `1-${targetExternalName}`;
		const computedTag = makeTag({
			id: 21,
			name: 'inv',
			collectionGroupId: 2,
			tagKind: 'computed',
			expression,
			address: ''
		});

		const result = findReferencingComputedTags(
			targetTag.id,
			targetExternalName,
			[targetTag, computedTag],
			groups,
			connections
		);

		expect(result).toEqual<ReferencingTag[]>([
			{ id: 21, name: 'inv', externalName: 'calc.calc-group.inv', expression }
		]);
	});

	it('式が削除対象を参照する computed タグを見つける', () => {
		const expression = `(${targetExternalName} + line1.fast.temp02) / 2`;
		const computedTag = makeTag({
			id: 20,
			name: 'avg',
			collectionGroupId: 2,
			tagKind: 'computed',
			expression,
			address: ''
		});
		const tags = [targetTag, computedTag];

		const result = findReferencingComputedTags(
			targetTag.id,
			targetExternalName,
			tags,
			groups,
			connections
		);

		expect(result).toEqual<ReferencingTag[]>([
			{ id: 20, name: 'avg', externalName: 'calc.calc-group.avg', expression }
		]);
	});

	it('plc タグは computed でなくても除外する', () => {
		const otherPlcTag = makeTag({
			id: 30,
			name: 'other',
			collectionGroupId: 1,
			tagKind: 'plc',
			// plc タグに expression は無いが、念のため（送られてこない想定）。
			expression: targetExternalName
		});
		const tags = [targetTag, otherPlcTag];

		expect(
			findReferencingComputedTags(targetTag.id, targetExternalName, tags, groups, connections)
		).toEqual([]);
	});

	it('参照していない computed タグは含めない', () => {
		const unrelated = makeTag({
			id: 40,
			name: 'other-calc',
			collectionGroupId: 2,
			tagKind: 'computed',
			expression: 'line1.fast.temp02 * 2',
			address: ''
		});
		const tags = [targetTag, unrelated];

		expect(
			findReferencingComputedTags(targetTag.id, targetExternalName, tags, groups, connections)
		).toEqual([]);
	});

	it('削除対象自身は除外する（自己参照）', () => {
		const selfComputed = makeTag({
			id: 10,
			name: 'temp01',
			collectionGroupId: 1,
			tagKind: 'computed',
			expression: targetExternalName,
			address: ''
		});

		expect(
			findReferencingComputedTags(
				selfComputed.id,
				targetExternalName,
				[selfComputed],
				groups,
				connections
			)
		).toEqual([]);
	});

	it('複数の computed タグが参照していれば全件返す', () => {
		const computed1 = makeTag({
			id: 21,
			name: 'avg',
			collectionGroupId: 2,
			tagKind: 'computed',
			expression: targetExternalName,
			address: ''
		});
		const computed2 = makeTag({
			id: 22,
			name: 'doubled',
			collectionGroupId: 2,
			tagKind: 'computed',
			expression: `${targetExternalName} * 2`,
			address: ''
		});
		const tags = [targetTag, computed1, computed2];

		const result = findReferencingComputedTags(
			targetTag.id,
			targetExternalName,
			tags,
			groups,
			connections
		);
		expect(result.map((r) => r.externalName)).toEqual([
			'calc.calc-group.avg',
			'calc.calc-group.doubled'
		]);
	});
});

// --- formatDeleteConfirmMessage ----------------------------------------------

describe('formatDeleteConfirmMessage', () => {
	it('参照が無い場合でも完全外部名を含む', () => {
		const message = formatDeleteConfirmMessage('line1.fast.temp01', []);
		expect(message).toContain('line1.fast.temp01 を削除しますか？');
		expect(message).not.toContain('参照');
	});

	it('参照がある場合は一覧と警告を含む', () => {
		const referencing: ReferencingTag[] = [
			{ id: 20, name: 'avg', externalName: 'calc.calc-group.avg', expression: 'line1.fast.temp01' }
		];
		const message = formatDeleteConfirmMessage('line1.fast.temp01', referencing);
		expect(message).toContain('line1.fast.temp01 を削除しますか？');
		expect(message).toContain('calc.calc-group.avg');
		expect(message).toContain('参照');
	});
});
