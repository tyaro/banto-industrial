/**
 * banto-hub タグページの性能計測ハーネス（T18-5a 第2段、
 * docs/banto-hub-t18-design.md §4 決定6「実測ファースト方針」）。
 *
 * 決定3（同 §4）の性能目標を、10,000 タグ・500 グループを seed した
 * 実サーバー・実 DOM に対して実測する:
 * - 初期表示: `/tags` へ遷移してから、グリッドの最初の行が描画され検索
 *   ボックスが操作可能になるまで 2 秒（3 回計測・中央値）
 * - 検索: 部分一致クエリを入力してから件数表示（`N / 10000 件`）が更新
 *   されるまで p95 100ms（20 回サンプリング）
 * - 連続登録 1,000 件（`MAX_CONTINUOUS_COUNT`、
 *   `$lib/banto/continuousRegistration.ts` 参照）の dry-run 検証・適用が
 *   それぞれ 5 秒以内
 *
 * **opt-in・CI 対象外**: `banto-hub-perf.playwright.config.ts` の doc
 * comment 参照。このファイル名 (`perf-*.spec.ts`) は本体 e2e の
 * `testMatch: 'banto-hub-*.spec.ts'`（`banto-hub.playwright.config.ts`）に
 * 一致しないため、通常の `pnpm e2e:banto-hub` では拾われない。
 *
 * **性能目標をハードアサートしない**: 実行環境（CPU/メモリ/ディスク）に
 * よって数値が大きく振れるため、目標未達は `expect` の失敗ではなく
 * `console.warn` の WARN 表示に留める。spec 自体は計測が完了すれば pass する
 * （オーナー決定 §4-6「実測が目標を満たせば…正式仕様として記録」の判断材料を
 * 集めるためのハーネスであって、CI ゲートではない）。
 *
 * 前提データは REST 直叩き（`simulation: true`、実 PLC 不要 - 収集は一度も
 * 開始しないため、シミュレーション接続でも実際にポーリングは走らない）。
 * `banto-hub-perf.playwright.config.ts` が使い捨ての専用 DB
 * （一時ディレクトリ、実行後に自動削除）で専用サーバーを起動するため、共有 DB
 * を 10,000 タグで汚さない。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from '../tests-banto-hub/banto-hub-auth';

const dirname = path.dirname(fileURLToPath(import.meta.url));
const RESULTS_DIR = path.resolve(dirname, '..', 'perf-results');

const TAG_COUNT = 10_000;
const GROUP_COUNT = 500;
const CONNECTION_COUNT = 20;
const TAGS_PER_GROUP = TAG_COUNT / GROUP_COUNT; // 20
// 1リクエストあたりの上限は実装 (T11-1、apps/banto-hub/core/src/rest.rs の
// `tags_batch`) 側に明示のハード上限は無いが、連続登録の実 UI 上限
// (`MAX_CONTINUOUS_COUNT`) と揃えておく - 1トランザクションの大きさを実運用の
// 最大単位に合わせ、かつチャンクごとの進捗が見えるようにする。
const SEED_BATCH_CHUNK = 1000;
// 連続登録の最大点数（$lib/banto/continuousRegistration.ts::MAX_CONTINUOUS_COUNT）。
const CONTINUOUS_COUNT = 1000;
const INITIAL_LOAD_SAMPLES = 3;
const SEARCH_SAMPLES = 20;
// 500グループ分の seed アドレス（ホールディングレジスタ参照番号 400001〜
// 410000、`modbusHoldingAddress` 参照）と衝突しない範囲を連続登録の開始
// アドレスに使う。
const CONTINUOUS_START_ADDRESS = '411001';
const CONTINUOUS_GROUP_NAME = 'perf-continuous-group';

/**
 * modbus-tcp 接続向けの参照番号アドレス（`crates/banto-plc/src/address.rs::
 * Address::parse` 参照）を組み立てる - 「先頭1桁がエリア（`4`=
 * holding register）＋残り5桁が1始まりの番号（1〜65536）」の6桁固定形式。
 * `D100` のような SLMP デバイス記法は modbus-tcp 接続では
 * `preflight_transaction`（`apps/banto-hub/core/src/rest.rs` の
 * `tags_batch`）のアドレス検証で 422 になる（実装調査で判明 - `banto-tags`
 * 側の `TagInput` バリデーションは空文字チェックのみで、プロトコルとの
 * 整合はここではなく `banto-plc::Address::parse` 側で行われる）。
 */
function modbusHoldingAddress(referenceNumber: number): string {
	return `4${String(referenceNumber).padStart(5, '0')}`;
}

// 決定3（docs/banto-hub-t18-design.md §4）の性能目標。WARN 表示にのみ使う
// （spec 冒頭の doc comment のとおりハードアサートしない）。
const TARGET_INITIAL_LOAD_MS = 2000;
const TARGET_SEARCH_P95_MS = 100;
const TARGET_CONTINUOUS_MS = 5000;

interface SeedTimings {
	connectionsMs: number;
	groupsMs: number;
	tagsMs: number;
	totalMs: number;
}

interface PerfResults {
	seed?: SeedTimings;
	initialLoad?: { samplesMs: number[]; medianMs: number };
	search?: { samplesMs: number[]; p50Ms: number; p95Ms: number };
	continuous?: { validateMs: number; applyMs: number; totalMs: number };
	env?: { platform: string; cpuModel: string; cpuCount: number; totalMemGiB: number };
	generatedAt?: string;
}

function median(values: number[]): number {
	const sorted = [...values].sort((a, b) => a - b);
	const mid = Math.floor(sorted.length / 2);
	return sorted.length % 2 !== 0 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** 最近傍ランク法（nearest-rank）。20サンプル程度の実測 p95 報告には十分。 */
function percentile(values: number[], p: number): number {
	const sorted = [...values].sort((a, b) => a - b);
	const idx = Math.min(Math.max(Math.ceil((p / 100) * sorted.length) - 1, 0), sorted.length - 1);
	return sorted[idx];
}

/** 固定並列数のシンプルなワーカープール - seed の REST 直叩きを直列より速く終わらせる。 */
async function mapWithConcurrency<T, R>(
	items: T[],
	concurrency: number,
	fn: (item: T, index: number) => Promise<R>
): Promise<R[]> {
	const results: R[] = new Array(items.length);
	let next = 0;
	async function worker(): Promise<void> {
		for (;;) {
			const i = next++;
			if (i >= items.length) return;
			results[i] = await fn(items[i], i);
		}
	}
	await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, () => worker()));
	return results;
}

/**
 * sqlite への同時書き込みが競合して一時的に失敗する可能性への保険（seed 専用
 * - 計測対象の3フェーズでは使わない）。数回まで短い間隔でリトライする。
 */
async function withRetry<T>(fn: () => Promise<T>, attempts = 3): Promise<T> {
	let lastErr: unknown;
	for (let i = 0; i < attempts; i++) {
		try {
			return await fn();
		} catch (err) {
			lastErr = err;
			if (i < attempts - 1) await new Promise((r) => setTimeout(r, 50 * (i + 1)));
		}
	}
	throw lastErr;
}

async function createConnection(
	request: APIRequestContext,
	headers: Record<string, string>,
	name: string
): Promise<{ id: number }> {
	return withRetry(async () => {
		const res = await request.post('/api/plc-connections', {
			headers,
			data: {
				name,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		if (!res.ok()) {
			throw new Error(`PLC接続作成に失敗 (${name}): ${res.status()} ${await res.text()}`);
		}
		return (await res.json()) as { id: number };
	});
}

async function createGroup(
	request: APIRequestContext,
	headers: Record<string, string>,
	name: string,
	plcConnectionId: number
): Promise<{ id: number }> {
	return withRetry(async () => {
		const res = await request.post('/api/collection-groups', {
			headers,
			data: { name, plcConnectionId, periodMs: 1000, enabled: true }
		});
		if (!res.ok()) {
			throw new Error(`収集グループ作成に失敗 (${name}): ${res.status()} ${await res.text()}`);
		}
		return (await res.json()) as { id: number };
	});
}

interface SeedTagPayload {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: string;
	enabled: boolean;
}

async function createTagsBatchChunk(
	request: APIRequestContext,
	headers: Record<string, string>,
	tags: SeedTagPayload[]
): Promise<number> {
	return withRetry(async () => {
		const res = await request.post('/api/tags/batch', { headers, data: { tags, dryRun: false } });
		if (!res.ok()) {
			throw new Error(`tags/batch がHTTPエラー: ${res.status()} ${await res.text()}`);
		}
		const body = (await res.json()) as { ok: boolean; count: number; errors: unknown[] };
		if (!body.ok) {
			throw new Error(`tags/batch の検証エラー: ${JSON.stringify(body.errors).slice(0, 2000)}`);
		}
		return body.count;
	});
}

test.describe.serial('banto-hub タグページ性能計測 (T18-5a 第2段, opt-in・CI対象外)', () => {
	let page: Page;
	let request: APIRequestContext;
	let headers: Record<string, string>;
	const seedTimings: SeedTimings = { connectionsMs: 0, groupsMs: 0, tagsMs: 0, totalMs: 0 };
	const results: PerfResults = {};

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		request = page.request;
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		const seedStart = Date.now();
		console.log('[perf] seed開始: PLC接続/収集グループ/タグ');

		// 1) シミュレーション接続（500グループをラウンドロビンで割り振る）。
		const connStart = Date.now();
		const connectionNames = Array.from({ length: CONNECTION_COUNT }, (_, i) => `perf-plc-${i}`);
		const connections = await mapWithConcurrency(connectionNames, 8, (name) =>
			createConnection(request, headers, name)
		);
		seedTimings.connectionsMs = Date.now() - connStart;

		// 2) 収集グループ500件。
		const groupStart = Date.now();
		const groupNames = Array.from({ length: GROUP_COUNT }, (_, i) => `perf-group-${i}`);
		const groups = await mapWithConcurrency(groupNames, 8, (name, i) =>
			createGroup(request, headers, name, connections[i % CONNECTION_COUNT].id)
		);
		seedTimings.groupsMs = Date.now() - groupStart;

		// 3) 連続登録1000件フェーズ専用の接続+グループ（既存500グループの
		//    アドレス範囲 400001〜410000 と衝突しない CONTINUOUS_START_ADDRESS
		//    〜を使う）。
		const continuousConnection = await createConnection(request, headers, 'perf-continuous-plc');
		await createGroup(request, headers, CONTINUOUS_GROUP_NAME, continuousConnection.id);

		// 4) タグ10,000件を /api/tags/batch へ SEED_BATCH_CHUNK(1000)件ずつ
		//    投入（500グループ×20件、アドレスは参照番号400001〜410000で
		//    グループをまたいでも重複しない連番）。
		const tagStart = Date.now();
		const allTags: SeedTagPayload[] = [];
		for (let g = 0; g < GROUP_COUNT; g++) {
			for (let t = 0; t < TAGS_PER_GROUP; t++) {
				const referenceNumber = g * TAGS_PER_GROUP + t + 1; // 1-based, 1..10000
				allTags.push({
					name: `perf-tag-${g}-${t}`,
					collectionGroupId: groups[g].id,
					address: modbusHoldingAddress(referenceNumber),
					dataType: 'i16',
					enabled: true
				});
			}
		}
		let createdCount = 0;
		for (let i = 0; i < allTags.length; i += SEED_BATCH_CHUNK) {
			const chunk = allTags.slice(i, i + SEED_BATCH_CHUNK);
			const count = await createTagsBatchChunk(request, headers, chunk);
			createdCount += count;
			console.log(
				`[perf] seed tags: ${Math.min(i + SEED_BATCH_CHUNK, allTags.length)}/${allTags.length}件投入済み`
			);
		}
		seedTimings.tagsMs = Date.now() - tagStart;
		seedTimings.totalMs = Date.now() - seedStart;

		expect(createdCount).toBe(TAG_COUNT);

		console.log(
			`[perf] seed完了: connections=${seedTimings.connectionsMs}ms groups=${seedTimings.groupsMs}ms ` +
				`tags=${seedTimings.tagsMs}ms total=${seedTimings.totalMs}ms`
		);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 初期表示: /tags遷移から最初の行描画+検索ボックス操作可能まで（3回計測）', async () => {
		const samples: number[] = [];
		for (let i = 0; i < INITIAL_LOAD_SAMPLES; i++) {
			const start = Date.now();
			await page.goto('/tags', { waitUntil: 'commit' });
			await expect(page.locator('[data-cell-row="0"]').first()).toBeVisible({ timeout: 30_000 });
			await expect(page.locator('.search-box')).toBeEnabled();
			const elapsed = Date.now() - start;
			samples.push(elapsed);
			console.log(`[perf] 初期表示 #${i + 1}: ${elapsed}ms`);
		}
		const medianMs = median(samples);
		results.initialLoad = { samplesMs: samples, medianMs };
		console.log(`[perf] 初期表示 median=${medianMs}ms（目標 ${TARGET_INITIAL_LOAD_MS}ms）`);
		if (medianMs > TARGET_INITIAL_LOAD_MS) {
			console.warn(
				`[perf][WARN] 初期表示 median ${medianMs}ms が目標 ${TARGET_INITIAL_LOAD_MS}ms を超過`
			);
		}
	});

	test('2. 検索: 部分一致クエリ→件数表示更新まで（20回サンプリング、p50/p95）', async () => {
		const search = page.locator('.search-box');
		// `.count` 単体だと ConnectionTree のグループ別件数バッジ
		// （`<span class="count">(20)</span>`、`.count` クラス共有）にも
		// マッチしてしまう（strict mode 違反、実 DOM 検証で発覚）ので、
		// ツールバー配下に絞る。
		const count = page.locator('.toolbar .count');

		await search.fill('');
		await expect(count).toHaveText(`${TAG_COUNT} / ${TAG_COUNT} 件`);

		const samples: number[] = [];
		for (let i = 0; i < SEARCH_SAMPLES; i++) {
			// グループ番号100〜119は3桁の一意なプレフィックスになる
			// （例: "perf-tag-100-" は "perf-tag-1000-" のような4桁グループが
			// 存在しないため他グループの行と衝突しない）ので、毎回ちょうど
			// TAGS_PER_GROUP件がヒットする・かつ入力ごとに検索語が変わる
			// クエリ列になる。
			const groupIndex = 100 + i;
			const query = `perf-tag-${groupIndex}-`;
			const expectedText = `${TAGS_PER_GROUP} / ${TAG_COUNT} 件`;

			// 直前のクエリと件数表示が同じ文字列に見えても実際には別クエリの
			// 再計算結果であることを保証するため、毎回いったん全件表示へ戻して
			// から次のクエリを打つ（同一文字列が既に表示されていると
			// `toHaveText` が再計算を待たずに即成立してしまい、計測値が
			// 不当に短くなる）。
			await search.fill('');
			await expect(count).toHaveText(`${TAG_COUNT} / ${TAG_COUNT} 件`);

			const start = Date.now();
			await search.fill(query);
			await expect(count).toHaveText(expectedText, { timeout: 5_000 });
			const elapsed = Date.now() - start;
			samples.push(elapsed);
		}

		const p50Ms = percentile(samples, 50);
		const p95Ms = percentile(samples, 95);
		results.search = { samplesMs: samples, p50Ms, p95Ms };
		console.log(`[perf] 検索 samples=[${samples.join(', ')}]ms`);
		console.log(`[perf] 検索 p50=${p50Ms}ms p95=${p95Ms}ms（目標 p95 ${TARGET_SEARCH_P95_MS}ms）`);
		if (p95Ms > TARGET_SEARCH_P95_MS) {
			console.warn(`[perf][WARN] 検索 p95 ${p95Ms}ms が目標 ${TARGET_SEARCH_P95_MS}ms を超過`);
		}
	});

	test('3. 連続登録1000件: dry-run検証・適用の所要時間', async () => {
		await page.getByRole('button', { name: '連続登録' }).click();
		const drawer = page.getByRole('dialog', { name: '連続登録' });
		await expect(drawer).toBeVisible();
		await drawer.getByLabel('対象グループ').selectOption({ label: CONTINUOUS_GROUP_NAME });
		await drawer.getByLabel('開始アドレス').fill(CONTINUOUS_START_ADDRESS);
		await drawer.getByLabel('点数').fill(String(CONTINUOUS_COUNT));
		await expect(
			page.getByRole('heading', { level: 4, name: `プレビュー（${CONTINUOUS_COUNT}件）` })
		).toBeVisible({ timeout: 15_000 });

		// `name: '登録'` は部分一致だと「新規登録」「連続登録」ボタンにも
		// マッチして strict mode 違反になる（実 DOM 検証で発覚）ので、
		// drawer 内に絞った上で `exact: true` を付ける。
		const validateStart = Date.now();
		await drawer.getByRole('button', { name: '検証', exact: true }).click();
		await expect(
			page.locator('.toast .message', { hasText: `検証OK: ${CONTINUOUS_COUNT}件登録できます` })
		).toBeVisible({ timeout: 30_000 });
		const validateMs = Date.now() - validateStart;

		const applyStart = Date.now();
		await drawer.getByRole('button', { name: '登録', exact: true }).click();
		await expect(
			page.locator('.toast .message', { hasText: `${CONTINUOUS_COUNT}件登録しました` })
		).toBeVisible({ timeout: 30_000 });
		const applyMs = Date.now() - applyStart;

		results.continuous = { validateMs, applyMs, totalMs: validateMs + applyMs };
		console.log(
			`[perf] 連続登録${CONTINUOUS_COUNT}件: 検証=${validateMs}ms 適用=${applyMs}ms ` +
				`合計=${validateMs + applyMs}ms（目標 ${TARGET_CONTINUOUS_MS}ms）`
		);
		if (validateMs > TARGET_CONTINUOUS_MS) {
			console.warn(
				`[perf][WARN] 連続登録 dry-run検証 ${validateMs}ms が目標 ${TARGET_CONTINUOUS_MS}ms を超過`
			);
		}
		if (applyMs > TARGET_CONTINUOUS_MS) {
			console.warn(
				`[perf][WARN] 連続登録 適用 ${applyMs}ms が目標 ${TARGET_CONTINUOUS_MS}ms を超過`
			);
		}
	});

	test('4. 結果サマリの出力', async () => {
		results.seed = seedTimings;
		results.env = {
			platform: `${os.platform()} ${os.release()}`,
			cpuModel: os.cpus()[0]?.model ?? 'unknown',
			cpuCount: os.cpus().length,
			totalMemGiB: Math.round((os.totalmem() / 1024 ** 3) * 10) / 10
		};
		results.generatedAt = new Date().toISOString();

		console.log('==================== T18-5a 性能計測結果 ====================');
		console.log(
			`実行環境: ${results.env.platform} / ${results.env.cpuModel} ` +
				`(論理コア${results.env.cpuCount}) / ${results.env.totalMemGiB}GiB`
		);
		console.log(
			'※ 決定3（docs/banto-hub-t18-design.md §4）の基準機（Intel Core i5 第11世代・' +
				'メモリ8〜16GB）と異なる実行環境の場合、この数値は参考値扱い。'
		);
		console.log(
			`seed: connections=${seedTimings.connectionsMs}ms groups=${seedTimings.groupsMs}ms ` +
				`tags=${seedTimings.tagsMs}ms total=${seedTimings.totalMs}ms`
		);
		if (results.initialLoad) {
			console.log(
				`初期表示: samples=[${results.initialLoad.samplesMs.join(', ')}]ms ` +
					`median=${results.initialLoad.medianMs}ms（目標 ${TARGET_INITIAL_LOAD_MS}ms）`
			);
		}
		if (results.search) {
			console.log(
				`検索: p50=${results.search.p50Ms}ms p95=${results.search.p95Ms}ms` +
					`（目標 p95 ${TARGET_SEARCH_P95_MS}ms）`
			);
		}
		if (results.continuous) {
			console.log(
				`連続登録${CONTINUOUS_COUNT}件: 検証=${results.continuous.validateMs}ms ` +
					`適用=${results.continuous.applyMs}ms 合計=${results.continuous.totalMs}ms` +
					`（目標 ${TARGET_CONTINUOUS_MS}ms）`
			);
		}
		console.log('===============================================================');

		fs.mkdirSync(RESULTS_DIR, { recursive: true });
		const outPath = path.join(RESULTS_DIR, `perf-tags-10k-${Date.now()}.json`);
		fs.writeFileSync(outPath, JSON.stringify(results, null, 2));
		console.log(`[perf] 結果JSONを書き出し: ${outPath}`);
	});
});
