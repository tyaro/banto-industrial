// `check-banto-optimize-deps.mjs` のテスト（`node --test scripts/check-banto-optimize-deps.test.mjs`）。
// 一時ディレクトリに偽の node_modules（実体 + symlink）と vite.config.ts を作って確かめる。

import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { checkApp, parseExclude } from './check-banto-optimize-deps.mjs';

const APP = 'apps/x';

/**
 * @param {Record<string, string[]>} pkgs パッケージ名 -> 置くファイル（src/ 配下）
 * @param {string | null} config vite.config.ts の中身（null なら作らない）
 * @param {boolean} install node_modules を作るか
 */
function fixture(pkgs, config, install = true) {
	const root = mkdtempSync(join(tmpdir(), 'banto-optdeps-'));
	const app = join(root, APP);
	mkdirSync(app, { recursive: true });
	if (install) {
		mkdirSync(join(app, 'node_modules', '@banto'), { recursive: true });
		for (const [name, files] of Object.entries(pkgs)) {
			// pnpm と同じく、node_modules/@banto/<name> は別の場所への symlink にする。
			const real = join(root, 'store', name);
			mkdirSync(join(real, 'src'), { recursive: true });
			for (const f of files) writeFileSync(join(real, 'src', f), '');
			symlinkSync(real, join(app, 'node_modules', '@banto', name), 'junction');
		}
	}
	if (config !== null) writeFileSync(join(app, 'vite.config.ts'), config);
	return root;
}

const cfg = (names) =>
	`export default { optimizeDeps: { exclude: [${names.map((n) => `'${n}'`).join(', ')}] } };`;

function run(pkgs, config, install) {
	const root = fixture(pkgs, config, install);
	try {
		return checkApp(root, APP);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

test('全部入っていれば ok（警告なし）', () => {
	const r = run(
		{ 'admin-core': ['a.svelte.ts'], forms: ['b.svelte.ts', 'c.svelte'] },
		cfg(['@banto/admin-core', '@banto/forms'])
	);
	assert.deepEqual(r, { errors: [], warnings: [] });
});

test('.svelte.ts を持つものが漏れていれば失敗し、アプリとパッケージ名を示す', () => {
	const r = run({ 'admin-core': ['a.svelte.ts'], forms: ['b.svelte.ts'] }, cfg(['@banto/forms']));
	assert.equal(r.errors.length, 1);
	assert.match(r.errors[0], /apps\/x/);
	assert.match(r.errors[0], /@banto\/admin-core/);
});

test('.svelte.ts を持たないのに exclude にあるものは警告のみ', () => {
	const r = run(
		{ 'admin-core': ['a.svelte.ts'], charts: ['Chart.svelte'], theme: ['index.css'] },
		cfg(['@banto/admin-core', '@banto/charts', '@banto/theme'])
	);
	assert.deepEqual(r.errors, []);
	assert.equal(r.warnings.length, 2);
	assert.match(r.warnings.join('\n'), /@banto\/charts/);
	assert.match(r.warnings.join('\n'), /@banto\/theme/);
});

test('.svelte だけのパッケージは exclude に無くても失敗しない', () => {
	const r = run({ charts: ['Chart.svelte'] }, cfg([]));
	assert.deepEqual(r, { errors: [], warnings: [] });
});

test('node_modules が無ければ失敗（install 前）', () => {
	const r = run({}, cfg([]), false);
	assert.equal(r.errors.length, 1);
	assert.match(r.errors[0], /pnpm install/);
});

test('exclude が読めない書式なら失敗（見逃さない）', () => {
	const r = run(
		{ 'admin-core': ['a.svelte.ts'] },
		'export default { optimizeDeps: { exclude: LIST } };'
	);
	assert.equal(r.errors.length, 1);
	assert.match(r.errors[0], /parseExclude/);
});

test('parseExclude: コメントアウトされた名前は数えず、両方の引用符を読む', () => {
	const src = `export default {
		optimizeDeps: {
			exclude: [
				'@banto/a', // 説明 http://example
				/* '@banto/b', */
				"@banto/c"
				// '@banto/d'
			]
		}
	};`;
	assert.deepEqual(parseExclude(src), ['@banto/a', '@banto/c']);
});

test('parseExclude: optimizeDeps が無ければ null', () => {
	assert.equal(parseExclude('export default {};'), null);
});
