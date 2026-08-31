/**
 * `banto-hub-perf.playwright.config.ts` が作る一時 SQLite ディレクトリ
 * （`fs.mkdtempSync` under `os.tmpdir()`）を実行後に消す -
 * `e2e/global-teardown-banto-hub.ts`（T18-1 本体 e2e）の同型コピーだが、読む
 * env 変数を `BANTO_HUB_PERF_E2E_DB_DIR` にして、本体 e2e
 * （`BANTO_HUB_E2E_DB_DIR`）/chronogazer（`BANTO_E2E_DB_DIR`）どちらの一時
 * ディレクトリも誤って消さないようにしている。
 *
 * T18-5a 第2段（docs/banto-hub-t18-design.md §4 決定6「実測ファースト」）の
 * 性能計測ハーネス専用 - 10,000 タグ・500 グループを seed するので、本体
 * e2e（`banto-hub.playwright.config.ts`）と DB を共有しない使い捨て DB に
 * している（共有 DB を汚さない、というオーナー指示）。
 */
import fs from 'node:fs';

export default function globalTeardown(): void {
	const dbDir = process.env.BANTO_HUB_PERF_E2E_DB_DIR;
	if (!dbDir) return;
	try {
		fs.rmSync(dbDir, { recursive: true, force: true });
	} catch {
		// best-effort only - 詳細は e2e/global-teardown.ts の同型コメント参照
		// (Windows で webServer の子プロセスが sqlite ファイルを掴んだままの
		// タイミングだと EPERM になりうるが、全体の成否には影響させない)。
	}
}
