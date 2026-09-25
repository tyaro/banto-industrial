/**
 * `blockCache.ts` が chronogazer の同名ファイルの**複製のまま**であることの
 * 確認（#428）。アプリ間で共有するパッケージがまだ無いので、`/audit-log` の
 * ブロックキャッシュは両アプリに同じものを置いている。片方だけ直すと、
 * 片方の画面だけ #409 / #410 / #428 の欠陥に戻りうるので、先頭のモジュール
 * コメント（どちらのアプリのものかを書く場所）以外が一致することを固定する。
 *
 * 直すときは両方を同じに直す。chronogazer 側のテスト（`eventBlocks.test.ts`・
 * `auditBlocks.test.ts`）と banto-hub 側のテスト（`auditBlocks.test.ts`）の
 * 両方が、同じ実装に対して回ることになる。
 */
import { describe, expect, it } from 'vitest';

/**
 * `node:fs` の `readFileSync`。`import … from 'node:fs'` にしないのは、この
 * アプリに Node の型（`@types/node`）が無く svelte-check が通らないため
 * （`?raw` の import は、読む側のアプリの `.svelte-kit/tsconfig.json` が
 * 無いと変換で落ちる）。`process.getBuiltinModule` は Node 22.3 以降にあり、
 * このリポジトリは Node 24 以上（ルートの `engines`）。
 */
const { readFileSync } = (
	globalThis as unknown as {
		process: {
			getBuiltinModule(id: 'node:fs'): {
				readFileSync(file: URL, encoding: 'utf8'): string;
			};
		};
	}
).process.getBuiltinModule('node:fs');

/** 比べる 2 つのファイル（vitest はこのアプリのディレクトリで走る）。 */
const HUB = new URL('./blockCache.ts', import.meta.url);
const CHRONOGAZER = new URL('../../../chronogazer/src/lib/blockCache.ts', import.meta.url);

/** 改行を揃え、先頭のモジュールコメント（`/** … *\/`）を外した本体。 */
function body(file: URL): string {
	const text = readFileSync(file, 'utf8').replace(/\r\n/g, '\n');
	const match = /^\/\*\*[\s\S]*?\*\/\n/.exec(text);
	if (!match) throw new Error(`${file.pathname} の先頭にモジュールコメントがありません`);
	return text.slice(match[0].length);
}

describe('blockCache.ts（chronogazer との複製）', () => {
	it('先頭のモジュールコメント以外は chronogazer 側と一致する', () => {
		const hub = body(HUB);
		expect(hub.length).toBeGreaterThan(1000);
		expect(hub).toBe(body(CHRONOGAZER));
	});
});
