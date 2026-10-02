/**
 * テスト専用（アプリからは import しない）: v2 の標準 `AuthProvider`
 * （`resolve`・`credentialRevision`・`onCredentialChanged`）のメモリ上の偽物。
 * SessionController（`createSessionController`）を**本物のまま**走らせて、
 * 試運転の policy runner・ロックダウンの順序・再接続の失敗後の確認を確かめる
 * ために使う（モックの controller では adopt/end・ticket・世代の規則が見えない）。
 *
 * - `answer`: 次の `resolve()` が返す答えを決める（既定は `none`）。revision を
 *   受け取り、`checked`/`current` に入れる。
 * - `change()`: 資格情報が変わった（別タブのログイン・トークンの消去）ことを
 *   通知する（revision を進めて listener を呼ぶ）。
 * - `resolve` は `vi.fn` なので呼ばれた回数を数えられる。
 */
import { vi } from 'vitest';
import type { AuthProvider, CredentialRevision, Identity, ResolvedAuth } from '@banto/admin-core';

export type Answer = (revision: CredentialRevision) => ResolvedAuth | Promise<ResolvedAuth>;

export const noneAnswer: Answer = (r) => ({ status: 'none', checked: r, current: r });

export const accountAnswer =
	(identity: Identity): Answer =>
	(r) => ({ status: 'active', checked: r, current: r, identity, kind: 'account' });

export const ALICE: Identity = { id: 'alice', name: 'Alice', role: 'admin' };

export function fakeAuthProvider(initial: Answer = noneAnswer) {
	let revision = 1;
	const listeners = new Set<() => void>();
	const rev = (): CredentialRevision => `${revision}` as CredentialRevision;
	const state = { answer: initial };
	const resolve = vi.fn(async (_options?: { signal?: AbortSignal }) => state.answer(rev()));
	const auth: AuthProvider = {
		login: async () => ({ success: true }),
		logout: async () => {},
		resolve,
		credentialRevision: rev,
		onCredentialChanged(listener) {
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		}
	};
	return {
		auth,
		resolve,
		setAnswer(answer: Answer): void {
			state.answer = answer;
		},
		change(): void {
			revision += 1;
			for (const listener of [...listeners]) listener();
		}
	};
}
