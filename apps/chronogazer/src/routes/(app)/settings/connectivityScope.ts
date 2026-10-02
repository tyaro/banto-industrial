// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/routes/(app)/settings/connectivityScope.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Pure classification of the embedded server's reachability (Issue #216),
 * plus the one loopback test both halves of that classification share.
 * `ConnectivitySection.svelte` uses `connectivityScope` for the "running,
 * this PC only" vs. "running, reachable from the LAN" status wording, and
 * `pickPrimaryLanUrl` to choose which returned URL (if any) gets a QR code.
 *
 * Owner review on PR #254 (P2, 2nd round): these two decisions used to be
 * made two different ways - `connectivityScope` checked `bind` against a
 * fixed `{'127.0.0.1','::1','localhost'}` Set, while
 * `ConnectivitySection.svelte`'s QR picker checked each URL with
 * `!url.includes('127.0.0.1')`. Neither is a real loopback test: a bind of
 * `127.0.0.2` (anywhere in the loopback range other than the canonical
 * address) is loopback per `banto-server::server::lan_urls_for_bind`'s
 * `Ipv4Addr::is_loopback` check, but was not in the Set (status showed
 * `'lan'`) and did not contain the substring `127.0.0.1` (the QR picker
 * still chose it as "the LAN URL", the opposite of what the status line
 * claimed). Both entry points now call `isLoopbackHost`, so they can no
 * longer disagree with each other.
 *
 * This intentionally stays on the frontend rather than adding a
 * backend-computed field to `ServerStatus`: the input (a bind literal, or a
 * URL's hostname) is already fully available here, and the set of loopback
 * forms is small and closed. If `ServerStatus.bind` ever stops being a
 * literal the frontend can parse on its own, revisit this in favor of an
 * additive backend-computed field (e.g. `scope`/`primaryLanUrl`) instead.
 *
 * **IPv4 only, by owner decision (2026-09-29).** Earlier revisions of this
 * fix also classified IPv6 binds/URLs (loopback `::1`, wildcard `::`,
 * IPv4-mapped spellings, link-local `fe80::/10`), mirroring
 * `banto_server::server::lan_urls_for_bind`'s IPv6 handling at the time.
 * That Rust-side IPv6 support turned out to have two real bugs of its own
 * (PR #254 review, 4th round: IPv4 URLs falsely advertised as reachable
 * over a `::` bind when the listener is not dual-stack, and link-local IPv6
 * advertised without the zone id it needs to be reachable at all) - rather
 * than keep patching IPv6-specific guidance for a path this app does not
 * support end-to-end, `lan_urls_for_bind` now returns no URLs at all for
 * any IPv6 bind, and this module was simplified to match: `isLoopbackHost`/
 * `isUnspecifiedHost`/`pickPrimaryLanUrl` only ever recognize IPv4, and
 * `isIpv4Bind` tells `ConnectivitySection.svelte` when to show its
 * dedicated "IPv6 binds are not guided" message instead of the normal
 * local/LAN status line. Revisit if/when the listener grows deliberate
 * dual-stack/IPv6 support.
 */
export type ConnectivityScope = 'local' | 'lan';

/**
 * Whether `host` (a bind literal like `ServerStatus.bind`, or a URL's
 * `hostname`) is an IPv4 address anywhere in the loopback range
 * (`127.0.0.0/8`, not just `127.0.0.1`) - the only address family/range
 * this module still classifies as loopback (see the module doc for why
 * IPv6 is out of scope).
 *
 * Deliberately conservative on anything that is not a plain IPv4 literal
 * (returns `false`, i.e. "not proven loopback") - this feeds a "should we
 * show a LAN URL/QR" decision, and wrongly saying "loopback" would hide a
 * real LAN URL, which is a more confusing failure to debug than wrongly
 * saying "not loopback".
 */
export function isLoopbackHost(host: string): boolean {
	const octets = matchIpv4(host);
	return octets !== null && octets[0] === 127;
}

/**
 * Whether `host` is the IPv4 unspecified/wildcard address `0.0.0.0` -
 * "listen on every interface", never a destination a client connects to
 * (RFC 4291 §2.5.2 / RFC 1122 §3.2.1.3 for the IPv4 case). See this
 * module's top-of-file doc for why `pickPrimaryLanUrl` excludes it in
 * addition to loopback.
 */
export function isUnspecifiedHost(host: string): boolean {
	return matchIpv4(host) !== null && normalizeHost(host) === '0.0.0.0';
}

/**
 * Whether `bind` (`ServerStatus.bind`) is an IPv4 literal at all - the
 * gate `ConnectivitySection.svelte` uses to decide whether to show the
 * normal local/LAN status line (and URLs/QR) or its dedicated "IPv6 is not
 * guided" message (owner decision, 2026-09-29 - see the module doc).
 * Anything that is not a plain, valid IPv4 dotted-quad - an IPv6 literal in
 * any form, a hostname, a zone-qualified address, garbage - is "not IPv4".
 */
export function isIpv4Bind(bind: string): boolean {
	return matchIpv4(bind) !== null;
}

/** Trims and strips a `[...]` bracket pair, lower-cased. */
function normalizeHost(host: string): string {
	const trimmed = host.trim();
	const bare = trimmed.startsWith('[') && trimmed.endsWith(']') ? trimmed.slice(1, -1) : trimmed;
	return bare.toLowerCase();
}

/** A valid IPv4 dotted-quad's four octets (each checked to be 0-255), or `null`. */
function matchIpv4(host: string): number[] | null {
	const lower = normalizeHost(host);
	const m = lower.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
	if (!m) return null;
	const octets = m.slice(1).map(Number);
	return octets.every((n) => n >= 0 && n <= 255) ? octets : null;
}

/**
 * Classifies `bind` (the raw `ServerStatus.bind`/`ServerSettings.bind`
 * string) into `'local'` (only this PC can connect) or `'lan'` (other
 * devices on the network are configured to be able to connect - actual
 * reachability still depends on firewalls/routers, which this does not and
 * cannot know about). Only meaningful for an IPv4 `bind` - callers check
 * `isIpv4Bind` first (see this module's doc).
 */
export function connectivityScope(bind: string): ConnectivityScope {
	return isLoopbackHost(bind) ? 'local' : 'lan';
}

/**
 * Whether `url` (one of `ServerStatus.urls`, e.g. `http://127.0.0.2:8721`)
 * points at a loopback address, by parsing out its hostname and running it
 * through the same `isLoopbackHost` check `connectivityScope` uses.
 * Unparseable input is treated as NOT loopback (see `isLoopbackHost`'s doc)
 * so a malformed entry does not silently disappear from the "pick a URL for
 * the QR" search.
 */
export function isLoopbackUrl(url: string): boolean {
	try {
		return isLoopbackHost(new URL(url).hostname);
	} catch {
		return false;
	}
}

/**
 * Whether `url` points at the unspecified/wildcard address (see
 * `isUnspecifiedHost`'s doc) - i.e. not a real destination at all, even
 * though it is not loopback either. `pickPrimaryLanUrl` excludes both.
 */
export function isUnspecifiedUrl(url: string): boolean {
	try {
		return isUnspecifiedHost(new URL(url).hostname);
	} catch {
		return false;
	}
}

/**
 * Given `ServerStatus.urls` (already scoped to the applied `bind` by
 * `banto_server::lan_urls_for_bind`), picks the one URL to show a QR code
 * for: the first IPv4 entry that is neither loopback nor the unspecified
 * address itself, or `null` if none qualify (a loopback-scoped bind's
 * `urls` is always exactly one loopback entry, so this always resolves to
 * `null` for it - no separate `scope === 'local'` check needed here, but
 * `ConnectivitySection.svelte` still gates the QR block on `scope ===
 * 'lan'` too, so the two independent computations stay cross-checked
 * against each other rather than one silently relying on the other never
 * being wrong). The IPv4-only and unspecified-address exclusions are
 * defense-in-depth - `lan_urls_for_bind` should never actually put a
 * non-IPv4 or unspecified entry in `urls` - not something expected to
 * trigger in practice.
 */
export function pickPrimaryLanUrl(urls: readonly string[]): string | null {
	return (
		urls.find((url) => {
			let hostname: string;
			try {
				hostname = new URL(url).hostname;
			} catch {
				return false;
			}
			return matchIpv4(hostname) !== null && !isLoopbackUrl(url) && !isUnspecifiedUrl(url);
		}) ?? null
	);
}
