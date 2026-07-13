/**
 * Deterministic sample data for the items resource (spec §4, §8.1 items
 * page). 10,000 rows generated from a seeded PRNG so the dataset is stable
 * across reloads/tests without shipping a static 10k-row fixture.
 *
 * Moved here from the old routes/(app)/items/data.ts stub in M2: the rows
 * now seed an InMemoryDataProvider (src/lib/banto/setup.ts) instead of being
 * imported directly by the items pages.
 */
export interface Item {
	id: number;
	name: string;
	price: number;
	stock: number;
	updatedAt: string;
	// Index signature so `Item[]` satisfies @banto/admin-core's
	// `InMemorySeed.rows: Record<string, unknown>[]`.
	[key: string]: unknown;
}

const ROW_COUNT = 10_000;
const SEED = 0x8a17c05; // fixed seed: deterministic output across reloads/tests

/** mulberry32: small, fast, deterministic PRNG (no runtime dependency). */
function mulberry32(seed: number): () => number {
	let state = seed;
	return function next() {
		state |= 0;
		state = (state + 0x6d2b79f5) | 0;
		let t = Math.imul(state ^ (state >>> 15), 1 | state);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

const PRODUCT_BASES = [
	'緑茶',
	'ほうじ茶',
	'麦茶',
	'烏龍茶',
	'紅茶',
	'コーヒー',
	'抹茶ラテ',
	'レモネード',
	'炭酸水',
	'スポーツドリンク',
	'オレンジジュース',
	'アップルジュース'
] as const;

const PRODUCT_SIZES = ['280ml', '350ml', '500ml', '600ml', '1L', '2L'] as const;

const BASE_UNIT_PRICE: Record<(typeof PRODUCT_BASES)[number], number> = {
	緑茶: 140,
	ほうじ茶: 140,
	麦茶: 120,
	烏龍茶: 150,
	紅茶: 160,
	コーヒー: 130,
	抹茶ラテ: 220,
	レモネード: 180,
	炭酸水: 110,
	スポーツドリンク: 170,
	オレンジジュース: 190,
	アップルジュース: 190
};

const DAY_MS = 24 * 60 * 60 * 1000;
// Fixed "today" for deterministic output regardless of when the app runs.
const UPDATED_AT_END = Date.UTC(2026, 6, 2); // 2026-07-02
const UPDATED_AT_SPAN_DAYS = 900; // ~2.5 years of history

function toIsoDate(timeMs: number): string {
	return new Date(timeMs).toISOString().slice(0, 10);
}

/** Generate `count` deterministic sample items. */
export function generateSampleItems(count: number = ROW_COUNT): Item[] {
	const random = mulberry32(SEED);
	const rows: Item[] = [];

	for (let i = 0; i < count; i++) {
		const id = i + 1;
		const base = PRODUCT_BASES[Math.floor(random() * PRODUCT_BASES.length)];
		const size = PRODUCT_SIZES[Math.floor(random() * PRODUCT_SIZES.length)];
		// Occasional lot suffix so names aren't just `base size` repeated verbatim.
		const lot = random();
		const name = lot < 0.15 ? `${base} ${size} 数量限定` : `${base} ${size}`;

		const unitPrice = BASE_UNIT_PRICE[base];
		const priceJitter = Math.round((random() * 40 - 20) / 10) * 10; // -20..+20, rounded to 10
		const price = Math.max(50, unitPrice + priceJitter);

		const stock = Math.floor(random() * 500);

		const daysAgo = Math.floor(random() * UPDATED_AT_SPAN_DAYS);
		const updatedAt = toIsoDate(UPDATED_AT_END - daysAgo * DAY_MS);

		rows.push({ id, name, price, stock, updatedAt });
	}

	return rows;
}

/** Pre-generated dataset used to seed the InMemoryDataProvider. */
export const sampleItems: Item[] = generateSampleItems();
