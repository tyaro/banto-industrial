/**
 * H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）:
 * `apiKeysAdmin.ts` が re-export する `apiKeyWarnings`（実体は
 * `./apiKeyWarnings`、純関数）に対するユニットテスト。しきい値定数
 * （`EXPIRY_WARNING_THRESHOLD_MS` = 14日、`LONG_UNUSED_THRESHOLD_MS` =
 * 90日）そのものと、境界値・排他性（期限切れなら期限接近は立てない等）を
 * 仕様として固定する。`tagCsv.test.ts` と同じスタイル（最小フィクスチャ +
 * describe/it）。
 *
 * `./apiKeysAdmin` からではなく `./apiKeyWarnings` から直接 import する
 * 点に注意 - `apiKeysAdmin.ts` は `@banto/admin-core`（Svelte 5 rune を
 * 使う `.svelte.ts` を推移的に import する）をトップレベルで import して
 * おり、`@sveltejs/vite-plugin-svelte` を導入しないこのリポジトリの最小
 * vitest 構成（`vitest.config.ts` の doc comment、H5 決定）ではそれだけで
 * `ReferenceError: $state is not defined` になる。`apiKeyWarnings.ts` は
 * 依存ゼロなのでこの問題を起こさない（同ファイルの doc comment参照）。
 */
import { describe, expect, it } from 'vitest';
import {
	apiKeyWarnings,
	EXPIRY_WARNING_THRESHOLD_MS,
	LONG_UNUSED_THRESHOLD_MS,
	type ApiKeyExpiryInfo
} from './apiKeyWarnings';

const DAY_MS = 24 * 60 * 60 * 1000;
// 適当な固定基準時刻（2023-11-14T22:13:20.000Z）- 何年か等は無関係、値の
// 大きさが epoch ミリ秒として現実的であることだけ確認できれば十分。
const NOW = 1_700_000_000_000;

/** 最小フィクスチャ - `apiKeyWarnings` が読むのは `expiresAt`/`lastUsedAt`
 *  の2フィールドのみ（[`ApiKeyExpiryInfo`] 参照）。 */
function makeKey(overrides: Partial<ApiKeyExpiryInfo> = {}): ApiKeyExpiryInfo {
	return {
		lastUsedAt: NOW - DAY_MS, // 既定: 1日前に使用(長期未使用ではない)
		expiresAt: null, // 既定: 無期限
		...overrides
	};
}

describe('しきい値定数', () => {
	it('EXPIRY_WARNING_THRESHOLD_MS は14日', () => {
		expect(EXPIRY_WARNING_THRESHOLD_MS).toBe(14 * DAY_MS);
	});

	it('LONG_UNUSED_THRESHOLD_MS は90日', () => {
		expect(LONG_UNUSED_THRESHOLD_MS).toBe(90 * DAY_MS);
	});
});

describe('apiKeyWarnings', () => {
	describe('無期限キー(expiresAt: null)', () => {
		it('expiringSoon/expired は常に false（使用状況に関わらず）', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: null, lastUsedAt: NOW }), NOW);
			expect(w.expiringSoon).toBe(false);
			expect(w.expired).toBe(false);
		});
	});

	describe('期限接近(expiringSoon)', () => {
		it('残り14日ちょうどは接近とみなす(境界含む)', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: NOW + 14 * DAY_MS }), NOW);
			expect(w.expiringSoon).toBe(true);
			expect(w.expired).toBe(false);
		});

		it('残り14日を1msでも超えていれば接近ではない', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: NOW + 14 * DAY_MS + 1 }), NOW);
			expect(w.expiringSoon).toBe(false);
		});

		it('残り1日でも接近', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: NOW + DAY_MS }), NOW);
			expect(w.expiringSoon).toBe(true);
		});
	});

	describe('期限切れ(expired)', () => {
		it('過去の expiresAt は expired: true(かつ expiringSoon は false - 排他)', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: NOW - DAY_MS }), NOW);
			expect(w.expired).toBe(true);
			expect(w.expiringSoon).toBe(false);
		});

		it('expiresAt が nowMs ちょうどは expired 側(境界 - サーバー側 is_expired と同じ)', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: NOW }), NOW);
			expect(w.expired).toBe(true);
			expect(w.expiringSoon).toBe(false);
		});
	});

	describe('長期未使用(longUnused)', () => {
		it('90日ちょうど未使用は長期未使用とみなす(境界含む)', () => {
			const w = apiKeyWarnings(makeKey({ lastUsedAt: NOW - 90 * DAY_MS }), NOW);
			expect(w.longUnused).toBe(true);
		});

		it('89日なら長期未使用ではない', () => {
			const w = apiKeyWarnings(makeKey({ lastUsedAt: NOW - 89 * DAY_MS }), NOW);
			expect(w.longUnused).toBe(false);
		});

		it('一度も使われていない(lastUsedAt: null)場合は long-unused としない(発行直後の誤検知を避ける設計判断)', () => {
			const w = apiKeyWarnings(makeKey({ lastUsedAt: null }), NOW);
			expect(w.longUnused).toBe(false);
		});

		it('無期限キーでも長期未使用は独立して判定される', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: null, lastUsedAt: NOW - 90 * DAY_MS }), NOW);
			expect(w.longUnused).toBe(true);
			expect(w.expired).toBe(false);
			expect(w.expiringSoon).toBe(false);
		});
	});

	describe('オールクリア(全て false)', () => {
		it('無期限 + 最近使用', () => {
			const w = apiKeyWarnings(makeKey({ expiresAt: null, lastUsedAt: NOW - DAY_MS }), NOW);
			expect(w).toEqual({ expiringSoon: false, expired: false, longUnused: false });
		});

		it('期限が十分先 + 最近使用', () => {
			const w = apiKeyWarnings(
				makeKey({ expiresAt: NOW + 365 * DAY_MS, lastUsedAt: NOW - DAY_MS }),
				NOW
			);
			expect(w).toEqual({ expiringSoon: false, expired: false, longUnused: false });
		});
	});
});
