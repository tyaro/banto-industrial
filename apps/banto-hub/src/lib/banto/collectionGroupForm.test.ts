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

describe('blankGroupForm / groupToForm / formToGroupInput', () => {
	it('blankGroupForm は渡された既定周期を文字列化した初期値を返す', () => {
		expect(blankGroupForm(100)).toEqual({
			name: '',
			plcConnectionId: '',
			periodMs: '100',
			enabled: true
		});
	});

	it('groupToForm は保存済みグループを文字列化したフォーム状態へ変換する', () => {
		const group: CollectionGroup = {
			id: 7,
			name: 'Group1',
			plcConnectionId: 3,
			periodMs: 5000,
			enabled: true
		};
		expect(groupToForm(group)).toEqual({
			name: 'Group1',
			plcConnectionId: '3',
			periodMs: '5000',
			enabled: true
		});
	});

	it('formToGroupInput は数値フィールドを number へ戻す（往復変換）', () => {
		const group: CollectionGroup = {
			id: 1,
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false
		};
		const form: CollectionGroupFormState = groupToForm(group);
		expect(formToGroupInput(form)).toEqual({
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false
		});
	});
});
