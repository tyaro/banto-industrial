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
	validateQuerySql,
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
	it('blankGroupForm は渡された既定周期を文字列化した初期値を返す（defaultWritable も既定 ON、querySql は空文字列）', () => {
		expect(blankGroupForm(100)).toEqual({
			name: '',
			plcConnectionId: '',
			periodMs: '100',
			enabled: true,
			defaultWritable: true,
			querySql: ''
		});
	});

	it('groupToForm は保存済みグループを文字列化したフォーム状態へ変換する（defaultWritable も引き継ぐ、querySql は null→空文字列）', () => {
		const group: CollectionGroup = {
			id: 7,
			name: 'Group1',
			plcConnectionId: 3,
			periodMs: 5000,
			enabled: true,
			defaultWritable: false,
			querySql: null
		};
		expect(groupToForm(group)).toEqual({
			name: 'Group1',
			plcConnectionId: '3',
			periodMs: '5000',
			enabled: true,
			defaultWritable: false,
			querySql: ''
		});
	});

	it('groupToForm は querySql が設定されているグループでは値をそのまま引き継ぐ（postgres 配下）', () => {
		const group: CollectionGroup = {
			id: 8,
			name: 'PgGroup',
			plcConnectionId: 5,
			periodMs: 5000,
			enabled: true,
			defaultWritable: false,
			querySql: 'SELECT a, b FROM v1'
		};
		expect(groupToForm(group).querySql).toBe('SELECT a, b FROM v1');
	});

	it('formToGroupInput は数値フィールドを number へ戻す（往復変換、defaultWritable も含む、非 postgres では querySql を送らない）', () => {
		const group: CollectionGroup = {
			id: 1,
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false,
			defaultWritable: true,
			querySql: null
		};
		const form: CollectionGroupFormState = groupToForm(group);
		expect(formToGroupInput(form, false)).toEqual({
			name: 'X',
			plcConnectionId: 2,
			periodMs: 1000,
			enabled: false,
			defaultWritable: true,
			querySql: undefined
		});
	});

	it('formToGroupInput は postgres 配下かつ入力ありのとき trim 済み querySql を送る', () => {
		const form: CollectionGroupFormState = {
			...blankGroupForm(1000),
			plcConnectionId: '5',
			querySql: '  SELECT a FROM v1  '
		};
		expect(formToGroupInput(form, true).querySql).toBe('SELECT a FROM v1');
	});

	it('formToGroupInput は postgres 配下でも空白のみの querySql は送らない（undefined、サーバー側必須チェックへ委ねる）', () => {
		const form: CollectionGroupFormState = {
			...blankGroupForm(1000),
			plcConnectionId: '5',
			querySql: '   '
		};
		expect(formToGroupInput(form, true).querySql).toBeUndefined();
	});
});

describe('validateQuerySql（S3、crates/banto-tags/src/collection_group.rs::validate_query_sql のミラー）', () => {
	it('postgres 以外の接続配下では常にエラー無し', () => {
		expect(validateQuerySql({ querySql: '' }, false)).toEqual({});
		expect(validateQuerySql({ querySql: 'DELETE FROM x' }, false)).toEqual({});
	});

	it('postgres 配下で空欄（trim 後空）は必須エラー', () => {
		expect(validateQuerySql({ querySql: '   ' }, true)).toEqual({ querySql: '必須項目です' });
	});

	it('postgres 配下で SELECT/WITH 以外の先頭キーワードはエラー', () => {
		expect(validateQuerySql({ querySql: 'DELETE FROM x' }, true)).toEqual({
			querySql: 'SQL は SELECT または WITH で始まる必要があります'
		});
		expect(validateQuerySql({ querySql: 'update x set y=1' }, true)).toEqual({
			querySql: 'SQL は SELECT または WITH で始まる必要があります'
		});
	});

	it('postgres 配下で SELECT/WITH は大文字小文字を問わず受理される', () => {
		expect(validateQuerySql({ querySql: 'select a from v1' }, true)).toEqual({});
		expect(validateQuerySql({ querySql: 'With q as (select 1) select * from q' }, true)).toEqual(
			{}
		);
	});

	it('postgres 配下で ";" を含む文はエラー（単文のみ）', () => {
		expect(validateQuerySql({ querySql: 'SELECT 1; SELECT 2' }, true)).toEqual({
			querySql: "SQL は単文で指定してください（';' は使用できません）"
		});
	});

	it('postgres 配下で MAX_QUERY_SQL_LEN を超える文はエラー', () => {
		const sql = `SELECT ${'a'.repeat(8192)}`;
		expect(validateQuerySql({ querySql: sql }, true)).toEqual({
			querySql: '8192文字以内で入力してください'
		});
	});
});
