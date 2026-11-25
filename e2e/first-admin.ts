/**
 * The administrator account of the ChronoGazer e2e database (`pnpm e2e`).
 *
 * banto v8 (ADR-0034 §5/§7): a new database has no account and needs no login,
 * and there is no first-run setup screen (`POST /api/auth/setup` and
 * `BANTO_ALLOW_SETUP` are gone). `playwright.config.ts` therefore creates this
 * account with `banto-serve create-admin` BEFORE the server starts
 * (`serve-with-first-admin.mjs`) - the same step an operator of a headless
 * install takes - which also makes the database login-required. The specs then
 * start at the ordinary login form.
 */
export const E2E_ADMIN = {
	displayName: 'E2E管理者',
	username: 'e2e-admin',
	password: 'E2eAdminPass1'
} as const;
