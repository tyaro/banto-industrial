import { describe, expect, it } from 'vitest';
import {
	blankSinkGroupForm,
	buildRecommendedDdl,
	formToSinkGroupInput,
	intervalMsLabel,
	isValidSinkTableName,
	sinkGroupToForm,
	validateSinkGroupForm
} from './sinkGroupForm';
import type { SinkGroup } from './sinkGroupsAdmin';

describe('buildRecommendedDdl', () => {
	// `apps/banto-hub-sink/src/sql.rs`の
	// `recommended_ddl_quotes_the_table_and_matches_the_design_schema`と
	// 同じ入力・同じ出力になることを確認する（このモジュールの doc comment
	// 「推奨 DDL の組み立て」節参照 - Hub の UI とサイドカーで1バイトも
	// 違わない文面にする）。
	it('matches the sidecar recommended_ddl output for a bare table name', () => {
		const ddl = buildRecommendedDdl('t');
		expect(ddl).toBe(
			'CREATE TABLE "t" (ts timestamptz NOT NULL, tag_id bigint NOT NULL, ' +
				'external_name text NOT NULL, value double precision, quality text NOT NULL)'
		);
	});

	it('matches the sidecar recommended_ddl output for a schema-qualified table name', () => {
		const ddl = buildRecommendedDdl('s.t');
		expect(ddl).toBe(
			'CREATE TABLE "s"."t" (ts timestamptz NOT NULL, tag_id bigint NOT NULL, ' +
				'external_name text NOT NULL, value double precision, quality text NOT NULL)'
		);
	});

	it('matches the sidecar test fixture public.tag_history', () => {
		const ddl = buildRecommendedDdl('public.tag_history');
		expect(ddl.startsWith('CREATE TABLE "public"."tag_history" (')).toBe(true);
		expect(ddl).toContain('ts timestamptz NOT NULL');
		expect(ddl).toContain('tag_id bigint NOT NULL');
		expect(ddl).toContain('external_name text NOT NULL');
		expect(ddl).toContain('value double precision');
		expect(ddl).toContain('quality text NOT NULL');
	});
});

describe('isValidSinkTableName', () => {
	it('accepts bare and schema-qualified identifiers', () => {
		for (const good of ['t', '_t', 'tag_history', 'public.tag_history', '_s._t9', 'T']) {
			expect(isValidSinkTableName(good), good).toBe(true);
		}
	});

	it('rejects quoted, empty, and malformed identifiers', () => {
		const tooLong = 'a'.repeat(64);
		for (const bad of [
			'',
			'.',
			'a.',
			'.a',
			'1tag',
			'a.b.c',
			'tag-history',
			'tag history',
			'tag"history',
			'tag;drop',
			'タグ',
			tooLong
		]) {
			expect(isValidSinkTableName(bad), bad).toBe(false);
		}
	});

	it('accepts exactly 63 characters (boundary)', () => {
		expect(isValidSinkTableName('a'.repeat(63))).toBe(true);
	});
});

describe('validateSinkGroupForm', () => {
	function validForm() {
		return {
			name: 'line1-log',
			dbConnectionId: '1',
			mode: 'interval' as const,
			intervalMs: '1000',
			tableName: 'public.tag_history',
			tagIds: [1]
		};
	}

	it('accepts a well-formed form', () => {
		expect(validateSinkGroupForm(validForm())).toEqual({});
	});

	it('rejects an empty name', () => {
		const errors = validateSinkGroupForm({ ...validForm(), name: '  ' });
		expect(errors.name).toBe('名前を入力してください');
	});

	it('rejects a name over 64 characters', () => {
		const errors = validateSinkGroupForm({ ...validForm(), name: 'a'.repeat(65) });
		expect(errors.name).toContain('64文字以内');
	});

	it('rejects an unknown mode', () => {
		// @ts-expect-error - deliberately invalid for the runtime check
		const errors = validateSinkGroupForm({ ...validForm(), mode: 'hourly' });
		expect(errors.mode).toContain('interval');
	});

	it('rejects intervalMs below the minimum', () => {
		const errors = validateSinkGroupForm({ ...validForm(), intervalMs: '50' });
		expect(errors.intervalMs).toContain('100');
	});

	it('rejects intervalMs above the maximum', () => {
		const errors = validateSinkGroupForm({ ...validForm(), intervalMs: '3600001' });
		expect(errors.intervalMs).toContain('3600000');
	});

	it('rejects a non-integer intervalMs', () => {
		const errors = validateSinkGroupForm({ ...validForm(), intervalMs: '100.5' });
		expect(errors.intervalMs).toBeDefined();
	});

	it('rejects a quoted table name', () => {
		const errors = validateSinkGroupForm({ ...validForm(), tableName: '"quoted table"' });
		expect(errors.tableName).toContain('引用符は使用できません');
	});

	it('rejects an unselected connection', () => {
		const errors = validateSinkGroupForm({ ...validForm(), dbConnectionId: '' });
		expect(errors.dbConnectionId).toBe('接続を選択してください');
	});

	it('rejects an empty tagIds array', () => {
		const errors = validateSinkGroupForm({ ...validForm(), tagIds: [] });
		expect(errors.tagIds).toBe('対象タグを1件以上指定してください');
	});
});

describe('form <-> wire conversion', () => {
	const group: SinkGroup = {
		id: 7,
		name: 'line1-log',
		dbConnectionId: 3,
		mode: 'on_change',
		intervalMs: 2000,
		tableName: 'reporting.tag_history',
		storeBad: true,
		enabled: false,
		tagIds: [10, 11]
	};

	it('sinkGroupToForm stringifies numeric fields for input binding', () => {
		const form = sinkGroupToForm(group);
		expect(form.dbConnectionId).toBe('3');
		expect(form.intervalMs).toBe('2000');
		expect(form.tagIds).toEqual([10, 11]);
		// タグ id 配列は複製である（元の配列を共有しない）こと。
		expect(form.tagIds).not.toBe(group.tagIds);
	});

	it('formToSinkGroupInput round-trips back to the wire shape', () => {
		const form = sinkGroupToForm(group);
		const input = formToSinkGroupInput(form);
		expect(input).toEqual({
			name: 'line1-log',
			dbConnectionId: 3,
			mode: 'on_change',
			intervalMs: 2000,
			tableName: 'reporting.tag_history',
			storeBad: true,
			enabled: false,
			tagIds: [10, 11]
		});
	});

	it('blankSinkGroupForm defaults to interval mode and empty selections', () => {
		const form = blankSinkGroupForm();
		expect(form.mode).toBe('interval');
		expect(form.tagIds).toEqual([]);
		expect(form.dbConnectionId).toBe('');
	});
});

describe('intervalMsLabel', () => {
	it('changes label for on_change mode', () => {
		expect(intervalMsLabel('interval')).toBe('発行間隔（ミリ秒）');
		expect(intervalMsLabel('on_change')).toBe('最短発行間隔（ミリ秒）');
	});
});
