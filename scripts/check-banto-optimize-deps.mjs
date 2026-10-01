// `@banto/*` の `optimizeDeps.exclude` の漏れを静的に検査する（#478）。
//
// 背景: `@banto/*` はソース配布で、`.svelte.ts`（runes モジュール）を生のまま
// 出荷する。Git 依存で入れた派生アプリは、これらが node_modules の実体になり、
// `pnpm dev` の Vite 依存オプティマイザ（prebundle）が `.svelte.ts` を TS 専用構文で
// js_parse_error にして 500 になる（tyaro/banto#150、banto ADR-0007）。**build では
// 壊れない**うえ、E2E は静的 build を配信するので、dev 起動の経路は CI のどこも
// 通らない。`.svelte.ts` を持つ `@banto/*` が増えたのに各アプリの
// `vite.config.ts` の `optimizeDeps.exclude` が追いつかないと、CI は緑のまま
// 開発者の `pnpm dev` だけが壊れる。それを install 後に静的に検出する。
//
// 検査:
// - 各アプリの `node_modules/@banto/*`（symlink は実体へ解決）を走査し、`src/` 等の下に
//   `.svelte.ts` を持つパッケージを洗い出す。それが `optimizeDeps.exclude` に
//   無ければ失敗（exit 1）。
// - `.svelte.ts` を持たないのに exclude に入っているものは警告のみ。ADR-0007 の
//   不変条件は「`.svelte.ts` を持つものは必ず列挙」で、`.svelte` コンポーネントだけの
//   パッケージ（charts 等）は preprocess 経路を通るので ADR 上は対象外だが、
//   ソース配布の `.svelte` を持つものをまとめて exclude しておくのは害が無い
//   （dev で個別配信になり cold start がやや遅くなるだけ）。意図的なら問題ない。
// - `node_modules` が無い（install 前）なら失敗。
//
// 使い方: `node scripts/check-banto-optimize-deps.mjs [--root <repo>] [<appDir>...]`
// （appDir 既定は apps/banto-hub と apps/chronogazer。Node 標準ライブラリのみ）。
// `vite.config.ts` は実行せず、コメントを除いて `optimizeDeps` 内の `exclude: [...]`
// の文字列リテラルを読む（式や変数経由の指定は読めないので、その場合は失敗にして
// 気付けるようにする）。

import { existsSync, readdirSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const DEFAULT_APPS = ['apps/banto-hub', 'apps/chronogazer'];

/** `.svelte.ts` を含むか（パッケージ実体の下を再帰、入れ子の node_modules は見ない）。 */
export function hasFileWithSuffix(dir, suffix) {
	for (const ent of readdirSync(dir, { withFileTypes: true })) {
		if (ent.name === 'node_modules') continue;
		const p = join(dir, ent.name);
		if (ent.isDirectory()) {
			if (hasFileWithSuffix(p, suffix)) return true;
		} else if (ent.name.endsWith(suffix)) {
			return true;
		}
	}
	return false;
}

/** `vite.config.ts` のソースから `optimizeDeps.exclude` の文字列配列を返す。読めなければ null。 */
export function parseExclude(source) {
	// コメントを落とす（文字列中の // は URL 等で現れうるので、文字列は温存する）。
	const stripped = source.replace(
		/("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`)|\/\*[\s\S]*?\*\/|\/\/[^\n]*/g,
		(m, str) => str ?? ''
	);
	const od = /optimizeDeps\s*:\s*\{/.exec(stripped);
	if (!od) return null;
	const rest = stripped.slice(od.index + od[0].length);
	const ex = /exclude\s*:\s*\[([^\]]*)\]/.exec(rest);
	if (!ex) return null;
	const items = [];
	const lit = /'((?:\\.|[^'\\])*)'|"((?:\\.|[^"\\])*)"/g;
	let m;
	while ((m = lit.exec(ex[1])) !== null) items.push(m[1] ?? m[2]);
	// 文字列リテラル以外（スプレッド・変数）が混ざっていれば読み切れていない。
	const residue = ex[1].replace(lit, '').replace(/[\s,]/g, '');
	return residue === '' ? items : null;
}

/**
 * 1 アプリを検査する。
 * @returns {{ errors: string[], warnings: string[] }}
 */
export function checkApp(root, appDir) {
	const errors = [];
	const warnings = [];
	const app = resolve(root, appDir);
	const banto = join(app, 'node_modules', '@banto');
	if (!existsSync(banto)) {
		errors.push(
			`${appDir}: node_modules/@banto が無い（pnpm install が済んでいない？）。` +
				`install 後に実行すること`
		);
		return { errors, warnings };
	}
	const configPath = join(app, 'vite.config.ts');
	if (!existsSync(configPath)) {
		errors.push(`${appDir}: vite.config.ts が無い`);
		return { errors, warnings };
	}

	/** @type {Map<string, boolean>} パッケージ名 -> .svelte.ts を持つか */
	const pkgs = new Map();
	for (const name of readdirSync(banto).sort()) {
		let real;
		try {
			real = realpathSync(join(banto, name));
		} catch {
			continue;
		}
		if (!statSync(real).isDirectory()) continue;
		pkgs.set(`@banto/${name}`, hasFileWithSuffix(real, '.svelte.ts'));
	}

	const exclude = parseExclude(readFileSync(configPath, 'utf8'));
	if (exclude === null) {
		errors.push(
			`${appDir}/vite.config.ts: optimizeDeps.exclude を文字列リテラルの配列として読めない。` +
				`書式を変えたならこのスクリプトの parseExclude も合わせること`
		);
		return { errors, warnings };
	}
	const excluded = new Set(exclude);

	for (const [name, hasSvelteTs] of pkgs) {
		if (hasSvelteTs && !excluded.has(name)) {
			errors.push(
				`${appDir}: ${name} は .svelte.ts を含むのに vite.config.ts の optimizeDeps.exclude に無い` +
					`（pnpm dev で js_parse_error になる。banto#150 / ADR-0007）`
			);
		}
	}
	for (const name of exclude) {
		if (name.startsWith('@banto/') && pkgs.get(name) === false) {
			warnings.push(
				`${appDir}: ${name} は .svelte.ts を持たないが optimizeDeps.exclude に入っている。` +
					`ソース配布の .svelte をまとめて exclude しているだけなら意図どおりで問題ない` +
					`（ADR-0007 は .svelte のみのパッケージを対象外とするが、exclude しても害は無い）`
			);
		} else if (name.startsWith('@banto/') && !pkgs.has(name)) {
			warnings.push(`${appDir}: ${name} は optimizeDeps.exclude にあるが node_modules に無い`);
		}
	}
	return { errors, warnings };
}

export function main(argv) {
	let root = resolve(fileURLToPath(import.meta.url), '..', '..');
	const apps = [];
	for (let i = 0; i < argv.length; i++) {
		if (argv[i] === '--root') root = resolve(argv[++i]);
		else apps.push(argv[i]);
	}
	let failed = false;
	for (const appDir of apps.length > 0 ? apps : DEFAULT_APPS) {
		const { errors, warnings } = checkApp(root, appDir);
		for (const w of warnings) console.warn(`警告: ${w}`);
		for (const e of errors) console.error(`エラー: ${e}`);
		if (errors.length > 0) failed = true;
		else console.log(`ok: ${appDir}`);
	}
	return failed ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	process.exit(main(process.argv.slice(2)));
}
