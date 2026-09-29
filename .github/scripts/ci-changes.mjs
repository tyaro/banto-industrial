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
//   フル CI にする。
//
// 将来 `apps/banto-scada` を足すとき（ci.yml 冒頭のコメントにも同じ手順）:
//   1. RULES に `^apps/banto-scada/` → `scada`（と E2E を足すなら
//      `e2e/tests-banto-scada/` 等 → `e2e_scada`）を足す。
//   2. `derive()` に `run_frontend_scada` / `run_e2e_scada` と
//      RUST_PACKAGES.scada（Cargo のパッケージ名）を足す。
//   3. ci-changes.test.mjs に「scada だけの変更」の行を足す。
//   4. ci.yml に frontend-scada / e2e-scada ジョブを足し、ci-gate の `needs`
//      と GATE_JOBS に加える。

import { appendFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 領域フラグ。`full` はすべてを走らせる。 */
export const AREAS = [
	'full',
	'docs',
	'rust_all',
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

	// --- Rust workspace 全体
	// crates/** は両アプリが依存する（当面は依存グラフを解かず全体 + 両 E2E）。
	// proto/** は banto-hub-core の build.rs が読む。.cargo/** は cargo 設定。
	{ re: /^(crates|proto|\.cargo)\//, areas: ['rust_all'], why: '共有 crate / proto / cargo 設定' },
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
		re: /^e2e\/(tests\/|playwright\.config\.ts$|global-teardown\.ts$|chronogazer-e2e-run-dir\.ts$)/,
		areas: ['e2e_chronogazer'],
		why: 'chronogazer の E2E'
	},
	{ re: /^e2e\/README\.md$/, areas: ['docs'], why: 'E2E の説明文書' },
	{ re: /^e2e\//, areas: ['e2e_hub', 'e2e_chronogazer'], why: 'E2E 共通（tsconfig 等）' },

	// --- 文書・エージェント向け設定（lint/format だけ）
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
	chronogazer: ['chronogazer-core', 'chronogazer'],
	hub_sink: ['banto-hub-sink']
};

/**
 * 変更パスの一覧から領域フラグを出す。
 * @param {string[]} files リポジトリ相対パス（`/` 区切り）
 * @returns {{ areas: Record<string, boolean>, matches: Array<{ file: string, areas: string[], why: string }> }}
 */
export function classifyAreas(files) {
	/** @type {Record<string, boolean>} */
	const areas = Object.fromEntries(AREAS.map((a) => [a, false]));
	const matches = [];
	for (const raw of files) {
		const file = raw.trim().replace(/\\/g, '/');
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
 * 領域フラグから、各ジョブを走らせるか・Rust の対象を決める。
 * @param {Record<string, boolean>} a
 */
export function derive(a) {
	const full = a.full;
	const rustWorkspace = full || a.rust_all;
	const packages = new Set();
	for (const key of ['hub', 'chronogazer', 'hub_sink']) {
		if (a[key]) for (const p of RUST_PACKAGES[key]) packages.add(p);
	}
	const runRust = rustWorkspace || packages.size > 0;
	return {
		// 判定したら必ず走る（repo 全体の ESLint + Prettier。docs の整形もここ）。
		run_lint_format: true,
		// apps/banto-hub/src/lib/blockCache.sync.test.ts が chronogazer の
		// blockCache.ts を読むので、chronogazer の変更でも hub の frontend を回す。
		run_frontend_hub: full || a.frontend_all || a.hub || a.chronogazer,
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
		run_rust_db_source: full || a.rust_all || a.hub || a.hub_sink,
		run_e2e_hub: full || a.rust_all || a.frontend_all || a.hub || a.e2e_hub,
		run_e2e_chronogazer: full || a.rust_all || a.frontend_all || a.chronogazer || a.e2e_chronogazer
	};
}

/**
 * @param {{ event: string, files: string[] | null }} input
 *   `files` が null のとき（差分が取れなかった）はフル CI。
 */
export function classify({ event, files }) {
	const forced =
		event !== 'pull_request'
			? `${event} イベント（PR 以外）はフル CI`
			: files === null
				? '差分を取得できなかった（安全側でフル CI）'
				: null;
	const { areas, matches } = classifyAreas(files ?? []);
	if (forced) areas.full = true;
	return { areas, matches, forced, jobs: derive(areas) };
}

function main() {
	const event = process.env.CI_EVENT ?? '';
	const filesPath = process.env.CI_CHANGED_FILES;
	/** @type {string[] | null} */
	let files = null;
	if (filesPath && process.env.CI_DIFF_OK === 'true') {
		files = readFileSync(filesPath, 'utf8').split('\n');
	}
	const result = classify({ event, files });

	const lines = [
		...AREAS.map((a) => `${a}=${result.areas[a]}`),
		...Object.entries(result.jobs).map(([k, v]) => `${k}=${v}`)
	];
	console.log(lines.join('\n'));
	if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, lines.join('\n') + '\n');

	const summary = [
		'## 変更パス判定（#468）',
		'',
		result.forced ? `**${result.forced}**` : `変更ファイル ${result.matches.length} 件`,
		'',
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
