// CI の変更パス判定（#468）。`.github/workflows/ci.yml` の `changes` ジョブが
// 呼び、どの後続ジョブを走らせるかを GITHUB_OUTPUT に書き出す。
//
// 設計の要点:
// - 判定は純関数 `classify()` に閉じ込め、`ci-changes.test.mjs` で表として
//   総当たりする（`node --test .github/scripts/ci-changes.test.mjs`。CI の
//   `changes` ジョブも判定の前に毎回これを実行する）。
// - **判定の漏れは「必要なテストが走らない」側の失敗になる**ので、どの規則にも
//   当たらないパスはフル CI に倒す（`full`）。迷うものは実行する側に倒す。
// - 規則は上から順に見て、最初に当たったものを採る（具体的なものを先に書く）。
// - push（main）・PR 以外の起動・`.github/**` の変更は、呼び出し側または規則で
//   フル CI にする（main への push が常にフルなのは、Swatinem/rust-cache が main で
//   保存するキャッシュを workspace 全体のものに保つため。ci.yml の rust ジョブ参照）。
//
// 共有 crate（`crates/<name>/`）の変更は、依存グラフで絞る（2026-10-16 オーナー決定）:
// - 依存グラフは `cargo metadata --format-version 1 --no-deps --offline`（ネットワーク
//   不要）の `packages[].dependencies[].path` から、workspace の中の path 依存だけで
//   組む（`graphFromMetadata()`）。normal・dev・build の依存をすべて辺にする（dev は
//   テストが、build は build.rs が壊れうるため）。target 固有の依存も含む。
// - 変わったファイルが属するパッケージ（パッケージのディレクトリの最長一致）と、
//   それに推移的に依存する workspace のパッケージだけを `-p` で clippy / test する。
// - E2E・PostgreSQL・linux-clippy は、届いたパッケージが `JOB_PACKAGES` の表に
//   当たるときだけ走らせる（例: chronogazer-core に届けば e2e-chronogazer）。
// - 安全側: グラフが無い（`cargo metadata` の失敗）・`crates/` の下でどのパッケージ
//   にも属さないパス・届く先が workspace の `RDEPS_FULL_RATIO` 以上、のどれでも
//   従来どおり Rust workspace 全体 + 両 E2E（`rust_all`）に倒す。`proto/`・`.cargo/`・
//   `.config/`・root の Cargo.toml / Cargo.lock・toolchain も従来どおり `rust_all`。
// - **Cargo の依存に現れない結び付き**（`include_str!` で別パッケージのファイルを
//   読むなど）はグラフに出ない。今あるもの（banto-tagclient のテストが
//   banto-hub-core の stream.rs を読む）は RUST_PACKAGES.hub で拾っており、
//   `include_str!` / `include_bytes!` がパッケージをまたぐ箇所が判定で拾われて
//   いることはテストで確かめる。
//
// 将来 `apps/banto-scada` を足すとき（ci.yml 冒頭のコメントにも同じ手順）:
//   1. RULES に `^apps/banto-scada/` → `scada`（と E2E を足すなら
//      `e2e/tests-banto-scada/` 等 → `e2e_scada`）を足す。
//   2. `derive()` に `run_frontend_scada` / `run_e2e_scada` と
//      RUST_PACKAGES.scada（Cargo のパッケージ名）を足す。E2E が build する
//      パッケージを JOB_PACKAGES.run_e2e_scada に足す（`crates/scada-*` は
//      依存グラフで自動的に拾われる）。
//   3. ci-changes.test.mjs に「scada だけの変更」の行を足す。
//   4. ci.yml に frontend-scada / e2e-scada ジョブを足し、ci-gate の `needs`
//      と GATE_JOBS に加える。

import { execFileSync } from 'node:child_process';
import { appendFileSync, readFileSync } from 'node:fs';
import { posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 共有 crate の置き場の前置き（RULES・`touchesCrates`・`owningPackage` が共有する）。 */
export const CRATES_PREFIX = 'crates/';

/** 文字列を正規表現のリテラルとして使うためのエスケープ。 */
export const escapeRegExp = (/** @type {string} */ text) =>
	text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** 領域フラグ。`full` はすべてを走らせる。 */
export const AREAS = [
	'full',
	'docs',
	'rust_all',
	'crates',
	'frontend_all',
	'hub',
	'chronogazer',
	'hub_sink',
	'e2e_hub',
	'e2e_chronogazer'
];

/**
 * パス → 領域の規則（上から順、最初に当たったものを採る）。
 * どれにも当たらないパスは `full` 扱い（`classify()` 参照）。
 * @type {Array<{ re: RegExp, areas: string[], why: string }>}
 */
export const RULES = [
	// --- フル CI（ワークフロー・判定スクリプト自身・ビルド/リリース用スクリプト）
	{ re: /^\.github\//, areas: ['full'], why: 'CI 定義・判定スクリプト自身' },
	{ re: /^scripts\//, areas: ['full'], why: 'ビルド/テスト基盤のスクリプト' },
	{ re: /^\.gitattributes$/, areas: ['full'], why: '改行・属性（全ファイルに効く）' },

	// --- 共有 crate（依存グラフで、そのパッケージと逆依存に絞る。`classify()` 参照）
	{ re: new RegExp('^' + escapeRegExp(CRATES_PREFIX)), areas: ['crates'], why: '共有 crate' },

	// --- Rust workspace 全体
	// proto/** は banto-hub-core の build.rs が読む。.cargo/** は cargo 設定、
	// .config/** は cargo-nextest の設定（テストの並列・グループ）。
	{
		re: /^(proto|\.cargo|\.config)\//,
		areas: ['rust_all'],
		why: 'proto / cargo・nextest 設定'
	},
	{
		re: /^(Cargo\.toml|Cargo\.lock|rust-toolchain(\.toml)?|deny\.toml|\.?rustfmt\.toml|\.?clippy\.toml)$/,
		areas: ['rust_all'],
		why: 'workspace / toolchain / lint 設定'
	},

	// --- frontend 全体（root の依存・lint/format/TS 共通設定）
	{
		re: /^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|eslint\.config\.[cm]?js|\.prettierrc(\..+)?|\.prettierignore|tsconfig(\..+)?\.json|\.npmrc|\.nvmrc|\.node-version)$/,
		areas: ['frontend_all'],
		why: 'root の依存・lint/format/TS 共通設定'
	},

	// --- アプリ単位（banto-hub-sink を banto-hub より先に書く）
	{ re: /^apps\/banto-hub-sink\//, areas: ['hub_sink'], why: 'banto-hub-sink' },
	{ re: /^apps\/banto-hub\//, areas: ['hub'], why: 'banto-hub' },
	{ re: /^apps\/chronogazer\//, areas: ['chronogazer'], why: 'chronogazer' },

	// --- E2E 基盤（アプリ固有のものを先に、残りは両方）
	{
		re: /^e2e\/(tests-banto-hub(-perf)?\/|banto-hub[^/]*$|global-teardown-banto-hub[^/]*$)/,
		areas: ['e2e_hub'],
		why: 'banto-hub の E2E'
	},
	{
		re: /^e2e\/(tests(-public-viewer|-chronogazer-server)?\/|(public-viewer\.|chronogazer-server\.)?playwright\.config\.ts$|global-teardown\.ts$|chronogazer-e2e-run-dir\.ts$)/,
		areas: ['e2e_chronogazer'],
		why: 'chronogazer の E2E'
	},
	{ re: /^e2e\/README\.md$/, areas: ['docs'], why: 'E2E の説明文書' },
	{ re: /^e2e\//, areas: ['e2e_hub', 'e2e_chronogazer'], why: 'E2E 共通（tsconfig 等）' },

	// --- 文書・エージェント向け設定（lint/format だけ）
	{
		re: /^interop\/opcua\//,
		areas: ['docs'],
		why: 'OPC UA interop は専用 opcua-interop workflow で検証'
	},
	{ re: /^docs\//, areas: ['docs'], why: 'docs' },
	{ re: /^[^/]+\.md$/, areas: ['docs'], why: 'root の Markdown' },
	{ re: /^(LICENSE|\.gitignore)$/, areas: ['docs'], why: 'ライセンス・ignore' },
	{ re: /^\.(claude|cursor)\//, areas: ['docs'], why: 'エージェント向け設定（CI は読まない）' }
];

/**
 * Rust をアプリ単位に絞るときのパッケージ（Cargo の package 名）。
 * 依存する側（consumer）も含める:
 * - hub: banto-hub-sink は banto-hub-core を dev-dependency に持つ。
 *   crates/banto-tagclient のテスト（src/close.rs）は
 *   apps/banto-hub/core/src/stream.rs を include_str! で読む。
 */
export const RUST_PACKAGES = {
	hub: ['banto-hub-core', 'banto-hub-shell', 'banto-hub-sink', 'banto-tagclient'],
	chronogazer: ['chronogazer-core', 'chronogazer', 'chronogazer-elev'],
	hub_sink: ['banto-hub-sink']
};

/**
 * Rust の対象（`-p` に並ぶパッケージ）にこれらのどれかが入ったら走らせるジョブ。
 * 各ジョブが build / test するパッケージ（ci.yml の各ジョブの cargo の `-p`）。
 * - rust-db-source: `-p banto-hub-core`（db_source）と `-p banto-hub-sink`
 * - linux-clippy: `-p chronogazer-elev -p banto-hub-core --lib`、加えて
 *   `-p banto-instance-lock` の clippy・test（その段は Rust の対象にこの crate が
 *   入るときだけ。ci.yml の段の `if:`）と、同じく `-p banto-safe-root`
 * - e2e-hub: `-p banto-hub-core --bin banto-hub`
 * - e2e-chronogazer: `-p chronogazer-core`（banto-serve・dev_plc・chronogazer-server）
 * - driver-api: `-p banto-driver-api -p banto-driver-conformance`（MSRV の check・
 *   `--all-features` の clippy / test・cargo-semver-checks。R1-1・R1-2、
 *   docs/banto-driver-api-design.md §9.1）
 */
export const JOB_PACKAGES = {
	run_driver_api: ['banto-driver-api', 'banto-driver-conformance'],
	run_rust_db_source: ['banto-hub-core', 'banto-hub-sink'],
	run_linux_clippy: [
		'chronogazer-elev',
		'banto-hub-core',
		'banto-instance-lock',
		'banto-safe-root'
	],
	run_e2e_hub: ['banto-hub-core'],
	run_e2e_chronogazer: ['chronogazer-core']
};

/**
 * 共有 crate の変更が届くパッケージの数が workspace のこの割合以上なら、
 * `-p` を並べずに workspace 全体（`rust_all`）に倒す。全体とほとんど変わらない
 * 範囲を `-p` で流すと、main の全体キャッシュと feature の組み合わせがずれて
 * 作り直しが増えるだけになるため。
 *
 * 0.8 は暫定値（実測の根拠は無い）。分母は workspace 全体のパッケージ数で、今の
 * 最大でも約 50% しか届かないので、実質は保険にとどまる。`-p` を十数個並べるより
 * `--workspace` のほうがキャッシュで速いケースが出たら、実測して見直す（#640）。
 */
export const RDEPS_FULL_RATIO = 0.8;

/**
 * @typedef {{ packages: Array<{ name: string, dir: string, deps: string[] }> }} RustGraph
 *   workspace のパッケージ。`dir` はリポジトリ相対（`/` 区切り、末尾 `/` なし）、
 *   `deps` は workspace の中の path 依存のパッケージ名（normal・dev・build のすべて）。
 */

/**
 * 変更パスの正規化（前後の空白を落とし、`\` を `/` にする）。パスを規則に当てる
 * 側（`classifyAreas`）と、`crates/` に触れたかを見る側（`touchesCrates`）が
 * 同じ実装を使う。
 * @param {string} raw
 */
export const normalizePath = (raw) => raw.trim().replace(/\\/g, '/');

/**
 * 変更パスに `crates/` の下のものがあるか（依存グラフを読むかの判断）。
 * @param {string[]} files
 */
export function touchesCrates(files) {
	return files.some((f) => normalizePath(f).startsWith(CRATES_PREFIX));
}

const toSlash = (/** @type {string} */ p) => p.replace(/\\/g, '/').replace(/\/+$/, '');

/**
 * `cargo metadata --format-version 1 --no-deps` の JSON から依存グラフを組む（純関数）。
 * workspace の外を指す path 依存（無いはずだが）は辺にしない。
 * @param {any} meta
 * @returns {RustGraph}
 */
export function graphFromMetadata(meta) {
	const root = toSlash(meta.workspace_root);
	const members = new Set(meta.workspace_members);
	const pkgs = meta.packages.filter((/** @type {any} */ p) => members.has(p.id));
	/** @type {Map<string, string>} 絶対パス（`/` 区切り）→ パッケージ名 */
	const byDir = new Map(
		pkgs.map((/** @type {any} */ p) => [posix.dirname(toSlash(p.manifest_path)), p.name])
	);
	const rel = (/** @type {string} */ abs) => {
		if (abs === root) return '';
		if (!abs.startsWith(root + '/')) throw new Error(`workspace の外のパッケージ: ${abs}`);
		return abs.slice(root.length + 1);
	};
	return {
		packages: pkgs.map((/** @type {any} */ p) => ({
			name: p.name,
			dir: rel(posix.dirname(toSlash(p.manifest_path))),
			deps: [
				...new Set(
					p.dependencies
						.filter((/** @type {any} */ d) => d.path && byDir.has(toSlash(d.path)))
						.map((/** @type {any} */ d) => byDir.get(toSlash(d.path)))
				)
			].sort()
		}))
	};
}

/**
 * ファイルが属するパッケージ（ディレクトリの最長一致）。無ければ null。
 * ルート（`dir === ''`）がパッケージのときは、それ以外のパッケージに属さない
 * `crates/` の下のパスの持ち主にしない（`crates/README.md` を全体に倒す安全側が
 * 効かなくなるため）。今のルートは仮想マニフェストなので該当しない。
 * @param {RustGraph} graph
 * @param {string} file リポジトリ相対（`/` 区切り）
 */
export function owningPackage(graph, file) {
	let best = null;
	for (const p of graph.packages) {
		const inside =
			p.dir === ''
				? !file.startsWith(CRATES_PREFIX)
				: file === p.dir || file.startsWith(p.dir + '/');
		if (inside) {
			if (!best || p.dir.length > best.dir.length) best = p;
		}
	}
	return best ? best.name : null;
}

/**
 * `names` と、それに（推移的に）依存する workspace のパッケージ。
 * @param {RustGraph} graph
 * @param {Iterable<string>} names
 * @returns {Set<string>}
 */
export function reverseDeps(graph, names) {
	/** @type {Map<string, string[]>} 依存先 → 依存元 */
	const users = new Map();
	for (const p of graph.packages) {
		for (const d of p.deps) {
			if (!users.has(d)) users.set(d, []);
			users.get(d)?.push(p.name);
		}
	}
	const seen = new Set(names);
	const queue = [...seen];
	while (queue.length > 0) {
		const n = /** @type {string} */ (queue.pop());
		for (const u of users.get(n) ?? []) {
			if (!seen.has(u)) {
				seen.add(u);
				queue.push(u);
			}
		}
	}
	return seen;
}

/**
 * 変更パスの一覧から領域フラグを出す。
 * @param {string[]} files リポジトリ相対パス（`/` 区切り）
 * @returns {{ areas: Record<string, boolean>, matches: Array<{ file: string, areas: string[], why: string, package?: string }> }}
 */
export function classifyAreas(files) {
	/** @type {Record<string, boolean>} */
	const areas = Object.fromEntries(AREAS.map((a) => [a, false]));
	const matches = [];
	for (const raw of files) {
		const file = normalizePath(raw);
		if (file === '') continue;
		const rule = RULES.find((r) => r.re.test(file));
		const hit = rule
			? { file, areas: rule.areas, why: rule.why }
			: { file, areas: ['full'], why: 'どの規則にも当たらない（安全側でフル CI）' };
		for (const a of hit.areas) areas[a] = true;
		matches.push(hit);
	}
	return { areas, matches };
}

/**
 * `crates/` の変更を依存グラフでパッケージに直す。直せないものは `rust_all` に倒す
 * （`areas` と `matches` を書き換える）。
 * @param {Record<string, boolean>} areas
 * @param {Array<{ file: string, areas: string[], why: string, package?: string }>} matches
 * @param {RustGraph | null} graph
 * @returns {{ changed: string[], affected: string[], fallback: string | null }}
 */
function resolveCrates(areas, matches, graph) {
	/** @type {Set<string>} */
	const changed = new Set();
	/** @type {string | null} */
	let fallback = null;
	for (const m of matches) {
		if (!m.areas.includes('crates')) continue;
		const owner = graph ? owningPackage(graph, m.file) : null;
		if (owner) {
			m.package = owner;
			m.why = `共有 crate（${owner} と逆依存）`;
			changed.add(owner);
		} else {
			m.areas = ['rust_all'];
			m.why = graph
				? 'crates/ の下でどのパッケージにも属さない（安全側で全体）'
				: '依存グラフが無い（安全側で全体）';
			areas.rust_all = true;
			fallback ??= m.why;
		}
	}
	if (!graph || changed.size === 0) return { changed: [...changed], affected: [], fallback };
	const affected = reverseDeps(graph, changed);
	if (affected.size >= RDEPS_FULL_RATIO * graph.packages.length) {
		areas.rust_all = true;
		fallback ??= `共有 crate の変更が ${affected.size}/${graph.packages.length} パッケージに届く（${RDEPS_FULL_RATIO * 100}% 以上なので全体）`;
	}
	return { changed: [...changed].sort(), affected: [...affected].sort(), fallback };
}

/**
 * 領域フラグと、共有 crate の変更が届くパッケージから、各ジョブを走らせるか・
 * Rust の対象を決める。
 * @param {Record<string, boolean>} a
 * @param {string[]} [crateAffected] 共有 crate の変更が届くパッケージ
 */
export function derive(a, crateAffected = []) {
	const full = a.full;
	const rustWorkspace = full || a.rust_all;
	const packages = new Set(crateAffected);
	for (const key of ['hub', 'chronogazer', 'hub_sink']) {
		if (a[key]) for (const p of RUST_PACKAGES[key]) packages.add(p);
	}
	const hits = (/** @type {keyof typeof JOB_PACKAGES} */ job) =>
		JOB_PACKAGES[job].some((p) => packages.has(p));
	const runRust = rustWorkspace || packages.size > 0;
	return {
		// 判定したら必ず走る（repo 全体の ESLint + Prettier。docs の整形もここ）。
		run_lint_format: true,
		run_frontend_hub: full || a.frontend_all || a.hub,
		run_frontend_chronogazer: full || a.frontend_all || a.chronogazer,
		run_rust: runRust,
		rust_args: !runRust
			? ''
			: rustWorkspace
				? '--workspace'
				: [...packages]
						.sort()
						.map((p) => `-p ${p}`)
						.join(' '),
		run_driver_api: rustWorkspace || hits('run_driver_api'),
		run_rust_db_source: rustWorkspace || a.hub || a.hub_sink || hits('run_rust_db_source'),
		run_linux_clippy: rustWorkspace || hits('run_linux_clippy'),
		run_e2e_hub: rustWorkspace || a.frontend_all || a.hub || a.e2e_hub || hits('run_e2e_hub'),
		run_e2e_chronogazer:
			rustWorkspace ||
			a.frontend_all ||
			a.chronogazer ||
			a.e2e_chronogazer ||
			hits('run_e2e_chronogazer')
	};
}

/**
 * @param {{ event: string, files: string[] | null, graph?: RustGraph | null }} input
 *   `files` が null のとき（差分が取れなかった）はフル CI。`graph` は Rust の依存
 *   グラフ（`graphFromMetadata()`）。null なら `crates/` の変更は workspace 全体。
 */
export function classify({ event, files, graph = null }) {
	const forced =
		event !== 'pull_request'
			? `${event} イベント（PR 以外）はフル CI`
			: files === null
				? '差分を取得できなかった（安全側でフル CI）'
				: null;
	const { areas, matches } = classifyAreas(files ?? []);
	if (forced) areas.full = true;
	const crates = resolveCrates(areas, matches, graph);
	return { areas, matches, forced, crates, jobs: derive(areas, crates.affected) };
}

/**
 * workspace の依存グラフを `cargo metadata` から読む（ネットワークを使わない）。
 * @param {string} cwd リポジトリのルート
 * @returns {RustGraph}
 */
export function loadRustGraph(cwd) {
	const out = execFileSync(
		'cargo',
		['metadata', '--format-version', '1', '--no-deps', '--offline'],
		{ cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'inherit'] }
	);
	return graphFromMetadata(JSON.parse(out));
}

function main() {
	const event = process.env.CI_EVENT ?? '';
	const filesPath = process.env.CI_CHANGED_FILES;
	/** @type {string[] | null} */
	let files = null;
	if (filesPath && process.env.CI_DIFF_OK === 'true') {
		files = readFileSync(filesPath, 'utf8').split('\n');
	}
	// グラフは PR で crates/ に触れたときだけ読む（push はどのみちフル CI）。
	// 読めなければ null のまま = crates/ の変更は workspace 全体に倒れる。
	/** @type {RustGraph | null} */
	let graph = null;
	if (event === 'pull_request' && files && touchesCrates(files)) {
		try {
			graph = loadRustGraph(fileURLToPath(new URL('../../', import.meta.url)));
		} catch (e) {
			console.log(`::warning::cargo metadata に失敗したので crates/ の変更は全体で流す: ${e}`);
		}
	}
	const result = classify({ event, files, graph });

	const lines = [
		...AREAS.map((a) => `${a}=${result.areas[a]}`),
		...Object.entries(result.jobs).map(([k, v]) => `${k}=${v}`)
	];
	console.log(lines.join('\n'));
	if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, lines.join('\n') + '\n');

	const { changed, affected, fallback } = result.crates;
	const summary = [
		'## 変更パス判定（#468）',
		'',
		result.forced ? `**${result.forced}**` : `変更ファイル ${result.matches.length} 件`,
		'',
		...(changed.length > 0 || fallback
			? [
					`- 変わった共有 crate: ${changed.map((c) => `\`${c}\``).join(', ') || '(なし)'}`,
					`- 届くパッケージ（${affected.length}）: ${affected.map((c) => `\`${c}\``).join(', ') || '(なし)'}`,
					...(fallback ? [`- **全体に倒した理由**: ${fallback}`] : []),
					''
				]
			: []),
		'| 出力 | 値 |',
		'| --- | --- |',
		...lines.map((l) => {
			const [k, ...v] = l.split('=');
			return `| \`${k}\` | \`${v.join('=') || '(空)'}\` |`;
		}),
		'',
		'<details><summary>ファイルごとの判定</summary>',
		'',
		'| ファイル | 領域 | 理由 |',
		'| --- | --- | --- |',
		...result.matches.map((m) => `| \`${m.file}\` | ${m.areas.join(', ')} | ${m.why} |`),
		'',
		'</details>',
		''
	];
	if (process.env.GITHUB_STEP_SUMMARY) {
		appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary.join('\n'));
	}
}

if (process.argv[1] && resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1])) {
	main();
}
