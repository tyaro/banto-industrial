#!/usr/bin/env node
// デモのサイト（`pnpm demo:build` の `target/demo-pages/`）を、GitHub Pages の振る舞いを写した
// fallback の無い静的なサーバーで配る（docs/demo-pages-design.md §3.5 の確かめ方）。
//
//   node e2e/demo-pages-server.mjs [port]     （既定 8808）
//
// - `/banto-industrial/` の下を `target/demo-pages/` から配る。
// - ディレクトリは末尾 `/` へ 301（Pages の `/hub/tags` → `/hub/tags/`）、末尾 `/` なら
//   その `index.html`。
// - 無いパス（`/banto-industrial/` の外を含む）は、最上位の `404.html` を 404 で返す。
//   サブディレクトリの `index.html` へは回さない（SPA の fallback をしない）。
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const siteRoot = path.join(repoRoot, 'target', 'demo-pages');
const SITE_BASE = '/banto-industrial';
const port = Number(process.argv[2] ?? process.env.DEMO_PAGES_PORT ?? 8808);

const TYPES = {
	'.html': 'text/html; charset=utf-8',
	'.js': 'text/javascript; charset=utf-8',
	'.mjs': 'text/javascript; charset=utf-8',
	'.css': 'text/css; charset=utf-8',
	'.json': 'application/json',
	'.webmanifest': 'application/manifest+json',
	'.map': 'application/json',
	'.svg': 'image/svg+xml',
	'.png': 'image/png',
	'.jpg': 'image/jpeg',
	'.jpeg': 'image/jpeg',
	'.gif': 'image/gif',
	'.ico': 'image/x-icon',
	'.webp': 'image/webp',
	'.woff': 'font/woff',
	'.woff2': 'font/woff2',
	'.ttf': 'font/ttf',
	'.wasm': 'application/wasm',
	'.txt': 'text/plain; charset=utf-8'
};

function send(res, status, file, method) {
	const type = TYPES[path.extname(file).toLowerCase()] ?? 'application/octet-stream';
	const body = fs.readFileSync(file);
	res.writeHead(status, { 'content-type': type, 'content-length': body.length });
	res.end(method === 'HEAD' ? undefined : body);
}

function notFound(res, method) {
	const page = path.join(siteRoot, '404.html');
	if (fs.existsSync(page)) return send(res, 404, page, method);
	res.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
	res.end(method === 'HEAD' ? undefined : 'Not Found');
}

const server = http.createServer((req, res) => {
	const method = req.method ?? 'GET';
	if (method !== 'GET' && method !== 'HEAD') {
		res.writeHead(405, { allow: 'GET, HEAD' });
		return res.end();
	}
	const url = new URL(req.url ?? '/', 'http://localhost');
	let pathname;
	try {
		pathname = decodeURIComponent(url.pathname);
	} catch {
		return notFound(res, method);
	}
	if (pathname === SITE_BASE) {
		res.writeHead(301, { location: `${SITE_BASE}/${url.search}` });
		return res.end();
	}
	if (!pathname.startsWith(`${SITE_BASE}/`)) return notFound(res, method);
	const rel = pathname.slice(SITE_BASE.length + 1);
	const file = path.resolve(siteRoot, rel);
	if (file !== siteRoot && !file.startsWith(siteRoot + path.sep)) return notFound(res, method);
	let stat;
	try {
		stat = fs.statSync(file);
	} catch {
		return notFound(res, method);
	}
	if (stat.isDirectory()) {
		if (!pathname.endsWith('/')) {
			res.writeHead(301, { location: `${url.pathname}/${url.search}` });
			return res.end();
		}
		const index = path.join(file, 'index.html');
		return fs.existsSync(index) ? send(res, 200, index, method) : notFound(res, method);
	}
	return send(res, 200, file, method);
});

server.listen(port, '127.0.0.1', () => {
	console.log(`demo-pages: http://127.0.0.1:${port}${SITE_BASE}/ （${siteRoot}）`);
});
