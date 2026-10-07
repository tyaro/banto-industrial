// `ci-changes.mjs` の判定表テスト（#468）。
// 実行: `node --test .github/scripts/ci-changes.test.mjs`
// CI の `changes` ジョブも判定の前に毎回これを実行する。

import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { classify, classifyAreas, RUST_PACKAGES } from './ci-changes.mjs';

const ALL = {
	run_lint_format: true,
	run_frontend_hub: true,
	run_frontend_chronogazer: true,
	run_rust: true,
	rust_args: '--workspace',
	run_rust_db_source: true,
	run_e2e_hub: true,
	run_e2e_chronogazer: true
};

const NONE = {
	run_lint_format: true,
	run_frontend_hub: false,
	run_frontend_chronogazer: false,
	run_rust: false,
	rust_args: '',
	run_rust_db_source: false,
	run_e2e_hub: false,
	run_e2e_chronogazer: false
};

const HUB_RUST = '-p banto-hub-core -p banto-hub-shell -p banto-hub-sink -p banto-tagclient';
const CHRONO_RUST = '-p chronogazer -p chronogazer-core';

/** [名前, 変更ファイル, 期待するジョブ] */
const TABLE = [
	// --- issue #468 で確かめる 4 つの組み合わせ
	['docs だけ', ['docs/plan.md', 'README.md'], NONE],
	[
		'hub だけ（frontend）',
		['apps/banto-hub/src/routes/+page.svelte'],
		{
			...NONE,
			run_frontend_hub: true,
			run_rust: true,
			rust_args: HUB_RUST,
			run_rust_db_source: true,
			run_e2e_hub: true
		}
	],
	[
		'hub だけ（core）',
		['apps/banto-hub/core/src/stream.rs'],
		{
			...NONE,
			run_frontend_hub: true,
			run_rust: true,
			rust_args: HUB_RUST,
			run_rust_db_source: true,
			run_e2e_hub: true
		}
	],
	[
		'chronogazer だけ',
		['apps/chronogazer/src/lib/foo.ts', 'apps/chronogazer/core/src/lib.rs'],
		{
			...NONE,
			run_frontend_chronogazer: true,
			run_rust: true,
			rust_args: CHRONO_RUST,
			run_e2e_chronogazer: true
		}
	],
	['crates の変更', ['crates/banto-tags/src/lib.rs'], ALL_BUT_FRONTEND()],

	// --- それ以外
	[
		'banto-hub-sink だけ',
		['apps/banto-hub-sink/src/main.rs'],
		{ ...NONE, run_rust: true, rust_args: '-p banto-hub-sink', run_rust_db_source: true }
	],
	['Cargo.lock', ['Cargo.lock'], ALL_BUT_FRONTEND()],
	['root Cargo.toml', ['Cargo.toml'], ALL_BUT_FRONTEND()],
	['rust-toolchain.toml', ['rust-toolchain.toml'], ALL_BUT_FRONTEND()],
	['proto', ['proto/tagserver/v1/tagserver.proto'], ALL_BUT_FRONTEND()],
	['.cargo', ['.cargo/config.toml'], ALL_BUT_FRONTEND()],
	[
		'pnpm-lock.yaml',
		['pnpm-lock.yaml'],
		{
			...NONE,
			run_frontend_hub: true,
			run_frontend_chronogazer: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'eslint.config.js',
		['eslint.config.js'],
		{
			...NONE,
			run_frontend_hub: true,
			run_frontend_chronogazer: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	['.github の変更はフル', ['.github/workflows/ci.yml'], ALL],
	['判定スクリプト自身もフル', ['.github/scripts/ci-changes.mjs'], ALL],
	['scripts/ はフル', ['scripts/build-release.ps1'], ALL],
	['どの規則にも当たらないパスはフル', ['some-new-dir/file.txt'], ALL],
	['.gitattributes はフル', ['.gitattributes'], ALL],
	[
		'hub の E2E だけ',
		['e2e/tests-banto-hub/banto-hub-smoke.spec.ts'],
		{ ...NONE, run_e2e_hub: true }
	],
	[
		'hub の Playwright 設定',
		['e2e/banto-hub.playwright.config.ts'],
		{ ...NONE, run_e2e_hub: true }
	],
	['chronogazer の E2E だけ', ['e2e/tests/smoke.spec.ts'], { ...NONE, run_e2e_chronogazer: true }],
	[
		'chronogazer の Playwright 設定',
		['e2e/playwright.config.ts'],
		{ ...NONE, run_e2e_chronogazer: true }
	],
	[
		'chronogazer の閲覧公開 E2E（spec と config）',
		['e2e/tests-public-viewer/public-viewer.spec.ts', 'e2e/public-viewer.playwright.config.ts'],
		{ ...NONE, run_e2e_chronogazer: true }
	],
	[
		'E2E 共通（tsconfig）は両方',
		['e2e/tsconfig.json'],
		{ ...NONE, run_e2e_hub: true, run_e2e_chronogazer: true }
	],
	['E2E の README は docs', ['e2e/README.md'], NONE],
	['.claude の設定は docs 扱い', ['.claude/launch.json'], NONE],
	[
		'hub と chronogazer の両方',
		['apps/banto-hub/src/app.css', 'apps/chronogazer/src/app.css'],
		{
			...NONE,
			run_frontend_hub: true,
			run_frontend_chronogazer: true,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-tagclient',
				'-p chronogazer',
				'-p chronogazer-core'
			].join(' '),
			run_rust_db_source: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'docs + crates なら crates 側（全体）',
		['docs/plan.md', 'crates/banto-expr/src/lib.rs'],
		ALL_BUT_FRONTEND()
	],
	['Windows 区切りでも同じ判定', ['apps\\banto-hub-sink\\src\\main.rs'], null],
	['変更 0 件（空行だけ）', ['', '  '], NONE]
];

function ALL_BUT_FRONTEND() {
	return { ...ALL, run_frontend_hub: false, run_frontend_chronogazer: false };
}

for (const [name, files, expected] of TABLE) {
	test(`PR: ${name}`, () => {
		const got = classify({ event: 'pull_request', files }).jobs;
		const want = expected ?? {
			...NONE,
			run_rust: true,
			rust_args: '-p banto-hub-sink',
			run_rust_db_source: true
		};
		assert.deepEqual(got, want);
	});
}

test('push（main）は変更内容に関係なくフル', () => {
	assert.deepEqual(classify({ event: 'push', files: ['docs/plan.md'] }).jobs, ALL);
});

test('PR 以外の起動（workflow_dispatch 等）はフル', () => {
	assert.deepEqual(classify({ event: 'workflow_dispatch', files: null }).jobs, ALL);
	assert.deepEqual(classify({ event: 'merge_group', files: ['docs/plan.md'] }).jobs, ALL);
});

test('PR で差分が取れなかったらフル', () => {
	const r = classify({ event: 'pull_request', files: null });
	assert.deepEqual(r.jobs, ALL);
	assert.ok(r.forced);
});

test('banto-hub-sink は banto-hub に誤って当たらない', () => {
	const { areas } = classifyAreas(['apps/banto-hub-sink/Cargo.toml']);
	assert.equal(areas.hub, false);
	assert.equal(areas.hub_sink, true);
});

// 今リポジトリにある全ファイルが、フォールバック（どの規則にも当たらない）に
// 落ちずに明示の規則で分類されること。新しいトップレベルのディレクトリや
// 設定ファイルを足したら、ここで気付いて RULES に足す（落ちても CI 上は
// フル CI になるだけで危険側ではないが、判定表を網羅に保つため）。
test('リポジトリの全ファイルが明示の規則に当たる', (t) => {
	let files;
	try {
		files = execFileSync('git', ['ls-files'], { encoding: 'utf8' }).split('\n');
	} catch {
		t.skip('git ls-files が使えない');
		return;
	}
	const unmatched = classifyAreas(files)
		.matches.filter((m) => m.why.startsWith('どの規則にも当たらない'))
		.map((m) => m.file);
	assert.deepEqual(unmatched, []);
});

// RUST_PACKAGES の名前が workspace の実在パッケージと一致すること（改名で
// `cargo -p` が「そんなパッケージは無い」と落ちる / 対象から漏れるのを防ぐ）。
test('RUST_PACKAGES は workspace の実在パッケージ', () => {
	const root = new URL('../../', import.meta.url);
	const cargo = readFileSync(new URL('Cargo.toml', root), 'utf8');
	const members = [...cargo.match(/members\s*=\s*\[([^\]]*)\]/)[1].matchAll(/"([^"]+)"/g)].map(
		(m) => m[1]
	);
	const names = new Set(
		members.map((dir) => {
			const toml = readFileSync(new URL(`${dir}/Cargo.toml`, root), 'utf8');
			return toml.match(/^name\s*=\s*"([^"]+)"/m)[1];
		})
	);
	for (const pkgs of Object.values(RUST_PACKAGES)) {
		for (const p of pkgs) assert.ok(names.has(p), `${p} は workspace に無い`);
	}
});
