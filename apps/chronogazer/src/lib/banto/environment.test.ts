/**
 * `isTauri()` の origin の判定（#392 B2-2、設計書 docs/chronogazer-service-design.md
 * §3.2）。シェルがサービス（`chronogazer-server`）の HTTP の画面へ navigate すると、
 * Tauri はその窓にも `__TAURI_INTERNALS__` を注入する。内部オブジェクトの有無だけで
 * 判定すると invoke の経路を選んで壊れるので、origin が Tauri 自身のアセットのものかも
 * 見る。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import tauriConf from '../../../src-tauri/tauri.conf.json';
import { TAURI_DEV_ORIGIN, TAURI_ORIGINS, isTauri, isTauriOrigin } from './environment';

afterEach(() => {
	vi.unstubAllGlobals();
	vi.unstubAllEnvs();
});

describe('isTauriOrigin（純関数）', () => {
	const cases: [string, string | undefined, boolean][] = [
		['http://tauri.localhost', undefined, true],
		['https://tauri.localhost', undefined, true],
		['tauri://localhost', undefined, true],
		// サービス・LAN の画面は Tauri 自身ではない。
		['http://127.0.0.1:8721', undefined, false],
		['http://localhost:8721', undefined, false],
		['http://[::1]:8721', undefined, false],
		['http://192.0.2.10:8721', undefined, false],
		// 開発の origin は開発ビルドのときだけ（`devOrigin` を渡したときだけ）。
		['http://localhost:1420', undefined, false],
		['http://localhost:1420', TAURI_DEV_ORIGIN, true],
		['http://127.0.0.1:8721', TAURI_DEV_ORIGIN, false],
		// 似た名前を通さない。
		['http://tauri.localhost.evil.example', undefined, false],
		['http://tauri.localhost:8080', undefined, false]
	];
	for (const [origin, devOrigin, expected] of cases) {
		it(`${origin}（dev: ${devOrigin ?? 'なし'}）→ ${expected}`, () => {
			expect(isTauriOrigin(origin, devOrigin)).toBe(expected);
		});
	}

	it('TAURI_ORIGINS は Windows（http/https）と macOS・Linux の 3 つ', () => {
		expect([...TAURI_ORIGINS].sort()).toEqual(
			['http://tauri.localhost', 'https://tauri.localhost', 'tauri://localhost'].sort()
		);
	});
});

describe('TAURI_DEV_ORIGIN は tauri.conf.json の devUrl と同じ', () => {
	it('devUrl の origin', () => {
		expect(new URL(tauriConf.build.devUrl).origin).toBe(TAURI_DEV_ORIGIN);
	});
});

describe('isTauri()', () => {
	it('window が無い（SSR・node）なら false', () => {
		expect(isTauri()).toBe(false);
	});

	it('内部オブジェクトが無ければ、Tauri の origin でも false', () => {
		vi.stubGlobal('window', { location: { origin: 'http://tauri.localhost' } });
		expect(isTauri()).toBe(false);
	});

	it('内部オブジェクトがあり Tauri 自身の origin なら true', () => {
		for (const origin of TAURI_ORIGINS) {
			vi.stubGlobal('window', { __TAURI_INTERNALS__: {}, location: { origin } });
			expect(isTauri()).toBe(true);
		}
	});

	it('内部オブジェクトがあってもサービスへ navigate した窓（127.0.0.1）なら false', () => {
		vi.stubGlobal('window', {
			__TAURI_INTERNALS__: {},
			location: { origin: 'http://127.0.0.1:8721' }
		});
		expect(isTauri()).toBe(false);
	});

	it('開発ビルド（vitest は DEV）では tauri dev の origin も true', () => {
		expect(import.meta.env.DEV).toBe(true);
		vi.stubGlobal('window', {
			__TAURI_INTERNALS__: {},
			location: { origin: TAURI_DEV_ORIGIN }
		});
		expect(isTauri()).toBe(true);
	});
});
