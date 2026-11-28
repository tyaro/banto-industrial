import { describe, expect, it } from 'vitest';
import { OPCUA_PLAINTEXT_NOTICE, driverNotice } from './driverNotices';

describe('driverNotice', () => {
	it('opcua は平文・匿名の注意を返す', () => {
		expect(driverNotice('opcua')).toBe(OPCUA_PLAINTEXT_NOTICE);
		expect(OPCUA_PLAINTEXT_NOTICE).toContain('平文');
		expect(OPCUA_PLAINTEXT_NOTICE).toContain('匿名');
	});

	it('slmp など他のプロトコルには出さない', () => {
		expect(driverNotice('slmp')).toBeNull();
		expect(driverNotice('modbus-tcp')).toBeNull();
		expect(driverNotice('')).toBeNull();
	});
});
