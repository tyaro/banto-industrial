/**
 * `collectionGroupForm.ts`（T18-6b）に対するユニットテスト。
 * `plcConnectionForm.test.ts` と同じスタイル（describe/it、依存ゼロの純関数を
 * 直接 import）。`nextGroupName` は `plcConnectionForm.ts::nextConnectionName`
 * と共通の `sequentialName.ts::nextSequentialName` を使うため、prefix を
 * `'group'` に変えた同じケースを一通り確認する。
 */
import { describe, expect, it } from 'vitest';
import {
	blankGroupForm,
	formToGroupInput,
	groupToForm,
	nextGroupName,
	type CollectionGroupFormState
} from './collectionGroupForm';
import type { CollectionGroup } from './tagRegistryAdmin';

describe('nextGroupName', () => {
	it('既存名が無ければ group1 を返す', () => {
		expect(nextGroupName([])).toBe('group1');
	});

	it('実装指示の例と同型: group1, group3 があれば group2 を返す（歯抜けを埋める）', () => {
		expect(nextGroupName(['group1', 'group3'])).toBe('group2');
	});

	it('連続した既存名の直後（歯抜けなし）は最大値+1を返す', () => {
		expect(nextGroupName(['group1', 'group2', 'group3'])).toBe('group4');
	});

	it('無関係な名前（自由入力）は無視する', () => {
		expect(nextGroupName(['ライン1', 'group1'])).toBe('group2');
	});

	it('接尾辞付きの名前（group1-old 等）は番号として扱わない', () => {
		expect(nextGroupName(['group1-old', 'group1'])).toBe('group2');
	});

	it('prefix を明示指定できる', () => {
		expect(nextGroupName(['line1'], 'line')).toBe('line2');
	});
});

describe('nextGroupName（修正1: pendingNames — 実機で再現した不具合、2026-08-31 オーナー報告）', () => {
	it('実機再現ケース: 既存 group1/group2 に加え pending の group1 が3件あっても group3 を返す（既存の pending は衝突候補として1つに畳んでよい）', () => {
		// オーナーが実機で再現した状況そのもの: DB には group1/group2 が
		// あり、収集稼働中に3回作成した結果 pending キューには全部
		// name="group1" の未適用作成が3件積まれている。連番プリフィルは
		// これらも見た上で、まだ誰も使っていない group3 を提案すべき
		// （修正前は既存レコードだけを見て毎回 group1 を提案し、後から
		// 一括適用すると名前の一意制約で3件とも失敗していた）。
		expect(nextGroupName(['group1', 'group2'], 'group', ['group1', 'group1', 'group1'])).toBe(
			'group3'
		);
	});

	it('pendingNames が空なら既存レコードのみの場合と同じ結果になる（回帰確認）', () => {
		expect(nextGroupName(['group1', 'group3'], 'group', [])).toBe('group2');
		expect(nextGroupName(['group1', 'group3'])).toBe('group2');
	});

	it('pendingNames を省略しても既存の呼び出し（引数2つ）と同じ結果になる', () => {
		expect(nextGroupName(['group1'], 'group')).toBe('group2');
	});

	it('pendingNames にしか無い番号も歯抜け埋めの対象として除外する', () => {
		// 既存レコードには group1 しか無いが、group2 は pending（未適用の
		// 作成キュー）が既に占有している想定 - group3 を返すべき。
		expect(nextGroupName(['group1'], 'group', ['group2'])).toBe('group3');
	});

	it('pending 取得失敗相当（呼び出し側が pendingNames を渡さない）: 既存レコードだけで採番して続行する', () => {
		// pending の取得に失敗した場合、呼び出し側（CollectionGroupDrawer）は
		// pendingNames を渡さず（＝ 既定の []）続行する設計。この関数自体は
		// pendingNames の有無に関わらず常に既存レコードだけでの採番結果を
		// 下回らない（取得失敗時に採番自体が止まらないことの確認）。
		expect(nextGroupName(['group1', 'group2'])).toBe('group3');
	});
});

describe('blankGroupForm / groupToForm / formToGroupInput', () => {
	it('blankGroupForm は渡された既定周期を文字列化した初期値を返す（defaultWritable も既定 ON）', () => {
		expect(blankGroupForm(100)).toEqual({
			name: '',
			plcConnectionId: '',
			periodMs: '100',
			enabled: true,
			defaultWritable: true
		});
	});

	it('groupToForm は保存済みグループを文字列化したフォーム状態へ変換する（defaultWritable も引き継ぐ）', () => {
		const group: CollectionGroup = {
			id: 7,
			name: 'Group1',
			plcConnectionId: 3,
			periodMs: 5000,
			enabled: true,
			defaultWritable: false
		};
		expect(groupToForm(group)).toEqual({
			name: 'Group1',
			plcConnectionId: '3',
			periodMs: '5000',
			enabled: true,
			defaultWritable: false
		});
	});

	it('formToGroupInput は数値フィールドを number へ戻す（往復変換、defaultWritable も含む）', () => {
		const group: CollectionGroup = {
			id: 1,
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false,
			defaultWritable: true
		};
		const form: CollectionGroupFormState = groupToForm(group);
		expect(formToGroupInput(form)).toEqual({
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false,
			defaultWritable: true
		});
	});
});
