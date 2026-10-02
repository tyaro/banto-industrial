// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/routes/(app)/settings/connectivityScope.test.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
import { describe, expect, it } from 'vitest';
import {
	connectivityScope,
	isIpv4Bind,
	isLoopbackHost,
	isUnspecifiedHost,
	isUnspecifiedUrl,
	pickPrimaryLanUrl,
	type ConnectivityScope
} from './connectivityScope';

describe('isLoopbackHost / connectivityScope (Issue #216, PR #254 P2 follow-up)', () => {
	// Table: bind/host literal -> expected loopback-ness -> expected scope.
	// Includes the owner's counter-example from the review: a non-canonical
	// loopback IPv4 (127.0.0.2, still in 127.0.0.0/8) that a fixed
	// `{'127.0.0.1', ...}` Set-based check missed.
	const cases: Array<[host: string, loopback: boolean, scope: ConnectivityScope]> = [
		['127.0.0.1', true, 'local'],
		['127.0.0.2', true, 'local'], // whole 127.0.0.0/8, not just .1
		['127.255.255.255', true, 'local'],
		[' 127.0.0.1 ', true, 'local'], // whitespace-padded
		['0.0.0.0', false, 'lan'],
		['192.168.1.50', false, 'lan'],
		['not-an-ip', false, 'lan'] // unparseable: conservative default is "not loopback"
	];

	for (const [host, loopback, scope] of cases) {
		it(`classifies "${host}" as loopback=${loopback}, scope=${scope}`, () => {
			expect(isLoopbackHost(host)).toBe(loopback);
			expect(connectivityScope(host)).toBe(scope);
		});
	}

	it('counter-proof: a fixed-Set classifier (the pre-fix implementation) gets 127.0.0.2 wrong', () => {
		// Documents exactly the review's example: the old
		// `LOOPBACK_BINDS.has(bind)` check reported '127.0.0.2' as 'lan',
		// which is wrong (it's in the loopback range) - this is why the fix
		// moved to a real IP-range test instead of a fixed string Set.
		const oldSetBasedScope = (bind: string): ConnectivityScope =>
			new Set(['127.0.0.1']).has(bind) ? 'local' : 'lan';
		expect(oldSetBasedScope('127.0.0.2')).toBe('lan');
		expect(connectivityScope('127.0.0.2')).toBe('local');
	});
});

describe('isUnspecifiedHost (Issue #216, PR #254 P2 3rd round)', () => {
	// Table: host literal -> whether it is the unspecified/wildcard address
	// (never a connectable destination).
	const cases: Array<[host: string, unspecified: boolean]> = [
		['0.0.0.0', true],
		[' 0.0.0.0 ', true], // whitespace-padded
		['127.0.0.1', false], // loopback is not the wildcard
		['192.168.1.50', false],
		['not-an-ip', false]
	];

	for (const [host, unspecified] of cases) {
		it(`classifies "${host}" as unspecified=${unspecified}`, () => {
			expect(isUnspecifiedHost(host)).toBe(unspecified);
		});
	}

	it('isUnspecifiedUrl reads the same test through a URL, hostname and all', () => {
		expect(isUnspecifiedUrl('http://0.0.0.0:8721')).toBe(true);
		expect(isUnspecifiedUrl('http://192.168.1.50:8721')).toBe(false);
		expect(isUnspecifiedUrl('http://127.0.0.1:8721')).toBe(false);
	});
});

describe('isIpv4Bind (Issue #216, PR #254 P2 4th round: IPv6 out of scope)', () => {
	// Table: bind literal -> whether it is a plain IPv4 dotted-quad -
	// `ConnectivitySection.svelte`'s gate for "show the normal local/LAN
	// status line and URLs/QR" vs. "show the dedicated IPv6-not-guided
	// message". Owner decision, 2026-09-29: IPv6 (in any form - loopback,
	// wildcard, specific, IPv4-mapped, zone-qualified) and any other
	// non-IPv4 string are all "not guided", with no further distinction
	// needed on the frontend (the backend already returns no URLs for any
	// of them).
	const cases: Array<[bind: string, ipv4: boolean]> = [
		['127.0.0.1', true],
		['0.0.0.0', true],
		['192.168.1.50', true],
		[' 127.0.0.1 ', true],
		['::1', false],
		['::', false],
		['[::]', false],
		['2001:db8::1', false],
		['::ffff:127.0.0.1', false], // IPv4-mapped IPv6 spelling - still "not IPv4" on the frontend
		['fe80::1%eth0', false],
		['localhost', false],
		['not-an-ip', false]
	];

	for (const [bind, ipv4] of cases) {
		it(`classifies "${bind}" as isIpv4Bind=${ipv4}`, () => {
			expect(isIpv4Bind(bind)).toBe(ipv4);
		});
	}

	it('counter-proof: treating any non-empty string as IPv4 would wrongly guide IPv6 binds', () => {
		const wronglyPermissive = (_bind: string): boolean => true;
		expect(wronglyPermissive('::1')).toBe(true);
		expect(isIpv4Bind('::1')).toBe(false);
	});
});

describe('pickPrimaryLanUrl (Issue #216, PR #254 P2 follow-up)', () => {
	// Table: the backend's `ServerStatus.urls` list for a given bind -> the
	// URL (if any) the QR should be built from. Mirrors
	// `banto_server::lan_urls_for_bind`'s shape for each bind case - which,
	// by owner decision (2026-09-29), never returns anything but IPv4 URLs
	// (or an empty list for any IPv6/unparseable bind).
	const cases: Array<{ name: string; urls: string[]; expected: string | null }> = [
		{
			name: 'loopback IPv4 bind (127.0.0.1): single loopback URL, no QR',
			urls: ['http://127.0.0.1:8721'],
			expected: null
		},
		{
			// The review's counter-example: a non-canonical loopback address
			// must still resolve to "no QR", the same as 127.0.0.1.
			name: 'loopback-range IPv4 bind (127.0.0.2): still no QR',
			urls: ['http://127.0.0.2:8721'],
			expected: null
		},
		{
			name: '0.0.0.0 bind with one LAN interface: QR for the LAN entry, not the loopback prefix',
			urls: ['http://127.0.0.1:8721', 'http://192.168.1.50:8721'],
			expected: 'http://192.168.1.50:8721'
		},
		{
			name: '0.0.0.0 bind with no LAN interfaces: no QR (loopback-only list)',
			urls: ['http://127.0.0.1:8721'],
			expected: null
		},
		{
			name: 'a specific non-loopback bind (192.168.1.50): that single URL is the QR target',
			urls: ['http://192.168.1.50:8721'],
			expected: 'http://192.168.1.50:8721'
		},
		{
			name: 'an empty list (any IPv6/unparseable bind): no QR',
			urls: [],
			expected: null
		},
		{
			// Defense in depth (PR #254 review, 3rd/4th rounds): the backend
			// should never actually put a wildcard or non-IPv4 entry in
			// `urls`, but if it somehow did, the QR must not be built from
			// it.
			name: 'defense in depth: a wildcard entry is never picked, even alongside a real LAN URL',
			urls: ['http://127.0.0.1:8721', 'http://0.0.0.0:8721', 'http://192.168.1.50:8721'],
			expected: 'http://192.168.1.50:8721'
		},
		{
			name: 'defense in depth: a non-IPv4 entry is never picked, even alongside a real LAN URL',
			urls: ['http://[::1]:8721', 'http://192.168.1.50:8721'],
			expected: 'http://192.168.1.50:8721'
		}
	];

	for (const { name, urls, expected } of cases) {
		it(name, () => {
			expect(pickPrimaryLanUrl(urls)).toBe(expected);
		});
	}

	it('counter-proof: a substring check (the pre-fix implementation) wrongly picks a loopback-range URL', () => {
		// Documents the review's exact failure mode: ConnectivitySection's
		// old `serverStatus.urls.find((url) => !url.includes('127.0.0.1'))`
		// would treat 'http://127.0.0.2:8721' as the LAN URL and build a QR
		// for it, even though this bind is loopback-only.
		const oldSubstringPicker = (urls: string[]): string | null =>
			urls.find((url) => !url.includes('127.0.0.1')) ?? null;
		expect(oldSubstringPicker(['http://127.0.0.2:8721'])).toBe('http://127.0.0.2:8721');
		expect(pickPrimaryLanUrl(['http://127.0.0.2:8721'])).toBeNull();
	});

	it('counter-proof: a loopback-only check (without the non-IPv4/unspecified exclusions) wrongly picks a non-IPv4 entry', () => {
		const oldLoopbackOnlyPicker = (urls: string[]): string | null =>
			urls.find((url) => !url.includes('127.0.0.1')) ?? null;
		expect(oldLoopbackOnlyPicker(['http://127.0.0.1:8721', 'http://[::1]:8721'])).toBe(
			'http://[::1]:8721'
		);
		expect(pickPrimaryLanUrl(['http://127.0.0.1:8721', 'http://[::1]:8721'])).toBeNull();
	});
});
