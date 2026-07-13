/**
 * RBAC role model for the admin-template app (spec M10). Pure, UI-agnostic
 * helpers - no Svelte imports - so they're trivial to reason about/import
 * from anywhere (route guards, components, `usersAdmin.ts`).
 *
 * `Identity.role` (packages/admin-core's `provider.ts`) is `string |
 * undefined` on the wire, matching `admin_template_core::users::Role`'s
 * lowercase serde representation exactly (`'admin' | 'editor' | 'viewer'`).
 * `parseRole` is the single place that narrows that loose wire value down to
 * this app's `Role` type.
 */
import type { Identity } from '@banto/admin-core';

export type Role = 'admin' | 'editor' | 'viewer';

const ROLES: readonly Role[] = ['admin', 'editor', 'viewer'];

function isRole(value: unknown): value is Role {
	return typeof value === 'string' && (ROLES as readonly string[]).includes(value);
}

/**
 * Fail closed (spec M10): a missing identity, a missing `role`, or any
 * unrecognized value all map to `'viewer'` - the least-privileged role -
 * rather than assuming write/admin access. This matters most for an
 * `AuthProvider` implementation that predates roles entirely (an older/
 * custom one, `Identity.role` being optional for exactly that reason): such
 * a caller should see the MOST restrictive UI, not the most permissive one.
 */
export function parseRole(identity: Pick<Identity, 'role'> | null | undefined): Role {
	return isRole(identity?.role) ? identity.role : 'viewer';
}

/** `editor` or `admin` (spec M10: "editor: + create/update/delete"). */
export function canWriteResources(role: Role): boolean {
	return role === 'admin' || role === 'editor';
}

export function isAdmin(role: Role): boolean {
	return role === 'admin';
}
