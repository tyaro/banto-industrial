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
	// 模擬の応答を作った後に出る印。履歴は「系列 N・点 M」まで読み、現在値は返した回数を数える。
	let currentServed = 0;
	page.on('console', (m) => {
		if (m.text().startsWith('demo: 現在値を返しました')) currentServed += 1;
	});
	const historyServed = page.waitForEvent('console', {
		predicate: (m) => m.text().startsWith('demo: 履歴を返しました'),
		timeout: 15000
	});
	await page.goto(`${SITE}/chronogazer/monitor`);
	await expect(page.getByRole('heading', { name: '監視', exact: true })).toBeVisible();
	const served = (await historyServed).text().match(/系列 (\d+)・点 (\d+)/);
	// 履歴の応答が空でない（表示グループ「プレス」のペン 4 本ぶんの系列と点がある）。
	expect(Number(served?.[1])).toBeGreaterThan(0);
	expect(Number(served?.[2])).toBeGreaterThan(0);

	const trend = page.getByRole('application', { name: 'プレス のトレンド' });
	await expect(trend).toBeVisible();
	const lines = trend.locator('path.bi-trend__line');
	await expect(lines).not.toHaveCount(0);
	await expect(page.getByText('表示できる値はまだありません')).toHaveCount(0);
	// 各ペンの現在の範囲が凡例に出る（値が入った印）。
	await expect(page.getByText('press_pressure', { exact: true }).first()).toBeVisible();
	await expect(page.getByText(/\d+(\.\d+)?〜\d+(\.\d+)?\s*MPa/).first()).toBeVisible();

	// 履歴の描画: 既定の 10 分の窓のうち、開いた後の現在値では届かない過去の側（描画の領域の左 1/4）に線がある。
	// 起動して数秒の現在値だけなら、線は窓の右端の細い帯にしか出ない。
	const span = await lines.first().evaluate((el) => {
		const svg = el.ownerSVGElement ?? el.closest('svg');
		const plot = svg!.getBoundingClientRect();
		const line = el.getBoundingClientRect();
		return { leftRatio: (line.left - plot.left) / plot.width, widthRatio: line.width / plot.width };
	});
	expect(span.leftRatio).toBeLessThan(0.25);
	expect(span.widthRatio).toBeGreaterThan(0.5);

	// 現在値の更新: ポーリングが回り続け、模擬の現在値の応答が増える。
	const base = currentServed;
	await expect.poll(() => currentServed, { timeout: 15000 }).toBeGreaterThan(base + 1);

	// 現在値が線に反映される: 時間窓を 1 分に絞ると、追従中の窓の右端は時計とともに進む。画面が現在値を捨てて
	// 履歴だけで線を描いていると、線の右端は履歴の終わりで止まり、窓だけが進むので、待つほど線の右端と描画の
	// 領域の右端の間が開く（8 秒で窓の 13% 超）。現在値が書き足されていれば、右端は直近のポーリングに追い付いている。
	await page.locator('select:has(option[value="60"])').first().selectOption('60');
	await expect(lines).not.toHaveCount(0);
	await page.waitForTimeout(8000);
	const gapRatio = await lines.first().evaluate((el) => {
		const svg = el.ownerSVGElement ?? el.closest('svg');
		const plot = svg!.getBoundingClientRect();
		return (plot.right - el.getBoundingClientRect().right) / plot.width;
	});
	expect(gapRatio).toBeLessThan(0.05);

	expect(traffic.unsupported).toEqual([]);
	await expect(page.getByRole('alert').filter({ hasText: /\S/ })).toHaveCount(0);
	expect(traffic.forbidden()).toEqual([]);
});
