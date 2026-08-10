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
