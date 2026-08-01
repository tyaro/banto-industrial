/**
 * Client for the QR文字列リスト API（デバッグ支援, /qr-codes 画面）:
 * タッチパネル（HMI）のQRリーダーに読ませる文字列の登録・並び替えと、
 * サーバー側で生成されたQRコードSVGの取得。Same three-environment split as
 * `writeRegistryAdmin.ts`:
 *
 * - Tauri webview -> `invoke()` the `qr_strings_*` commands
 *   (`apps/relay-wright/src-tauri/src/lib.rs`).
 * - LAN browser served by the embedded server -> `fetch()` the
 *   `/api/qr-strings[...]` REST routes (`apps/relay-wright/core/src/rest.rs`).
 * - Plain `vite dev`/`vite preview` demo -> no backend/DB, so every call
 *   rejects with `DEMO_MODE_MESSAGE`; `isQrStringsAvailable()` lets the page
 *   show the note up front.
 *
 * Editor-write / viewer-read on the backend (spec M10); the page hides
 * mutation controls via `canWriteResources` (permissions.ts).
 *
 * `QrString.svg` is generated entirely by the backend's `qrcode` crate from
 * `text` - machine-generated markup only, which is what makes the page's
 * `{@html}` rendering of it satisfy docs/conventions.md §security.
 */
import { invoke } from '@tauri-apps/api/core';
import { getAuthProvider, isProviderError, ProviderError, type ErrorBody } from '@banto/admin-core';
import { CSRF_HEADER, getBantoMode } from './setup';

// --- wire types (camelCase, matching the Rust serde shapes) -----------------

/** Mirrors `relay_wright_core::qr_strings::QrString`. */
export interface QrString {
	id: number;
	/** 表示名（任意・空文字可）。QRタイルの下に添えるラベル。 */
	label: string;
	/** QRコードにする文字列。 */
	text: string;
	sortOrder: number;
	createdAt: string;
	/** サーバー側で `text` から生成されたインラインSVG（機械生成のみ）。 */
	svg: string;
}

/** Mirrors `relay_wright_core::qr_strings::QrStringInput`. */
export interface QrStringInput {
	label?: string;
	text: string;
}

// --- environment/error plumbing (mirrors writeRegistryAdmin.ts) -------------

export const DEMO_MODE_MESSAGE = 'デモモードでは利用できません';
const NETWORK_ERROR_MESSAGE = 'サーバーに接続できません';

function demoModeError(): ProviderError {
	return new ProviderError({ kind: 'other', message: DEMO_MODE_MESSAGE });
}

/** Backed by a real DB (Tauri or embedded server)? False in demo mode. */
export function isQrStringsAvailable(): boolean {
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

// --- QR strings -------------------------------------------------------------

/** 表示順（sortOrder 昇順）の全件。各行に svg を含む。 */
export async function listQrStrings(): Promise<QrString[]> {
	if (!isQrStringsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') return invokeCommand<QrString[]>('qr_strings_list');
	return httpRequest<QrString[]>('/api/qr-strings', { method: 'GET' });
}

export async function createQrString(input: QrStringInput): Promise<QrString> {
	if (!isQrStringsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<QrString>('qr_strings_create', { input });
	}
	return httpRequest<QrString>('/api/qr-strings', { method: 'POST', body: input });
}

export async function updateQrString(id: number, input: QrStringInput): Promise<QrString> {
	if (!isQrStringsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<QrString>('qr_strings_update', { id, input });
	}
	return httpRequest<QrString>(`/api/qr-strings/${id}`, { method: 'PUT', body: input });
}

export async function deleteQrString(id: number): Promise<void> {
	if (!isQrStringsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		await invokeCommand<void>('qr_strings_delete', { id });
		return;
	}
	await httpRequest<void>(`/api/qr-strings/${id}`, { method: 'DELETE', expectNoContent: true });
}

/**
 * 並び順の一括更新: 新しい表示順の id 配列（全件・過不足なし）を渡すと、
 * 並び替え後のリストが返る。↑/↓ボタンはローカルで入れ替えた全件を送る。
 */
export async function reorderQrStrings(ids: number[]): Promise<QrString[]> {
	if (!isQrStringsAvailable()) throw demoModeError();
	if (getBantoMode() === 'tauri') {
		return invokeCommand<QrString[]>('qr_strings_reorder', { input: { ids } });
	}
	return httpRequest<QrString[]>('/api/qr-strings/reorder', { method: 'PUT', body: { ids } });
}
