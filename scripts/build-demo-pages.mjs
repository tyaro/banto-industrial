#!/usr/bin/env node
// ブラウザで触れるデモ（GitHub Pages）のサイトを組み立てる（docs/demo-pages-design.md §3.5）。
//
//   node scripts/build-demo-pages.mjs        （= pnpm demo:build）
//
// 1. 両アプリを `VITE_BANTO_DEMO=1`・`BASE_PATH=/banto-industrial/<app>` でビルドする。
//    出力は `target/demo-build/<app>/`（`DEMO_BUILD_DIR`。各アプリの `build/` は embed-ui が
//    埋め込む製品の画面なので上書きしない）。
// 2. 1 つのサイト `target/demo-pages/` に `hub/`・`chronogazer/` として並べる。Pages では
//    これが `https://<owner>.github.io/banto-industrial/` の下になる。
// 3. GitHub Pages は無いパスをサブディレクトリの `index.html` へ回さない（最上位の
//    `404.html` を返すだけ）ので、SPA の fallback の `index.html` を対象のルートごとに写す
//    （`hub/tags/index.html`・`chronogazer/tags/index.html`・`chronogazer/monitor/index.html`）。
// 4. 最上位に入口の `index.html`（2 つのアプリへのリンク）と、入口へのリンクだけを出す
//    `404.html`（対象外のパスを SPA として開かない）を置く。
//
// `target/` は git に入れない。
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const buildRoot = path.join(repoRoot, 'target', 'demo-build');
const siteRoot = path.join(repoRoot, 'target', 'demo-pages');

/** Pages のサイトのベースパス（リポジトリ名）。 */
export const SITE_BASE = '/banto-industrial';

/** 組み立てるアプリ。`routes` は fallback を写す対象のルート（直に開く・読み直す URL）。 */
export const DEMO_APPS = [
	{ pkg: 'banto-hub', dir: 'hub', title: 'banto-hub（タグ登録とモニタ）', routes: ['tags'] },
	{
		pkg: 'chronogazer',
		dir: 'chronogazer',
		title: 'ChronoGazer（タグ登録とトレンド）',
		routes: ['tags', 'monitor']
	}
];

/**
 * `command` をシェルで走らせる（Windows の `pnpm` は `.cmd` なのでシェルが要る）。引数は
 * この中の定数だけなので、1 本の文字列で渡す（引数の配列とシェルを併せると Node が警告する）。
 */
function run(command, env) {
	const result = spawnSync(command, {
		cwd: repoRoot,
		stdio: 'inherit',
		env: { ...process.env, ...env },
		shell: true
	});
	if (result.status !== 0) {
		throw new Error(`${command} が失敗しました（終了コード ${result.status}）`);
	}
}

const escapeHtml = (s) =>
	s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);

function page(title, body) {
	return `<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>${escapeHtml(title)}</title>
<style>
body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 3rem auto; padding: 0 1rem; line-height: 1.7; }
a { color: #1d4ed8; }
</style>
</head>
<body>
${body}
</body>
</html>
`;
}

function entryPage() {
	const items = DEMO_APPS.map((app) => {
		const links = app.routes
			.map((r) => `<a href="${SITE_BASE}/${app.dir}/${r}/">/${r}</a>`)
			.join('・');
		return `<li>${escapeHtml(app.title)}: ${links}</li>`;
	}).join('\n');
	return page(
		'banto-industrial のデモ',
		`<h1>banto-industrial のデモ</h1>
<p>インストールせずにブラウザで画面を触れるデモです。機器・サーバーにはつながりません。データはこのタブの中だけにあり、読み直すと初期に戻ります。</p>
<ul>
${items}
</ul>`
	);
}

function notFoundPage() {
	return page(
		'ページがありません — banto-industrial のデモ',
		`<h1>ページがありません</h1>
<p>デモで開けるのは入口から辿れる画面だけです。</p>
<p><a href="${SITE_BASE}/">デモの入口へ</a></p>`
	);
}

export function buildDemoPages() {
	fs.rmSync(siteRoot, { recursive: true, force: true });
	fs.mkdirSync(siteRoot, { recursive: true });
	for (const app of DEMO_APPS) {
		const out = path.join(buildRoot, app.dir);
		fs.rmSync(out, { recursive: true, force: true });
		run(`pnpm --filter ${app.pkg} build`, {
			VITE_BANTO_DEMO: '1',
			BASE_PATH: `${SITE_BASE}/${app.dir}`,
			DEMO_BUILD_DIR: out
		});
		const dest = path.join(siteRoot, app.dir);
		fs.cpSync(out, dest, { recursive: true });
		const fallback = path.join(dest, 'index.html');
		if (!fs.existsSync(fallback)) throw new Error(`${fallback} がありません（fallback）`);
		for (const route of app.routes) {
			const dir = path.join(dest, route);
			fs.mkdirSync(dir, { recursive: true });
			fs.copyFileSync(fallback, path.join(dir, 'index.html'));
		}
	}
	fs.writeFileSync(path.join(siteRoot, 'index.html'), entryPage());
	fs.writeFileSync(path.join(siteRoot, '404.html'), notFoundPage());
	// Actions の配信は Jekyll を通さないが、`_app/` を確実に配るために置いておく。
	fs.writeFileSync(path.join(siteRoot, '.nojekyll'), '');
	console.log(`デモのサイトを組み立てました: ${path.relative(repoRoot, siteRoot)}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	buildDemoPages();
}
