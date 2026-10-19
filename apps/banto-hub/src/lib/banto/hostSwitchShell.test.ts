/**
 * `hostSwitchShell.ts` のユニットテスト（#661 661-5: invoke のエラーの文字列化）。
 */
import { describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { describeInvokeError, INVOKE_ERROR_FALLBACK } from './hostSwitchShell';

describe('describeInvokeError', () => {
	it('文字列（command の Err(String)）はそのまま', () => {
		expect(describeInvokeError('既に切替処理が進行中です')).toBe('既に切替処理が進行中です');
	});

	it('Error は message', () => {
		expect(describeInvokeError(new Error('IPC に失敗しました'))).toBe('IPC に失敗しました');
	});

	it('message を持つオブジェクトは message', () => {
		expect(describeInvokeError({ message: 'not allowed' })).toBe('not allowed');
	});

	it('message の無いオブジェクトは [object Object] にせず中立の文（#688）', () => {
		const text = describeInvokeError({ code: 1 });
		expect(text).toBe(INVOKE_ERROR_FALLBACK);
		expect(text).not.toContain('[object Object]');
	});

	it('空・null・数値・空の message も中立の文、代わりの文を渡せる', () => {
		expect(describeInvokeError('')).toBe(INVOKE_ERROR_FALLBACK);
		expect(describeInvokeError(null)).toBe(INVOKE_ERROR_FALLBACK);
		expect(describeInvokeError(42)).toBe(INVOKE_ERROR_FALLBACK);
		expect(describeInvokeError({ message: '' })).toBe(INVOKE_ERROR_FALLBACK);
		expect(describeInvokeError(undefined, '切替に失敗しました')).toBe('切替に失敗しました');
	});
});
