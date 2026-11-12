import { describe, expect, it } from 'vitest';
import { dottedNameError } from './dottedName';

describe('dottedNameError', () => {
	it('作成で . を含む接続名・グループ名を断る（サーバーと同じ文言）', () => {
		expect(dottedNameError('Line.1', '接続名')).toBe(
			'接続名に「.」は使えません（外部名「接続.グループ.タグ」の区切りに使うため）'
		);
		expect(dottedNameError(' fast.1 ', 'グループ名')).toBe(
			'グループ名に「.」は使えません（外部名「接続.グループ.タグ」の区切りに使うため）'
		);
	});

	it('. を含まない名前は通す', () => {
		expect(dottedNameError('Line1', '接続名')).toBeNull();
		expect(dottedNameError('', 'グループ名')).toBeNull();
	});

	it('名前を変えない更新は既存の . を含む名前でも通し、. を含む名前への改名は断る', () => {
		expect(dottedNameError(' Old.Line ', '接続名', 'Old.Line')).toBeNull();
		expect(dottedNameError('New.Line', '接続名', 'Old.Line')).not.toBeNull();
		expect(dottedNameError('NewLine', '接続名', 'Old.Line')).toBeNull();
	});
});
