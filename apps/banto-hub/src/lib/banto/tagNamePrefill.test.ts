/**
 * `tagNamePrefill.ts`（2026-09-01 オーナー要望）に対するユニットテスト。
 * `plcConnectionForm.test.ts` と同じスタイル（describe/it、依存ゼロの
 * 純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import { nextTagNameOnAddressChange } from './tagNamePrefill';

describe('nextTagNameOnAddressChange', () => {
	it('名前が空＋アドレス入力 → 名前をアドレスにする（プリフィル）', () => {
		expect(nextTagNameOnAddressChange(true, 'D100', false)).toBe('D100');
	});

	it('ユーザーが名前欄を自分で編集済み（nameTouched）なら上書きしない', () => {
		expect(nextTagNameOnAddressChange(true, 'D200', true)).toBeNull();
	});

	it('未編集のままアドレスを変更した場合は新しいアドレスへ追従する', () => {
		// 直前のアドレス入力で name が 'D100' にプリフィルされた状態を想定 -
		// nameTouched はまだ false のまま（ユーザーは名前欄自体には触れていない）。
		expect(nextTagNameOnAddressChange(true, 'D200', false)).toBe('D200');
	});

	it('tagKind が plc 以外（isPlc=false）なら働かない', () => {
		expect(nextTagNameOnAddressChange(false, 'D100', false)).toBeNull();
	});

	it('アドレスを空に戻した場合も素直に追従する（未編集の間は特別扱いしない）', () => {
		expect(nextTagNameOnAddressChange(true, '', false)).toBe('');
	});
});
