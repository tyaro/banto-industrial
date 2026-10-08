/**
 * アプリのトーストストア（banto #220 段階 3、2026-10-08）。実体は `@banto/ui` の
 * `createToastStore()`（banto ADR-0018 §8・段階 2c）で、ここはアプリ全体で
 * 1 つのインスタンスを持つだけ。表示は `@banto/ui` の `ToastHost`
 * （`routes/+layout.svelte`）、admin-core の `Notifier` への配線は
 * `src/lib/banto/setup.ts`。
 *
 * それまでの自前ストア（relay-wright からの複製）にあった T19 S2-c2（UX-40）の
 * アクションボタン（タグ削除の取り消し）と、`push` が `id` を返す仕様は
 * banto 側の標準になった。違いは 2 点: **id は文字列**（以前は数値）、
 * アクションの prop は **`onAction`**（以前は `onClick`）。呼び出し元が id を
 * 持って `dismiss(id)` する使い方（`(app)/tags/+page.svelte` の
 * `scheduleTagDeletion`）はそのまま動く。表示時間の既定は 4000ms で以前と同じ。
 */
import { createToastStore } from '@banto/ui';

export const toastStore = createToastStore();
