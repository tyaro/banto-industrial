/**
 * ChronoGazer の `/monitor` のデモ（docs/demo-pages-design.md §3.3・§6 の 5）。組み立てた成果物で、
 * 直に開くと表示グループのトレンドに履歴の線が描かれ、現在値のポーリングで線が伸び続ける。
 * その間、本物の `/api`・WebSocket・ほかのオリジンへの送信が 0 件で、模擬に口の無い `/api` も 0 件。
 */
import { expect, test } from '@playwright/test';
import { recordTraffic, SITE } from './helpers';

test.use({ viewport: { width: 1280, height: 720 } });

test('ChronoGazer /monitor: 直に開くとトレンドに線が描かれ、時間が進むと線が伸びる', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	const historyServed = page.waitForEvent('console', {
		predicate: (m) => m.text().startsWith('demo: 履歴を返しました'),
		timeout: 15000
	});
	await page.goto(`${SITE}/chronogazer/monitor`);
	await expect(page.getByRole('heading', { name: '監視', exact: true })).toBeVisible();
	await historyServed;

	const trend = page.getByRole('application', { name: 'プレス のトレンド' });
	await expect(trend).toBeVisible();
	// 履歴の系列（表示グループ「プレス」のペン 4 本）が線になる。「表示できる値はまだありません」は出ない。
	const lines = trend.locator('path.bi-trend__line');
	await expect(lines).not.toHaveCount(0);
	await expect(page.getByText('表示できる値はまだありません')).toHaveCount(0);
	// 各ペンの現在の範囲が凡例に出る（値が入った印）。
	await expect(page.getByText('press_pressure', { exact: true }).first()).toBeVisible();
	await expect(page.getByText(/\d+(\.\d+)?〜\d+(\.\d+)?\s*MPa/).first()).toBeVisible();

	// 現在値のポーリングで時間が進み、線の形が変わる。
	const before = await lines.first().getAttribute('d');
	await expect
		.poll(async () => lines.first().getAttribute('d'), { timeout: 15000 })
		.not.toBe(before);

	expect(traffic.unsupported).toEqual([]);
	await expect(page.getByRole('alert').filter({ hasText: /\S/ })).toHaveCount(0);
	expect(traffic.forbidden()).toEqual([]);
});
