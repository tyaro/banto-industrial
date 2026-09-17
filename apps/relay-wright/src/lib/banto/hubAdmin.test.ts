/**
 * `hubAdmin.ts` の純関数（6 状態 → 画面文言のマッピング）に対するユニット
 * テスト（#332 relay-wright 分）。`categories.test.ts` と同じ
 * describe/it スタイルで、依存ゼロで直接 import できる範囲だけを固定する
 * （`invoke`/`fetch` を伴う関数は E2E と Rust 側のテストが担当する）。
 *
 * ここで固定したい核心は受入条件の 2 点:
 * 1. **6 状態がすべて別の文言になる**（どれか 2 つが同じ表示に潰れない）。
 * 2. **`connected` の `tagCount: 0` は失敗ではない** - 「接続済み・利用
 *    可能なタグなし」という専用の文言になる。
 */
import { describe, expect, it } from 'vitest';
import {
	hubStatusDetail,
	hubStatusLabel,
	hubUnreachableCauseLabel,
	needsManualKey,
	type HubStatus
} from './hubAdmin';

const ALL_STATES: HubStatus[] = [
	{ state: 'notConfigured' },
	{ state: 'connected', tagCount: 3 },
	{ state: 'authFailed' },
	{ state: 'forbidden' },
	{ state: 'unreachable', cause: 'transport' },
	{ state: 'needsPairing' }
];

describe('hubStatusLabel', () => {
	it('6状態がそれぞれ別の見出しになる（どれかが同じ表示に潰れない）', () => {
		const labels = ALL_STATES.map(hubStatusLabel);
		expect(new Set(labels).size).toBe(ALL_STATES.length);
		expect(labels).not.toContain('');
	});

	it('タグ0件は失敗ではなく「接続済み・利用可能なタグなし」になる', () => {
		expect(hubStatusLabel({ state: 'connected', tagCount: 0 })).toBe(
			'接続済み・利用可能なタグなし'
		);
		expect(hubStatusLabel({ state: 'connected', tagCount: 2 })).toBe('接続済み（タグ2件）');
	});

	it('到達不能・認証失敗・権限不足・連携要求は互いに区別される', () => {
		expect(hubStatusLabel({ state: 'unreachable', cause: 'transport' })).toBe(
			'Hubに到達できません'
		);
		expect(hubStatusLabel({ state: 'authFailed' })).toBe('認証に失敗');
		expect(hubStatusLabel({ state: 'forbidden' })).toBe('権限が不足');
		expect(hubStatusLabel({ state: 'needsPairing' })).toBe('連携が必要');
	});
});

describe('hubStatusDetail', () => {
	it('どの状態でも「次に何をすればよいか」の説明が付く', () => {
		for (const status of ALL_STATES) {
			expect(hubStatusDetail(status).length).toBeGreaterThan(0);
		}
	});

	it('タグ0件の説明は「接続できているがタグが無い」ことを述べ、失敗として扱わない', () => {
		const detail = hubStatusDetail({ state: 'connected', tagCount: 0 });
		expect(detail).toContain('タグが登録されていません');
	});

	it('到達不能の説明には原因の日本語が含まれる', () => {
		expect(hubStatusDetail({ state: 'unreachable', cause: 'server_error' })).toContain(
			hubUnreachableCauseLabel('server_error')
		);
	});

	it('連携が必要の説明はロックダウン済みで自動発行しないことを述べる', () => {
		expect(hubStatusDetail({ state: 'needsPairing' })).toContain('ロックダウン');
	});
});

describe('hubUnreachableCauseLabel', () => {
	it('原因ごとに別の文言になる', () => {
		const labels = (['transport', 'protocol', 'server_error', 'invalid_endpoint'] as const).map(
			hubUnreachableCauseLabel
		);
		expect(new Set(labels).size).toBe(4);
	});
});

describe('needsManualKey', () => {
	it('手入力欄はロックダウン済み（連携が必要）と権限不足のときだけ出す', () => {
		expect(needsManualKey({ state: 'needsPairing' })).toBe(true);
		expect(needsManualKey({ state: 'forbidden' })).toBe(true);
	});

	it('再発行で直る状態・正常な状態では手入力欄を出さない', () => {
		// authFailed は「接続」で再発行できるので、まず手入力を促さない。
		expect(needsManualKey({ state: 'authFailed' })).toBe(false);
		expect(needsManualKey({ state: 'notConfigured' })).toBe(false);
		expect(needsManualKey({ state: 'connected', tagCount: 0 })).toBe(false);
		expect(needsManualKey({ state: 'unreachable', cause: 'transport' })).toBe(false);
	});
});
