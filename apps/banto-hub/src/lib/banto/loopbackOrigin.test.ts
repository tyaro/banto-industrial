import { describe, expect, it } from 'vitest';
import { isLoopbackHost } from './loopbackOrigin';

describe('isLoopbackHost（#644 PR5）', () => {
	it.each([
		'127.0.0.1',
		'127.1.2.3',
		'127.255.255.254',
		'localhost',
		'LocalHost',
		'localhost.',
		'app.localhost',
		'::1',
		'[::1]',
		'::ffff:127.0.0.1',
		'[::ffff:7f00:1]',
		'[::ffff:7f01:203]'
	])('%s は loopback', (host) => {
		expect(isLoopbackHost(host)).toBe(true);
	});

	it.each([
		'192.168.1.5',
		'10.0.0.1',
		'128.0.0.1',
		'126.255.255.255',
		'1127.0.0.1',
		'hub.example.com',
		'localhost.example.com',
		'mylocalhost',
		'[::2]',
		'[::ffff:c0a8:105]',
		'::ffff:192.168.1.5',
		'999.0.0.1',
		''
	])('%s は loopback ではない', (host) => {
		expect(isLoopbackHost(host)).toBe(false);
	});
});
