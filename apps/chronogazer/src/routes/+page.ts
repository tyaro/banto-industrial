import { redirect } from '@sveltejs/kit';
import { resolveAppPath } from '#lib/navigation.js';

// The root path only dispatches: guests to /login, users to /monitor
// (the (app) layout guard handles the auth check).
export function load(): never {
	redirect(307, resolveAppPath('/monitor'));
}
