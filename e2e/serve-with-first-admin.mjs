// Starts a banto server binary (`banto-serve` / `banto-hub`) for an e2e suite that needs an
// account: first `<bin> create-admin` on the fresh database, then the server itself.
//
//   node serve-with-first-admin.mjs <path to banto-serve>
//
// A new database has no account and needs no login, and `banto-serve` logs
// nobody in while that is so (ADR-0034 §5/§6) - it does not even start
// unless public viewing is on. The first administrator of a headless install
// is created offline, before the server starts (ADR-0034 §7); this is that
// step for the e2e databases (playwright.config.ts `webServer`).
//
// The account comes from the environment (`BANTO_E2E_ADMIN_USERNAME` /
// `_PASSWORD` / `_DISPLAY_NAME`, set by the config from e2e/first-admin.ts).
// The password goes to `create-admin` on standard input, never as an
// argument. Everything else in the environment (`BANTO_DB`, `PORT`, ...) is
// passed on to both runs unchanged.
import { spawn, spawnSync } from 'node:child_process';

const bin = process.argv[2];
const username = process.env.BANTO_E2E_ADMIN_USERNAME;
const password = process.env.BANTO_E2E_ADMIN_PASSWORD;
const displayName = process.env.BANTO_E2E_ADMIN_DISPLAY_NAME;
if (!bin || !username || !password || !displayName) {
	console.error(
		'serve-with-first-admin: usage: node serve-with-first-admin.mjs <banto-serve>, with BANTO_E2E_ADMIN_USERNAME / _PASSWORD / _DISPLAY_NAME set'
	);
	process.exit(2);
}

const created = spawnSync(
	bin,
	['create-admin', '--username', username, '--display-name', displayName],
	{ input: `${password}\n`, stdio: ['pipe', 'inherit', 'inherit'] }
);
if (created.error || created.status !== 0) {
	console.error(
		`serve-with-first-admin: create-admin failed (${created.error?.message ?? `exit ${created.status}`})`
	);
	process.exit(1);
}

const server = spawn(bin, [], { stdio: 'inherit' });
// Playwright stops the webServer by ending this process tree; a signal sent
// to this process alone is passed on.
for (const signal of ['SIGINT', 'SIGTERM']) {
	process.on(signal, () => server.kill(signal));
}
server.on('error', (err) => {
	console.error(`serve-with-first-admin: could not start banto-serve: ${err.message}`);
	process.exit(1);
});
server.on('exit', (code, signal) => {
	process.exit(code ?? (signal ? 1 : 0));
});
