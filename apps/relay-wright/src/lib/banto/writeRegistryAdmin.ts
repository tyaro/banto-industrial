/**
 * Client for the W2 write-registry API (plan `luminous-discovering-goblet.md`):
 * `write_targets` (書き込み先) and `write_rules` (書き込みルール, with their
 * inline AND conditions). Same three-environment split as `usersAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `write_targets_*` / `write_rules_*`
 *   commands (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `/api/write-targets[...]` / `/api/write-rules[...]` REST routes
 *   (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> there is no backend/registry DB,
 *   so every call rejects with `DEMO_MODE_MESSAGE` (mirrors usersAdmin.ts);
 *   `isWriteRegistryAvailable()` lets a page show the note up front.
 *
 * Both entities are editor-write / viewer-read on the backend (spec M10): a
 * viewer can list/get but a create/update/delete is rejected (403 REST /
 * `Forbidden` Tauri) and the page hides those controls via
 * `canWriteResources` (permissions.ts). Deliberately NOT built on
 * `@banto/admin-core`'s generic DataProvider - same reasoning usersAdmin.ts
 * gives (the Tauri commands take named args / an `input` bag, not the generic
 * `{resource}/list` shape).
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

export type WriteDataType = 'bit' | 'i16' | 'u16' | 'i32' | 'u32' | 'f32';

/** Mirrors `relay_wright_core::write_targets::WriteTarget`. */
export interface WriteTarget {
	id: number;
	name: string;
	plcConnectionId: number;
	address: string;
	dataType: WriteDataType;
	rawLo: number | null;
	rawHi: number | null;
	engLo: number | null;
	engHi: number | null;
	unit: string | null;
	decimals: number;
	enabled: boolean;
}

/** Mirrors `relay_wright_core::write_targets::WriteTargetInput`. */
export interface WriteTargetInput {
	name: string;
	plcConnectionId: number;
	address: string;
	dataType: WriteDataType;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	unit?: string | null;
	decimals: number;
	enabled: boolean;
}

export type EdgeMode = 'rising' | 'falling' | 'change';
export type WriteValueMode = 'constant' | 'copy_from_source';
export type ConditionOperator = 'eq' | 'neq' | 'gt' | 'gte' | 'lt' | 'lte' | 'between' | 'bit_is';

/** Mirrors `relay_wright_core::write_rule_conditions::WriteRuleCondition`. */
export interface WriteRuleCondition {
	id: number;
	writeRuleId: number;
	sourceTagId: number;
	operator: ConditionOperator;
	thresholdValue: number;
	thresholdValue2: number | null;
}

/** Mirrors `WriteRuleConditionInput`. */
export interface WriteRuleConditionInput {
	sourceTagId: number;
	operator: ConditionOperator;
	thresholdValue: number;
	thresholdValue2?: number | null;
}

/** Mirrors `relay_wright_core::write_rules::WriteRuleDetail` (flat rule + conditions). */
export interface WriteRuleDetail {
	id: number;
	name: string;
	enabled: boolean;
	edgeMode: EdgeMode;
	cooldownMs: number | null;
	writeTargetId: number;
	writeValueMode: WriteValueMode;
	writeConstantValue: number | null;
	writeSourceTagId: number | null;
	conditions: WriteRuleCondition[];
}

/** Mirrors `relay_wright_core::write_rules::WriteRuleInput`. */
export interface WriteRuleInput {
	name: string;
	enabled: boolean;
	edgeMode: EdgeMode;
	cooldownMs?: number | null;
	writeTargetId: number;
	writeValueMode: WriteValueMode;
	writeConstantValue?: number | null;
	writeSourceTagId?: number | null;
	conditions: WriteRuleConditionInput[];
}

// --- environment/error plumbing (mirrors usersAdmin.ts) ---------------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real registry DB (Tauri or embedded server)? False in demo mode. */
export function isWriteRegistryAvailable(): boolean {
	return getBantoMode() !== 'demo';
}

const ERROR_KINDS = new Set([
	'not_found',
	'validation',
	'unauthorized',
	'forbidden',
	'storage',
	'other'
]);

function isErrorBody(value: unknown): value is ErrorBody {
	if (typeof value !== 'object' || value === null) return false;
	const kind = (value as { kind?: unknown }).kind;
	return typeof kind === 'string' && ERROR_KINDS.has(kind);
}

function toProviderError(err: unknown): ProviderError {
	if (isProviderError(err)) return err;
	if (isErrorBody(err)) return new ProviderError(err);
	const message = err instanceof Error ? err.message : String(err);
	return new ProviderError({ kind: 'other', message });
}

async function invokeCommand<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	try {
		return (await invoke(cmd, args)) as T;
	} catch (err) {
		throw toProviderError(err);
	}
}

function currentToken(): string | null {
	const auth = getAuthProvider() as { getToken?: () => string | null };
	return auth.getToken ? auth.getToken() : null;
}

interface HttpInit {
	method: string;
	body?: unknown;
	expectNoContent?: boolean;
}

async function httpRequest<T>(path: string, init: HttpInit): Promise<T> {
	const hasBody = init.body !== undefined;
	const headers: Record<string, string> = { ...CSRF_HEADER };
	if (hasBody) headers['Content-Type'] = 'application/json';
	const token = currentToken();
	if (token) headers.Authorization = `Bearer ${token}`;

	let response: Response;
	try {
		response = await fetch(path, {
			method: init.method,
			headers,
			body: hasBody ? JSON.stringify(init.body) : undefined
		});
	} catch {
		throw new ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE });
	}

	if (!response.ok) {
		let body: unknown;
		try {
			body = await response.json();
		} catch {
			throw new ProviderError({
				kind: 'other',
				message: `${response.status} ${response.statusText}`
			});
		}
		if (isErrorBody(body)) throw new ProviderError(body);
		throw new ProviderError({
			kind: 'other',
			message: `${response.status} ${response.statusText}`
		});
	}

	if (init.expectNoContent) return undefined as T;
	return (await response.json()) as T;
}

// --- write targets ----------------------------------------------------------

export async function listWriteTargets(): Promise<WriteTarget[]> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<WriteTarget[]>('write_targets_list');
	return httpRequest<WriteTarget[]>('/api/write-targets', { method: 'GET' });
}

export async function createWriteTarget(input: WriteTargetInput): Promise<WriteTarget> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<WriteTarget>('write_targets_create', { input });
	}
	return httpRequest<WriteTarget>('/api/write-targets', { method: 'POST', body: input });
}

export async function updateWriteTarget(id: number, input: WriteTargetInput): Promise<WriteTarget> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<WriteTarget>('write_targets_update', { id, input });
	}
	return httpRequest<WriteTarget>(`/api/write-targets/${id}`, { method: 'PUT', body: input });
}

export async function deleteWriteTarget(id: number): Promise<void> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('write_targets_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/write-targets/${id}`, { method: 'DELETE', expectNoContent: true });
}

// --- write rules ------------------------------------------------------------

export async function listWriteRules(): Promise<WriteRuleDetail[]> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<WriteRuleDetail[]>('write_rules_list');
	return httpRequest<WriteRuleDetail[]>('/api/write-rules', { method: 'GET' });
}

export async function createWriteRule(input: WriteRuleInput): Promise<WriteRuleDetail> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<WriteRuleDetail>('write_rules_create', { input });
	}
	return httpRequest<WriteRuleDetail>('/api/write-rules', { method: 'POST', body: input });
}

export async function updateWriteRule(id: number, input: WriteRuleInput): Promise<WriteRuleDetail> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<WriteRuleDetail>('write_rules_update', { id, input });
	}
	return httpRequest<WriteRuleDetail>(`/api/write-rules/${id}`, { method: 'PUT', body: input });
}

export async function deleteWriteRule(id: number): Promise<void> {
	if (!isWriteRegistryAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('write_rules_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/write-rules/${id}`, { method: 'DELETE', expectNoContent: true });
}
