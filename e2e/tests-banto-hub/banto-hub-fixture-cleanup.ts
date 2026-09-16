/**
 * banto-hub E2E スイート共通のフィクスチャ掃除ヘルパー（#380 レビュー対応、
 * 2026-09-16）。ファイル名が `banto-hub-*.spec.ts` に一致しないので
 * `banto-hub.playwright.config.ts` の `testMatch` には拾われない
 * （`banto-hub-auth.ts` と同じ置き方）。
 *
 * ## なぜ共有するのか
 *
 * `banto-hub-tags-expression-completion.spec.ts` と
 * `banto-hub-tags-expression-insert.spec.ts` が同じ掃除を各自で持っていて、
 * 同じ落とし穴（下記）を2回踏んだ。スイート全体で1つの DB を共有しているので、
 * 掃除漏れは後続スペックの一覧行数（＝仮想化されたグリッドの描画窓）を押し上げ、
 * `name` の UNIQUE 制約は失敗テストのリトライで `beforeAll` ごと落とす。
 *
 * ## タグ削除の2つの落とし穴
 *
 * 1. **順序が式の参照に縛られる**。参照されている側を先に消すとサーバーの
 *    preflight（参照切れ）が 4xx で拒否する。依存の向きを解析する代わりに
 *    **1周で1件も消せなくなるまで回す**（{@link deleteAllWithRetries}）。
 * 2. **参照元は対象グループの外にもいる**。preflight は**カタログ全体**に対して
 *    依存を見るので、別グループの computed タグが対象タグを参照していると、
 *    対象グループ内だけを何周回しても進捗ゼロで終わる（#380 レビュー11回目）。
 *    そこで**対象タグを（推移的に）参照している computed タグをカタログ全体から
 *    拾って削除対象に加える**（{@link collectDependentTagIds}）。
 *
 * 参照の抽出はアプリ側の実装（`tagDeleteImpact.ts`）をそのまま使う - E2E 用に
 * 正規表現を書き起こすと、lexer 規則の写し間違いをもう1箇所抱えることになる。
 * あのモジュールは実行時 import を持たない（型 import のみ）ので、Playwright の
 * トランスパイルでもそのまま読める。
 */
import { expect, type APIRequestContext } from '@playwright/test';
import {
	buildExternalName,
	expressionReferencesExternalName
} from '../../apps/banto-hub/src/lib/banto/tagDeleteImpact';

interface CatalogConnection {
	id: number;
	name: string;
}
interface CatalogGroup {
	id: number;
	name: string;
	plcConnectionId: number;
}
interface CatalogTag {
	id: number;
	name: string;
	collectionGroupId: number;
	tagKind: string;
	expression: string | null;
}

/**
 * 与えられたパス群を「**進捗がある限り繰り返す**」方式で全部消す。
 *
 * 1周ごとに必ず1件以上は消える想定なので高々 O(n²) 回の DELETE で収束する。
 * 1周で1件も減らなければ、これ以上進まない（本当に消せない）ので `expect` で
 * 落とす - 握りつぶすと掃除漏れが後続スペックへ波及する。
 */
export async function deleteAllWithRetries(
	request: APIRequestContext,
	headers: Record<string, string>,
	paths: string[]
): Promise<void> {
	let remaining = [...paths];
	const failures = new Map<string, string>();
	while (remaining.length > 0) {
		const stillRemaining: string[] = [];
		failures.clear();
		for (const path of remaining) {
			const res = await request.delete(path, { headers });
			// 204 = 削除できた / 404 = 既に無い。それ以外（参照されていることによる
			// preflight 拒否など）は次の周で再試行する。
			if (res.status() === 204 || res.status() === 404) continue;
			stillRemaining.push(path);
			failures.set(path, `${res.status()} ${await res.text()}`);
		}
		expect(
			stillRemaining.length,
			`削除が進まなくなりました: ${[...failures].map(([p, why]) => `${p} -> ${why}`).join(', ')}`
		).toBeLessThan(remaining.length);
		remaining = stillRemaining;
	}
}

/**
 * `seedTagIds` のタグを（推移的に）参照している computed タグの id を、
 * **カタログ全体**から集めて返す（`seedTagIds` 自身も含む）。
 *
 * 参照の判定は `tagDeleteImpact.ts::expressionReferencesExternalName`
 * （lexer 忠実な近似正規表現）。到達できなくなるまで繰り返すので、
 * 「A を参照する B を参照する C」も拾える。
 */
function collectDependentTagIds(
	tags: CatalogTag[],
	externalNameById: Map<number, string>,
	seedTagIds: Set<number>
): Set<number> {
	const doomed = new Set(seedTagIds);
	for (;;) {
		let added = false;
		for (const tag of tags) {
			if (doomed.has(tag.id) || tag.tagKind !== 'computed' || !tag.expression) continue;
			for (const id of doomed) {
				const externalName = externalNameById.get(id);
				if (externalName === undefined) continue;
				if (expressionReferencesExternalName(tag.expression, externalName)) {
					doomed.add(tag.id);
					added = true;
					break;
				}
			}
		}
		if (!added) return doomed;
	}
}

export interface FixtureCleanupTarget {
	/** 掃除する収集グループ名（配下のタグごと消す。グループ自体も消す）。 */
	groupNames: string[];
	/**
	 * 掃除する PLC 接続名。**予約接続（`calc`/`mem`）は渡さないこと** -
	 * サーバー起動時に自動作成される共有リソースなので、その配下のグループと
	 * タグだけを消す。
	 */
	connectionNames?: string[];
}

/**
 * spec が使った固定名のリソースを、存在すれば掃除する。**`beforeAll` の先頭と
 * `afterAll` の両方**で呼ぶこと（このファイル冒頭の doc comment 参照）。
 *
 * 削除順は **タグ（対象グループ配下 + その依存元、まとめてリトライ）→ グループ
 * → 接続**。タグ同士の順序はリトライで解く。
 */
export async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>,
	target: FixtureCleanupTarget
): Promise<void> {
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	const groupsRes = await request.get('/api/collection-groups', { headers });
	const tagsRes = await request.get('/api/tags', { headers });
	if (!connectionsRes.ok() || !groupsRes.ok() || !tagsRes.ok()) return;

	const connections = (await connectionsRes.json()) as CatalogConnection[];
	const groups = (await groupsRes.json()) as CatalogGroup[];
	const tags = (await tagsRes.json()) as CatalogTag[];

	// 完全外部名（`{接続}.{グループ}.{タグ}`）- 参照判定に要る。
	const connectionNameById = new Map(connections.map((c) => [c.id, c.name]));
	const groupById = new Map(groups.map((g) => [g.id, g]));
	const externalNameById = new Map<number, string>();
	for (const tag of tags) {
		const group = groupById.get(tag.collectionGroupId);
		if (!group) continue;
		const connectionName = connectionNameById.get(group.plcConnectionId);
		if (connectionName === undefined) continue;
		externalNameById.set(tag.id, buildExternalName(connectionName, group.name, tag.name));
	}

	const targetGroups = groups.filter((g) => target.groupNames.includes(g.name));
	const targetGroupIds = new Set(targetGroups.map((g) => g.id));
	const seedTagIds = new Set(
		tags.filter((t) => targetGroupIds.has(t.collectionGroupId)).map((t) => t.id)
	);

	// 対象タグ + それを（推移的に）参照している computed タグ。
	const doomedTagIds = collectDependentTagIds(tags, externalNameById, seedTagIds);
	await deleteAllWithRetries(
		request,
		headers,
		[...doomedTagIds].map((id) => `/api/tags/${id}`)
	);

	// グループ・接続は FK（RESTRICT）だけの順序なので、同じリトライで足りる。
	await deleteAllWithRetries(
		request,
		headers,
		targetGroups.map((g) => `/api/collection-groups/${g.id}`)
	);

	const targetConnectionNames = target.connectionNames ?? [];
	await deleteAllWithRetries(
		request,
		headers,
		connections
			.filter((c) => targetConnectionNames.includes(c.name))
			.map((c) => `/api/plc-connections/${c.id}`)
	);
}
