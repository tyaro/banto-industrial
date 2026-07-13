// @ts-check
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import prettierConfig from 'eslint-config-prettier';
import globals from 'globals';
import svelteConfig from './apps/chronogazer/svelte.config.js';

export default tseslint.config(
	{
		// Generated/vendored/build output - never lint. Applies to every
		// config block below (global ignores must be their own object with
		// only an `ignores` key, per the flat config spec).
		ignores: [
			'**/node_modules/**',
			'**/.svelte-kit/**',
			'**/build/**',
			'**/dist/**',
			'**/target/**',
			'apps/chronogazer/src-tauri/gen/**',
			'pnpm-lock.yaml'
		]
	},
	js.configs.recommended,
	...tseslint.configs.recommended,
	...svelte.configs.recommended,
	// Not type-checked (`recommendedTypeChecked`): the workspace has
	// independent tsconfigs (one per app) with no shared project
	// reference graph, so wiring up type-aware linting across all of them
	// would add real complexity/CI time for limited payoff at this stage.
	// Revisit if type-aware rules turn out to catch real bugs.
	{
		languageOptions: {
			globals: {
				...globals.browser,
				...globals.node
			}
		}
	},
	{
		// `.svelte.ts` (Svelte 5 rune modules) get their own svelte-eslint-parser
		// config from `svelte.configs.recommended` above (so rune globals
		// resolve), which otherwise defaults its inner script parser to
		// espree and can't read TS syntax (interfaces, generics, etc.) - point
		// it at the TS parser here, same as plain `.svelte` files.
		files: ['**/*.svelte', '**/*.svelte.js', '**/*.svelte.ts'],
		languageOptions: {
			parserOptions: {
				parser: tseslint.parser,
				svelteConfig
			}
		}
	},
	{
		rules: {
			// Prefix a deliberately-unused arg/var with `_` (e.g. destructuring,
			// callback signatures constrained by an interface) instead of
			// disabling the rule inline.
			'@typescript-eslint/no-unused-vars': [
				'warn',
				{ argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' }
			],
			// Svelte 5 runes/props and Tauri IPC payloads are frequently typed
			// as `any` at the boundary (deserialized JSON, generic store
			// values); banning it outright fights the codebase's existing
			// patterns more than it catches bugs. Keep it a warning, not an
			// error.
			'@typescript-eslint/no-explicit-any': 'warn',
			// Headless cores intentionally use `interface`/`type` per spec
			// conventions already in the codebase - not worth enforcing one
			// over the other.
			'@typescript-eslint/consistent-type-definitions': 'off',
			// SvelteKit's typed-routing `resolve()` helper (the thing this rule
			// pushes every goto()/href toward) isn't adopted anywhere in this
			// app (no `resolve` alias configured in svelte.config.js) - the
			// rule would flag every existing navigation call site for an API
			// the codebase doesn't use.
			'svelte/no-navigation-without-resolve': 'off',
			// Flags every `new Map()`/`new Set()` regardless of whether it
			// escapes into reactive state - in this codebase every hit so far
			// has been a function-local temp collection (built and consumed
			// within one call, never assigned to component/class state), where
			// SvelteMap/SvelteSet would just add reactivity overhead for
			// nothing. Genuinely reactive collections already use
			// SvelteSet/SvelteMap by hand-applied convention (e.g.
			// grid-svelte's state.svelte.ts `collapsedGroups`); trust that
			// convention over this rule's blanket flagging.
			'svelte/prefer-svelte-reactivity': 'off'
		}
	},
	// Disable stylistic rules that fight Prettier's formatting.
	prettierConfig,
	svelte.configs.prettier
);
