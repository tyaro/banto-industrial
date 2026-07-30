/**
 * Single source of truth for this app's display name.
 *
 * "RelayWright" is a provisional name (plan: `luminous-discovering-goblet.md`
 * Context/W1 - "relay" = writing to PLC output devices, "wright" = a
 * craftsman acting according to rules) that the repo owner may still change
 * before later milestones. Every UI surface that shows the app's name
 * (Sidebar brand, login screen, `pageTitle()` fallback) imports this
 * constant instead of hardcoding the literal, so a rename only ever touches
 * this one file plus the non-UI identifiers that intentionally do NOT track
 * it (Tauri `identifier`/`productName` in `src-tauri/tauri.conf.json`, the
 * OS keyring `SERVICE_NAME` in `src-tauri/src/keyring_store.rs`, and on-disk
 * file names like `relay-wright.sqlite3` - those are protocol/storage
 * identifiers a rename must not silently orphan, same reasoning as
 * `keyring_store.rs`'s own doc comment).
 */
export const APP_NAME = 'RelayWright';
