/**
 * relay-wright の同名ファイルから無改変で複製（RBAC ロールモデルは
 * banto テンプレート共通・banto_server::AuthState の Role と同一）。
 */
import type { Identity } from '@banto/admin-core';

export type Role = 'admin' | 'editor' | 'viewer';

const ROLES: readonly Role[] = ['admin', 'editor', 'viewer'];

function isRole(value: unknown): value is Role {
	return typeof value === 'string' && (ROLES as readonly string[]).includes(value);
}

/**
 * フェイルクローズ: identity/role が欠けている・未知の値なら最も権限の
 * 弱い 'viewer' にする（書き込み/管理権限があると誤認させない）。
 */
export function parseRole(identity: Pick<Identity, 'role'> | null | undefined): Role {
	return isRole(identity?.role) ? identity.role : 'viewer';
}

/** editor 以上（書き込み可）。 */
export function canWriteResources(role: Role): boolean {
	return role === 'admin' || role === 'editor';
}

export function isAdmin(role: Role): boolean {
	return role === 'admin';
}
