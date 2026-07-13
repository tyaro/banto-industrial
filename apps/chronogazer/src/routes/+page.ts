import { redirect } from '@sveltejs/kit';

// The root path only dispatches: guests to /login, users to /dashboard
// (the (app) layout guard handles the auth check).
export function load(): never {
	redirect(307, '/dashboard');
}
