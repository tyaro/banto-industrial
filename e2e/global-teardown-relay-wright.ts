/**
 * `relay-wright.playwright.config.ts` が作る一時 SQLite ディレクトリ
 * （`fs.mkdtempSync` under `os.tmpdir()`）を実行後に消す -
 * `e2e/global-teardown-banto-hub.ts` の同型コピーだが、読む env 変数を
 * `RELAY_WRIGHT_E2E_DB_DIR` にして chronogazer 側（`BANTO_E2E_DB_DIR`）・
 * banto-hub 側（`BANTO_HUB_E2E_DB_DIR`）の一時ディレクトリを誤って消さない
 * ようにしている。
 */
import fs from 'node:fs';

export default function globalTeardown(): void {
	const dbDir = process.env.RELAY_WRIGHT_E2E_DB_DIR;
	if (!dbDir) return;
	try {
		fs.rmSync(dbDir, { recursive: true, force: true });
	} catch {
		// best-effort only - 詳細は e2e/global-teardown.ts の同型コメント参照
		// (Windows で webServer の子プロセスが sqlite ファイルを掴んだままの
		// タイミングだと EPERM になりうるが、全体の pnpm e2e:relay-wright の
		// 成否には影響させない)。
	}
}
