import { redirect } from '@sveltejs/kit';

// The root path only dispatches: guests to /login, users to /settings
// (the (app) layout guard handles the auth check). W1 has no monitoring
// screen yet (that lands in W4) - /settings is the only entry every role
// can reach, so it is the default landing page for now.
export function load(): never {
	redirect(307, '/settings');
}
