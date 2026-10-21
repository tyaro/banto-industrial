// `ci-changes.mjs` の判定表テスト（#468）。
// 実行: `node --test .github/scripts/ci-changes.test.mjs`
// CI の `changes` ジョブも判定の前に毎回これを実行する。

import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { posix } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
	classify,
	classifyAreas,
	escapeRegExp,
	graphFromMetadata,
	JOB_PACKAGES,
	loadRustGraph,
	normalizePath,
	owningPackage,
	RDEPS_FULL_RATIO,
	reverseDeps,
	RUST_PACKAGES,
	touchesCrates
} from './ci-changes.mjs';

const REPO_ROOT = fileURLToPath(new URL('../../', import.meta.url));

/**
 * 表テスト用の固定の依存グラフ。実グラフ（`cargo metadata`）の一部を写したもの
 * （辺が実グラフにもあることは下の「固定のグラフは実グラフの一部」で確かめる）。
 * 実グラフが育っても表の期待が揺れないように、表はこちらで引く。
 * @type {import('./ci-changes.mjs').RustGraph}
 */
const G = {
	packages: [
		{ name: 'banto-plc-address', dir: 'crates/banto-plc-address', deps: [] },
		{ name: 'banto-tags', dir: 'crates/banto-tags', deps: ['banto-plc-address'] },
		// R1-1: 公開するドライバ API（依存ゼロ）と、その型を再輸出する banto-plc。
		{ name: 'banto-driver-api', dir: 'crates/banto-driver-api', deps: [] },
		// R1-2: 公開する適合テスト一式（API にだけ依存）。
		{
			name: 'banto-driver-conformance',
			dir: 'crates/banto-driver-conformance',
			deps: ['banto-driver-api']
		},
		// R1-3b: ホストが注入する組み込みドライバ一式。bin（R1-7a から banto-hub-host）・shell・
		// sink のテストが使う（G には写していない broker / collect にも依存する）。
		// R1-4a: broker の表の 2 行を、下の 2 つのドライバ crate の `dial` で組む。
		{
			name: 'banto-drivers',
			dir: 'crates/banto-drivers',
			deps: ['banto-driver-api', 'banto-driver-modbus', 'banto-driver-slmp']
		},
		// R1-4a: banto-broker から移した SLMP / Modbus TCP のドライバ（内部 crate）。driver-api は
		// dev-dependency。banto-plc・banto-tags・broker・plc-write にも依存するが、G には写して
		// いない（banto-plc / banto-tags の行の期待を R1-6 まで揺らさないため。banto-drivers が
		// broker / collect を写していないのと同じ扱い）。
		// R1-5: tests/conformance.rs の dev-dependency で適合テスト一式に依存する（dev の辺も
		// 推移的に辿るので、conformance の変更でドライバ → banto-drivers → hub まで走る）。
		{
			name: 'banto-driver-modbus',
			dir: 'crates/banto-driver-modbus',
			deps: ['banto-driver-api', 'banto-driver-conformance']
		},
		{
			name: 'banto-driver-slmp',
			dir: 'crates/banto-driver-slmp',
			deps: ['banto-driver-api', 'banto-driver-conformance']
		},
		{
			name: 'banto-plc',
			dir: 'crates/banto-plc',
			deps: ['banto-driver-api', 'banto-plc-address']
		},
		{ name: 'banto-instance-lock', dir: 'crates/banto-instance-lock', deps: [] },
		{ name: 'banto-rtsp', dir: 'crates/banto-rtsp', deps: [] },
		{ name: 'banto-tagclient', dir: 'crates/banto-tagclient', deps: [] },
		{ name: 'banto-hub-bootstrap', dir: 'crates/banto-hub-bootstrap', deps: ['banto-tagclient'] },
		{ name: 'banto-service-elev', dir: 'crates/banto-service-elev', deps: [] },
		{ name: 'banto-service-control', dir: 'crates/banto-service-control', deps: [] },
		{
			name: 'banto-safe-root',
			dir: 'crates/banto-safe-root',
			// banto-service-elev は dev-dependency。
			deps: ['banto-service-elev']
		},
		{
			name: 'banto-secret-store',
			dir: 'crates/banto-secret-store',
			deps: ['banto-hub-bootstrap', 'banto-safe-root']
		},
		{
			name: 'chronogazer-core',
			dir: 'apps/chronogazer/core',
			deps: [
				'banto-hub-bootstrap',
				'banto-instance-lock',
				'banto-safe-root',
				'banto-secret-store',
				'banto-service-control',
				'banto-tagclient',
				'banto-tags'
			]
		},
		{
			name: 'chronogazer-elev',
			dir: 'apps/chronogazer/elev',
			deps: ['banto-safe-root', 'banto-service-control', 'banto-service-elev', 'chronogazer-core']
		},
		{
			name: 'chronogazer',
			dir: 'apps/chronogazer/src-tauri',
			deps: ['banto-hub-bootstrap', 'banto-safe-root', 'banto-service-control', 'chronogazer-core']
		},
		{
			name: 'banto-hub-core',
			dir: 'apps/banto-hub/core',
			// R1-7a: banto-drivers は dev-dependency だけ（bin が banto-hub-host へ移った。
			// src 内の単体テストと tests/ が実ドライバを使う。R2b-0 で外す）。
			deps: [
				'banto-driver-api',
				'banto-drivers',
				'banto-instance-lock',
				'banto-safe-root',
				'banto-service-control',
				'banto-service-elev',
				'banto-tags'
			]
		},
		// R1-7a: bin `banto-hub`・`banto-hub-elev` の package。banto-drivers を normal で持つ。
		{
			name: 'banto-hub-host',
			dir: 'apps/banto-hub/host',
			deps: ['banto-drivers', 'banto-hub-core', 'banto-safe-root']
		},
		{
			name: 'banto-hub-shell',
			dir: 'apps/banto-hub/src-tauri',
			deps: ['banto-drivers', 'banto-hub-core', 'banto-service-elev']
		},
		{
			name: 'banto-hub-sink',
			dir: 'apps/banto-hub-sink',
			// banto-drivers・banto-hub-core・banto-tags は dev-dependency。
			deps: [
				'banto-drivers',
				'banto-hub-core',
				'banto-safe-root',
				'banto-service-control',
				'banto-tagclient',
				'banto-tags'
			]
		}
	]
};

/** 実グラフ（`cargo metadata`）。1 回だけ読む。 */
let realGraph;
function real() {
	realGraph ??= loadRustGraph(REPO_ROOT);
	return realGraph;
}

/** `-p a -p b` → ['a', 'b']。`--workspace` は実グラフの全パッケージ。 */
function rustPackages(rustArgs, graph) {
	if (rustArgs === '--workspace') return graph.packages.map((p) => p.name);
	return rustArgs === '' ? [] : rustArgs.split(' ').filter((w) => w !== '-p');
}

const ALL = {
	run_lint_format: true,
	run_frontend_hub: true,
	run_frontend_chronogazer: true,
	run_rust: true,
	rust_args: '--workspace',
	run_driver_api: true,
	run_rust_db_source: true,
	run_linux_clippy: true,
	run_e2e_hub: true,
	run_e2e_chronogazer: true
};

const NONE = {
	run_lint_format: true,
	run_frontend_hub: false,
	run_frontend_chronogazer: false,
	run_rust: false,
	rust_args: '',
	run_driver_api: false,
	run_rust_db_source: false,
	run_linux_clippy: false,
	run_e2e_hub: false,
	run_e2e_chronogazer: false
};

const HUB_RUST =
	'-p banto-hub-core -p banto-hub-host -p banto-hub-shell -p banto-hub-sink -p banto-tagclient';
const CHRONO_RUST = '-p chronogazer -p chronogazer-core -p chronogazer-elev';

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
			run_linux_clippy: true,
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
			run_linux_clippy: true,
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
			run_linux_clippy: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'crates の変更は、そのパッケージと逆依存だけ（banto-tags）',
		['crates/banto-tags/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-tags',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-instance-lock だけ（両アプリの core に届く）',
		['crates/banto-instance-lock/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-instance-lock',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-secret-store だけ（chronogazer にだけ届く。hub の E2E・PostgreSQL は走らない）',
		['crates/banto-secret-store/src/acl.rs', 'crates/banto-secret-store/Cargo.toml'],
		{
			...NONE,
			run_rust: true,
			rust_args: '-p banto-secret-store -p chronogazer -p chronogazer-core -p chronogazer-elev',
			run_linux_clippy: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-safe-root（#623 PR-1a の新 crate。hub・chronogazer の両方に届く）',
		['crates/banto-safe-root/src/acl.rs', 'crates/banto-safe-root/Cargo.toml'],
		{
			...NONE,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-safe-root',
				'-p banto-secret-store',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-service-elev（dev-dependency を辿って banto-safe-root・banto-secret-store の逆依存まで届く）',
		['crates/banto-service-elev/src/win.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-safe-root',
				'-p banto-secret-store',
				'-p banto-service-elev',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-tagclient（sink に届くので PostgreSQL は走る。hub の E2E は走らない）',
		['crates/banto-tagclient/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args: [
				'-p banto-hub-bootstrap',
				'-p banto-hub-sink',
				'-p banto-secret-store',
				'-p banto-tagclient',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'banto-driver-api（R1-1 の公開 crate。MSRV / semver の driver-api ジョブが走る。R1-3b から core が直接依存するので hub 側も走る）',
		['crates/banto-driver-api/src/types.rs', 'crates/banto-driver-api/CHANGELOG.md'],
		{
			...NONE,
			run_rust: true,
			rust_args:
				'-p banto-driver-api -p banto-driver-conformance -p banto-driver-modbus -p banto-driver-slmp -p banto-drivers -p banto-hub-core -p banto-hub-host -p banto-hub-shell -p banto-hub-sink -p banto-plc',
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_driver_api: true
		}
	],
	[
		'banto-driver-conformance（R1-2 の公開 crate。driver-api ジョブが走る。R1-5 から Modbus / SLMP のドライバが dev-dependency で使うので、その逆依存の hub 側も走る）',
		[
			'crates/banto-driver-conformance/src/frame.rs',
			'crates/banto-driver-conformance/CHANGELOG.md'
		],
		{
			...NONE,
			run_rust: true,
			rust_args:
				'-p banto-driver-conformance -p banto-driver-modbus -p banto-driver-slmp -p banto-drivers -p banto-hub-core -p banto-hub-host -p banto-hub-shell -p banto-hub-sink',
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_driver_api: true
		}
	],
	[
		'banto-drivers（R1-3b の内部 crate）は hub の 4 パッケージが使う（R1-7a から host は normal、core・sink は dev）。公開 crate ではないので driver-api ジョブは走らない',
		['crates/banto-drivers/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args:
				'-p banto-drivers -p banto-hub-core -p banto-hub-host -p banto-hub-shell -p banto-hub-sink',
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true
		}
	],
	[
		'banto-driver-modbus（R1-6 の行。lib.rs だけの変更。banto-drivers 経由で hub の 4 パッケージへ届く。banto-hub-core へは dev-dependency 経由で届くが、R2b-0 でこの行を書き換えて banto-hub-core を外す）',
		['crates/banto-driver-modbus/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args:
				'-p banto-driver-modbus -p banto-drivers -p banto-hub-core -p banto-hub-host -p banto-hub-shell -p banto-hub-sink',
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true
		}
	],
	[
		'banto-driver-api の逆依存（banto-plc）だけの変更では driver-api ジョブは走らない',
		['crates/banto-plc/src/decode.rs'],
		{ ...NONE, run_rust: true, rust_args: '-p banto-plc' }
	],
	[
		'逆依存の無い crate はその crate だけ',
		['crates/banto-rtsp/src/lib.rs'],
		{ ...NONE, run_rust: true, rust_args: '-p banto-rtsp' }
	],
	['crates/ の下でどのパッケージにも属さないパスは全体', ['crates/README.md'], ALL_BUT_FRONTEND()],
	[
		'グラフに無い crate（新しく足した等）は全体',
		['crates/banto-new/src/lib.rs'],
		ALL_BUT_FRONTEND()
	],
	[
		'crates + root の Cargo.toml は全体（root が勝つ）',
		['crates/banto-rtsp/Cargo.toml', 'Cargo.toml'],
		ALL_BUT_FRONTEND()
	],
	[
		'crates + hub の app は和集合',
		['crates/banto-secret-store/src/lib.rs', 'apps/banto-hub/core/src/lib.rs'],
		{
			...NONE,
			run_frontend_hub: true,
			run_rust: true,
			rust_args: [
				'-p banto-hub-core',
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-secret-store',
				'-p banto-tagclient',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'chronogazer の elev だけ（昇格の境界）',
		['apps/chronogazer/elev/src/lib.rs'],
		{
			...NONE,
			run_rust: true,
			rust_args: CHRONO_RUST,
			run_linux_clippy: true,
			run_frontend_chronogazer: true,
			run_e2e_chronogazer: true
		}
	],

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
	['.config (nextest)', ['.config/nextest.toml'], ALL_BUT_FRONTEND()],
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
	['OPC UA interop 専用 workflow の変更はフル', ['.github/workflows/opcua-interop.yml'], ALL],
	['判定スクリプト自身もフル', ['.github/scripts/ci-changes.mjs'], ALL],
	['scripts/ はフル', ['scripts/build-release.ps1'], ALL],
	['どの規則にも当たらないパスはフル', ['some-new-dir/file.txt'], ALL],
	[
		'OPC UA interop の依存・試験ファイルだけ',
		['interop/opcua/requirements.txt', 'interop/opcua/smoke.py'],
		NONE
	],
	['未知の interop パスはフル', ['interop/other/smoke.py'], ALL],
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
		'chronogazer のサービス接続 E2E（spec と config、#392 B2-2）',
		[
			'e2e/tests-chronogazer-server/local-grant.spec.ts',
			'e2e/chronogazer-server.playwright.config.ts'
		],
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
				'-p banto-hub-host',
				'-p banto-hub-shell',
				'-p banto-hub-sink',
				'-p banto-tagclient',
				'-p chronogazer',
				'-p chronogazer-core',
				'-p chronogazer-elev'
			].join(' '),
			run_rust_db_source: true,
			run_linux_clippy: true,
			run_e2e_hub: true,
			run_e2e_chronogazer: true
		}
	],
	[
		'docs + crates なら crates 側',
		['docs/plan.md', 'crates/banto-rtsp/src/lib.rs'],
		{ ...NONE, run_rust: true, rust_args: '-p banto-rtsp' }
	],
	['Windows 区切りでも同じ判定', ['apps\\banto-hub-sink\\src\\main.rs'], null],
	['変更 0 件（空行だけ）', ['', '  '], NONE]
];

function ALL_BUT_FRONTEND() {
	return { ...ALL, run_frontend_hub: false, run_frontend_chronogazer: false };
}

for (const [name, files, expected] of TABLE) {
	test(`PR: ${name}`, () => {
		const got = classify({ event: 'pull_request', files, graph: G }).jobs;
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

// RUST_PACKAGES・JOB_PACKAGES の名前が workspace の実在パッケージと一致すること（改名で
// `cargo -p` が「そんなパッケージは無い」と落ちる / 対象から漏れるのを防ぐ）。
test('RUST_PACKAGES・JOB_PACKAGES は workspace の実在パッケージ', () => {
	const names = new Set(real().packages.map((p) => p.name));
	for (const pkgs of [...Object.values(RUST_PACKAGES), ...Object.values(JOB_PACKAGES)]) {
		for (const p of pkgs) assert.ok(names.has(p), `${p} は workspace に無い`);
	}
});

// --- 共有 crate の依存グラフ（2026-10-16 オーナー決定）

test('push（main）は crates/ の変更でもグラフに関係なくフル', () => {
	const r = classify({ event: 'push', files: ['crates/banto-rtsp/src/lib.rs'], graph: G });
	assert.deepEqual(r.jobs, ALL);
});

test('グラフが無い（cargo metadata の失敗）なら crates/ の変更は全体', () => {
	const r = classify({ event: 'pull_request', files: ['crates/banto-rtsp/src/lib.rs'] });
	assert.deepEqual(r.jobs, ALL_BUT_FRONTEND());
	assert.match(r.crates.fallback ?? '', /依存グラフが無い/);
});

test(`逆依存が workspace の ${RDEPS_FULL_RATIO * 100}% 以上に届くなら全体`, () => {
	// 5 パッケージのうち base を変えると 4/5 = 80% に届く。
	const g = {
		packages: [
			{ name: 'base', dir: 'crates/base', deps: [] },
			{ name: 'a', dir: 'crates/a', deps: ['base'] },
			{ name: 'b', dir: 'crates/b', deps: ['a'] },
			{ name: 'c', dir: 'crates/c', deps: ['base'] },
			{ name: 'lone', dir: 'crates/lone', deps: [] }
		]
	};
	const r = classify({ event: 'pull_request', files: ['crates/base/src/lib.rs'], graph: g });
	assert.equal(r.jobs.rust_args, '--workspace');
	assert.equal(r.jobs.run_e2e_hub, true);
	assert.match(r.crates.fallback ?? '', /4\/5/);
	// 2/5 = 40% なら -p のまま。
	const r2 = classify({ event: 'pull_request', files: ['crates/a/src/lib.rs'], graph: g });
	assert.equal(r2.jobs.rust_args, '-p a -p b');
	assert.equal(r2.jobs.run_e2e_hub, false);
	assert.equal(r2.crates.fallback, null);
});

test('graphFromMetadata は workspace の path 依存だけを normal・dev・build の別なく辺にする', () => {
	const meta = {
		workspace_root: 'D:\\ws',
		workspace_members: ['x', 'y', 'z'],
		packages: [
			{
				id: 'x',
				name: 'x',
				manifest_path: 'D:\\ws\\crates\\x\\Cargo.toml',
				dependencies: [
					{ name: 'serde', path: null, kind: null },
					{ name: 'outside', path: 'D:\\elsewhere\\outside', kind: null }
				]
			},
			{
				id: 'y',
				name: 'y',
				manifest_path: 'D:\\ws\\apps\\y\\core\\Cargo.toml',
				dependencies: [
					{ name: 'x', path: 'D:\\ws\\crates\\x', kind: 'dev' },
					{ name: 'x', path: 'D:\\ws\\crates\\x', kind: null, target: 'cfg(windows)' }
				]
			},
			{
				id: 'z',
				name: 'z',
				manifest_path: 'D:/ws/apps/z/Cargo.toml',
				dependencies: [{ name: 'y', rename: 'yy', path: 'D:/ws/apps/y/core/', kind: 'build' }]
			}
		]
	};
	assert.deepEqual(graphFromMetadata(meta), {
		packages: [
			{ name: 'x', dir: 'crates/x', deps: [] },
			{ name: 'y', dir: 'apps/y/core', deps: ['x'] },
			{ name: 'z', dir: 'apps/z', deps: ['y'] }
		]
	});
});

test('ファイルの持ち主はディレクトリの最長一致（前方一致の取り違えをしない）', () => {
	const g = {
		packages: [
			{ name: 'outer', dir: 'apps/hub', deps: [] },
			{ name: 'inner', dir: 'apps/hub/core', deps: [] },
			{ name: 'tags', dir: 'crates/banto-tags', deps: [] }
		]
	};
	assert.equal(owningPackage(g, 'apps/hub/core/src/lib.rs'), 'inner');
	assert.equal(owningPackage(g, 'apps/hub/src/x.rs'), 'outer');
	assert.equal(owningPackage(g, 'crates/banto-tagsx/src/lib.rs'), null);
	assert.equal(owningPackage(g, 'crates/README.md'), null);
});

test('ルートがパッケージでも、crates/ の下の非パッケージパスの持ち主にしない', () => {
	const g = {
		packages: [
			{ name: 'rootpkg', dir: '', deps: [] },
			{ name: 'tags', dir: 'crates/banto-tags', deps: [] }
		]
	};
	// crates/ の下でどのパッケージにも属さないパスは null（= 全体に倒れる）。
	assert.equal(owningPackage(g, 'crates/README.md'), null);
	assert.equal(owningPackage(g, 'crates/banto-tagsx/src/lib.rs'), null);
	assert.equal(owningPackage(g, 'crates/banto-tags/src/lib.rs'), 'tags');
	// crates/ の外のルート直下はルートのパッケージのもの。
	// （関数単体の挙動を固定するだけ。本番では resolveCrates に crates 領域のファイルしか
	// 渡らないので、この分岐は通らない。#647）
	assert.equal(owningPackage(g, 'src/main.rs'), 'rootpkg');
	// 判定全体でも rust_all に倒れる。
	const r = classify({ event: 'pull_request', files: ['crates/README.md'], graph: g });
	assert.equal(r.jobs.rust_args, '--workspace');
	assert.equal(r.areas.rust_all, true);
});

test('今のルートは仮想マニフェスト（ルートのパッケージが無い）', () => {
	assert.equal(
		real().packages.some((p) => p.dir === ''),
		false,
		'ルートがパッケージになった。owningPackage のルートの扱い（#640）を見直す'
	);
});

test('パスの正規化は classifyAreas と crates/ の検出で同じ（バックスラッシュ区切り・前後の空白）', () => {
	assert.equal(
		normalizePath('  crates\\banto-tags\\src\\lib.rs \r\n'),
		'crates/banto-tags/src/lib.rs'
	);
	for (const f of ['crates/x/a.rs', 'crates\\x\\a.rs', '  crates/x/a.rs\r', ' crates\\x\\a.rs ']) {
		assert.equal(touchesCrates([f]), true, JSON.stringify(f));
		assert.equal(classifyAreas([f]).matches[0].areas[0], 'crates', JSON.stringify(f));
	}
	for (const f of ['apps/crates/a.rs', 'docs/crates/a.md', 'cratesx/a.rs', '']) {
		assert.equal(touchesCrates([f]), false, JSON.stringify(f));
	}
	assert.equal(touchesCrates(['docs/a.md', ' crates\\x\\a.rs']), true);
});

test('固定のグラフ G は実グラフの一部（辺が実在する）', () => {
	const byName = new Map(real().packages.map((p) => [p.name, p]));
	for (const p of G.packages) {
		const r = byName.get(p.name);
		assert.ok(r, `${p.name} は実グラフに無い`);
		assert.equal(p.dir, r.dir, `${p.name} のディレクトリ`);
		for (const d of p.deps) assert.ok(r.deps.includes(d), `${p.name} → ${d} は実グラフに無い`);
	}
});

test('実グラフ: 主な crate の逆依存が期待のパッケージを含む', () => {
	const g = real();
	const want = {
		'banto-safe-root': [
			'banto-secret-store',
			'chronogazer-core',
			'chronogazer',
			'chronogazer-elev',
			'banto-hub-core',
			'banto-hub-sink'
		],
		'banto-secret-store': ['chronogazer-core', 'chronogazer'],
		'banto-instance-lock': [
			'chronogazer-core',
			'chronogazer',
			'chronogazer-elev',
			'banto-hub-core',
			'banto-hub-shell',
			'banto-hub-sink'
		],
		'banto-service-control': [
			'chronogazer-core',
			'chronogazer',
			'chronogazer-elev',
			'banto-hub-core',
			'banto-hub-shell',
			'banto-hub-sink'
		],
		// dev-dependency（banto-safe-root → banto-service-elev）を辿る。
		'banto-service-elev': [
			'banto-safe-root',
			'banto-secret-store',
			'chronogazer-core',
			'banto-hub-core'
		],
		// dev の辺（banto-hub-sink → banto-tags）も辿る。
		'banto-tags': ['banto-hub-core', 'banto-hub-sink', 'chronogazer-core']
	};
	for (const [crate, users] of Object.entries(want)) {
		const got = reverseDeps(g, [crate]);
		for (const u of users) assert.ok(got.has(u), `${crate} の逆依存に ${u} が無い`);
		assert.ok(got.size < g.packages.length, `${crate} の逆依存が workspace 全体になった`);
	}
});

test('実グラフ: banto-driver-api は依存ゼロ、conformance は API だけに依存し、変更すると driver-api ジョブが走る', () => {
	const g = real();
	const api = g.packages.find((p) => p.name === 'banto-driver-api');
	assert.ok(api, 'banto-driver-api が workspace に無い');
	assert.deepEqual(
		api.deps,
		[],
		'banto-driver-api に workspace の依存ができた（公開できなくなる）'
	);
	assert.ok(reverseDeps(g, ['banto-driver-api']).has('banto-plc'));
	// R1-2: 適合テスト一式も公開するので、workspace の依存は API だけ。
	const conformance = g.packages.find((p) => p.name === 'banto-driver-conformance');
	assert.ok(conformance, 'banto-driver-conformance が workspace に無い');
	assert.deepEqual(
		conformance.deps,
		['banto-driver-api'],
		'banto-driver-conformance に API 以外の workspace の依存ができた（公開できなくなる）'
	);
	assert.equal(
		classify({
			event: 'pull_request',
			files: ['crates/banto-driver-conformance/src/lib.rs'],
			graph: g
		}).jobs.run_driver_api,
		true
	);
	const jobs = classify({
		event: 'pull_request',
		files: ['crates/banto-driver-api/src/lib.rs'],
		graph: g
	}).jobs;
	assert.equal(jobs.run_driver_api, true);
	assert.equal(jobs.run_rust, true);
});

test('実グラフ: crates/ の下の追跡ファイルはすべてどれかのパッケージに属する', (t) => {
	let files;
	try {
		files = execFileSync('git', ['ls-files', 'crates'], { encoding: 'utf8', cwd: REPO_ROOT })
			.split('\n')
			.filter((f) => f !== '');
	} catch {
		t.skip('git ls-files が使えない');
		return;
	}
	const g = real();
	assert.deepEqual(
		files.filter((f) => owningPackage(g, f) === null),
		[]
	);
});

// 過去の実例（実グラフで引く）。今までの判定は 3 つとも workspace 全体だった。
const PR_622 = [
	'.github/scripts/ci-changes.mjs',
	'.github/scripts/ci-changes.test.mjs',
	'.github/workflows/ci.yml',
	'Cargo.lock',
	'Cargo.toml',
	'apps/banto-hub/core/Cargo.toml',
	'apps/banto-hub/core/src/bin/banto-hub-elev.rs',
	'apps/banto-hub/core/src/service_elevated.rs',
	'apps/chronogazer/README.md',
	'apps/chronogazer/core/src/bin/chronogazer_server/service.rs',
	'apps/chronogazer/core/src/server_service.rs',
	'apps/chronogazer/elev/Cargo.toml',
	'apps/chronogazer/elev/build.rs',
	'apps/chronogazer/elev/chronogazer-elev.manifest',
	'apps/chronogazer/elev/chronogazer-elev.rc',
	'apps/chronogazer/elev/src/lib.rs',
	'apps/chronogazer/elev/src/main.rs',
	'apps/chronogazer/elev/src/tests.rs',
	'apps/chronogazer/elev/src/win.rs',
	'crates/banto-secret-store/Cargo.toml',
	'crates/banto-secret-store/src/acl.rs',
	'crates/banto-service-control/src/install.rs',
	'docs/README.md',
	'docs/chronogazer-service-design.md'
];
const PR_637 = [
	'apps/chronogazer/elev/Cargo.toml',
	'apps/chronogazer/elev/src/lib.rs',
	'apps/chronogazer/elev/src/tests.rs',
	'apps/chronogazer/elev/src/win.rs',
	'crates/banto-secret-store/Cargo.toml',
	'crates/banto-secret-store/src/acl.rs',
	'crates/banto-secret-store/src/acl/guard.rs',
	'crates/banto-secret-store/src/acl/guard/tests.rs',
	'crates/banto-secret-store/src/acl/guard/win.rs',
	'crates/banto-secret-store/src/lib.rs',
	'crates/banto-service-elev/src/lib.rs',
	'crates/banto-service-elev/src/win.rs',
	'docs/chronogazer-service-design.md'
];

test('実例 #622: .github・root の Cargo.toml / Cargo.lock を含むので今までどおりフル', () => {
	assert.deepEqual(classify({ event: 'pull_request', files: PR_622, graph: real() }).jobs, ALL);
});

test('実例 #622 から .github・root の Cargo を除いた部分: 逆依存に絞る', () => {
	const files = PR_622.filter((f) => !/^(\.github\/|Cargo\.(toml|lock)$)/.test(f));
	const g = real();
	const jobs = classify({ event: 'pull_request', files, graph: g }).jobs;
	const pkgs = rustPackages(jobs.rust_args, g);
	for (const p of [
		'banto-secret-store',
		'banto-service-control',
		'banto-hub-core',
		'banto-hub-sink',
		'chronogazer-core',
		'chronogazer-elev'
	]) {
		assert.ok(pkgs.includes(p), `${p} が対象に無い`);
	}
	assert.ok(!pkgs.includes('banto-rtsp'));
	assert.ok(pkgs.length < g.packages.length);
	assert.equal(jobs.run_e2e_hub, true);
	assert.equal(jobs.run_e2e_chronogazer, true);
	assert.equal(jobs.run_rust_db_source, true);
	assert.equal(jobs.run_linux_clippy, true);
});

test('実例 #637（banto-secret-store・banto-service-elev・chronogazer-elev）: 逆依存に絞る', () => {
	const g = real();
	const jobs = classify({ event: 'pull_request', files: PR_637, graph: g }).jobs;
	const pkgs = rustPackages(jobs.rust_args, g);
	for (const p of [
		'banto-secret-store',
		'banto-service-elev',
		'chronogazer-core',
		'chronogazer-elev',
		'chronogazer',
		// banto-service-elev → banto-hub-core（昇格の部品）。
		'banto-hub-core',
		'banto-hub-shell'
	]) {
		assert.ok(pkgs.includes(p), `${p} が対象に無い`);
	}
	for (const p of ['banto-tags', 'banto-plc', 'banto-rtsp', 'banto-tsquery']) {
		assert.ok(!pkgs.includes(p), `${p} は対象に要らない`);
	}
	assert.equal(jobs.run_frontend_hub, false);
	assert.equal(jobs.run_frontend_chronogazer, true);
	assert.equal(jobs.run_e2e_hub, true);
	assert.equal(jobs.run_e2e_chronogazer, true);
});

test('実例: banto-instance-lock だけの変更は両アプリの core と、その逆依存', () => {
	const g = real();
	const jobs = classify({
		event: 'pull_request',
		files: ['crates/banto-instance-lock/src/lib.rs'],
		graph: g
	}).jobs;
	const pkgs = rustPackages(jobs.rust_args, g);
	assert.ok(pkgs.includes('banto-instance-lock'));
	assert.ok(pkgs.includes('chronogazer-core') && pkgs.includes('banto-hub-core'));
	assert.ok(!pkgs.includes('banto-tags') && !pkgs.includes('banto-secret-store'));
	assert.equal(jobs.run_frontend_hub || jobs.run_frontend_chronogazer, false);
	assert.equal(jobs.run_e2e_hub && jobs.run_e2e_chronogazer, true);
});

// `include_str!` / `include_bytes!` で別パッケージのファイルを読む箇所は Cargo の依存に
// 出ない。読まれる側のファイルが変わったとき、読む側のパッケージが Rust の対象に
// 入ることを確かめる（入らなければ RUST_PACKAGES に足す。今は banto-tagclient が
// banto-hub-core の stream.rs を読む 1 か所）。`concat!(env!("CARGO_MANIFEST_DIR"), ..)`
// や実行時の `fs::read` は見ていない。
test('パッケージをまたぐ include_str! / include_bytes! は読む側が対象に入る', (t) => {
	let files;
	try {
		files = execFileSync('git', ['ls-files', '--', 'crates/*.rs', 'apps/*.rs'], {
			encoding: 'utf8',
			cwd: REPO_ROOT
		})
			.split('\n')
			.filter((f) => f !== '');
	} catch {
		t.skip('git ls-files が使えない');
		return;
	}
	const g = real();
	const cross = [];
	for (const file of files) {
		const src = readFileSync(new URL(file, new URL('../../', import.meta.url)), 'utf8');
		for (const m of src.matchAll(/include_(?:str|bytes)!\s*\(\s*"([^"]+)"\s*\)/g)) {
			const target = posix.normalize(posix.join(posix.dirname(file), m[1]));
			const reader = owningPackage(g, file);
			const owner = owningPackage(g, target);
			if (!reader || !owner || reader === owner) continue;
			cross.push(`${reader} ← ${target}`);
			const jobs = classify({ event: 'pull_request', files: [target], graph: g }).jobs;
			assert.ok(
				rustPackages(jobs.rust_args, g).includes(reader),
				`${target} が変わっても ${reader}（${file} が読む）が対象に入らない`
			);
		}
	}
	// 走査が空振りしていないこと（既知の 1 か所を見つける）。
	assert.ok(cross.includes('banto-tagclient ← apps/banto-hub/core/src/stream.rs'), cross.join());
});

test('escapeRegExp は正規表現の特別な文字をすべて文字どおりにする', () => {
	// 特別な文字を全部並べても、エスケープ後は元の文字列にだけ一致する。
	const specials = '.*+?^${}()|[]\\';
	const re = new RegExp('^' + escapeRegExp(specials) + '$');
	assert.ok(re.test(specials));
	assert.ok(!re.test('x'));
	// 1 文字ずつでも、その文字だけに一致して、ほかの文字には一致しない。
	for (const ch of specials) {
		const one = new RegExp('^' + escapeRegExp(ch) + '$');
		assert.ok(one.test(ch), ch);
		assert.ok(!one.test('a'), ch);
	}
	// `crates/` のような特別でない文字はそのまま。ドットは任意の 1 文字にならない。
	assert.equal(escapeRegExp('crates/'), 'crates/');
	assert.ok(!new RegExp('^' + escapeRegExp('a.b') + '$').test('axb'));
});
