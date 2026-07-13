//! Database bootstrap for the chronogazer app (spec §12): connect,
//! run embedded migrations, seed demo data on first run.

use banto_core::BantoError;
use sqlx::SqlitePool;

const SEED_ROW_COUNT: usize = 1_000;

/// Connect to the SQLite database at `path`, run migrations, and seed demo
/// data if the `items` table is empty. Used by the `src-tauri` adapter with
/// a path under the app's data directory.
pub async fn init_db(path: impl AsRef<std::path::Path>) -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite(path).await?;
    run_migrations_and_seed(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db`] but against a private in-memory database. Used by
/// tests so each test gets an isolated, migrated, seeded database.
pub async fn init_db_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations_and_seed(&pool).await?;
    Ok(pool)
}

/// A migrated but *unseeded* in-memory database. Used by `items` service
/// tests that need full control over which rows exist (e.g. asserting a
/// specific id is absent), where the 1,000-row demo seed from
/// [`init_db_memory`] would collide with test fixture ids.
#[cfg(test)]
pub(crate) async fn migrate_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    Ok(pool)
}

async fn run_migrations_and_seed(pool: &SqlitePool) -> Result<(), BantoError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    seed_if_empty(pool).await?;
    Ok(())
}

async fn seed_if_empty(pool: &SqlitePool) -> Result<(), BantoError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    if count > 0 {
        return Ok(());
    }

    let rows = generate_sample_items(SEED_ROW_COUNT);
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO items (id, name, price, stock, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(row.id)
        .bind(&row.name)
        .bind(row.price)
        .bind(row.stock)
        .bind(&row.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(banto_storage::storage_error)?;
    }
    tx.commit().await.map_err(banto_storage::storage_error)?;
    Ok(())
}

struct SeedItem {
    id: i64,
    name: String,
    price: i64,
    stock: i64,
    updated_at: String,
}

/// mulberry32: small, fast, deterministic PRNG (no runtime dependency).
/// Ported 1:1 from `apps/chronogazer/src/lib/banto/sampleData.ts` so the
/// two seed generators (TS `InMemoryDataProvider` seed data, Rust SQLite
/// seed data) use the exact same algorithm and per-row draw order - only
/// the row count and destination differ.
struct Mulberry32 {
    state: u32,
}

impl Mulberry32 {
    fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// Returns a float in `[0, 1)`, matching the TS implementation's
    /// `>>> 0) / 4294967296` normalization.
    fn next(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6d2b79f5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
}

const PRODUCT_BASES: [&str; 12] = [
    "緑茶",
    "ほうじ茶",
    "麦茶",
    "烏龍茶",
    "紅茶",
    "コーヒー",
    "抹茶ラテ",
    "レモネード",
    "炭酸水",
    "スポーツドリンク",
    "オレンジジュース",
    "アップルジュース",
];

const PRODUCT_SIZES: [&str; 6] = ["280ml", "350ml", "500ml", "600ml", "1L", "2L"];

fn base_unit_price(base: &str) -> i64 {
    match base {
        "緑茶" => 140,
        "ほうじ茶" => 140,
        "麦茶" => 120,
        "烏龍茶" => 150,
        "紅茶" => 160,
        "コーヒー" => 130,
        "抹茶ラテ" => 220,
        "レモネード" => 180,
        "炭酸水" => 110,
        "スポーツドリンク" => 170,
        "オレンジジュース" => 190,
        "アップルジュース" => 190,
        _ => unreachable!("all PRODUCT_BASES entries are covered above"),
    }
}

const SEED: u32 = 0x8a17c05;
// Fixed "today" for deterministic output regardless of when the app runs
// (matches sampleData.ts's UPDATED_AT_END = Date.UTC(2026, 6, 2)).
const UPDATED_AT_END_DAYS_SINCE_EPOCH: i64 = 20636; // 2026-07-02 UTC
const UPDATED_AT_SPAN_DAYS: i64 = 900; // ~2.5 years of history

/// Days-since-epoch (1970-01-01) -> `YYYY-MM-DD`, using Howard Hinnant's
/// `civil_from_days` algorithm (http://howardhinnant.github.io/date_algorithms.html).
/// No date/time crate dependency for one small conversion.
///
/// `pub(crate)` (not private) since `crate::backup` (spec M17) reuses this to
/// turn a backup file's filesystem mtime into an ISO date for display,
/// rather than duplicating the same algorithm a second time.
pub(crate) fn iso_date_from_days_since_epoch(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Port of `generateSampleItems` (`apps/chronogazer/src/lib/banto/sampleData.ts`):
/// same PRNG, same product tables, same per-row draw order, so the first
/// `count` rows are identical to the TS dataset's first `count` rows.
fn generate_sample_items(count: usize) -> Vec<SeedItem> {
    let mut random = Mulberry32::new(SEED);
    let mut rows = Vec::with_capacity(count);

    for i in 0..count {
        let id = (i + 1) as i64;
        let base = PRODUCT_BASES[(random.next() * PRODUCT_BASES.len() as f64).floor() as usize];
        let size = PRODUCT_SIZES[(random.next() * PRODUCT_SIZES.len() as f64).floor() as usize];
        let lot = random.next();
        let name = if lot < 0.15 {
            format!("{base} {size} 数量限定")
        } else {
            format!("{base} {size}")
        };

        let unit_price = base_unit_price(base);
        let price_jitter = ((random.next() * 40.0 - 20.0) / 10.0).round() as i64 * 10; // -20..+20, rounded to 10
        let price = (unit_price + price_jitter).max(50);

        let stock = (random.next() * 500.0).floor() as i64;

        let days_ago = (random.next() * UPDATED_AT_SPAN_DAYS as f64).floor() as i64;
        let updated_at_days = UPDATED_AT_END_DAYS_SINCE_EPOCH - days_ago;
        let updated_at = iso_date_from_days_since_epoch(updated_at_days);

        rows.push(SeedItem {
            id,
            name,
            price,
            stock,
            updated_at,
        });
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_date_round_trips_known_epoch_days() {
        assert_eq!(iso_date_from_days_since_epoch(0), "1970-01-01");
        assert_eq!(iso_date_from_days_since_epoch(1), "1970-01-02");
        assert_eq!(iso_date_from_days_since_epoch(-1), "1969-12-31");
        // 2026-07-02 is the fixed "today" used by the seed generator.
        assert_eq!(
            iso_date_from_days_since_epoch(UPDATED_AT_END_DAYS_SINCE_EPOCH),
            "2026-07-02"
        );
    }

    #[test]
    fn generate_sample_items_is_deterministic() {
        let a = generate_sample_items(50);
        let b = generate_sample_items(50);
        assert_eq!(a.len(), 50);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.id, y.id);
            assert_eq!(x.name, y.name);
            assert_eq!(x.price, y.price);
            assert_eq!(x.stock, y.stock);
            assert_eq!(x.updated_at, y.updated_at);
        }
    }

    #[test]
    fn generate_sample_items_produces_plausible_rows() {
        let rows = generate_sample_items(200);
        for row in &rows {
            assert!(row.price >= 50);
            assert!(row.stock >= 0 && row.stock < 500);
            assert_eq!(row.updated_at.len(), 10); // YYYY-MM-DD
        }
    }

    #[tokio::test]
    async fn init_db_memory_migrates_and_seeds_exactly_once() {
        let pool = init_db_memory()
            .await
            .expect("init_db_memory should succeed");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, SEED_ROW_COUNT as i64);
    }

    #[tokio::test]
    async fn seeding_is_idempotent_across_two_inits_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations_and_seed(&pool).await.unwrap();
        run_migrations_and_seed(&pool).await.unwrap(); // second init: must not double-seed
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, SEED_ROW_COUNT as i64);
    }
}
