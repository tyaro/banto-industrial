import {
	createCollectionGroup,
	createPlcConnection,
	createTag,
	listCollectionGroups,
	listDrivers,
	listPlcConnections,
	listTags,
	updateCollectionGroup,
	updatePlcConnection,
	updateTag,
	isQueuedWhileRunningError,
	isSecretStateWire,
	type CollectionGroup,
	type CollectionGroupInput,
	type PlcConnection,
	type PlcConnectionInput,
	type Tag,
	type TagInput
} from './tagRegistryAdmin';
import { getGrpcSettings, saveGrpcSettings } from './grpcSettingsAdmin';
import { getMqttSettings, saveMqttSettings, type MqttSettings } from './mqttSettingsAdmin';
import {
	createSinkGroup,
	listSinkGroups,
	updateSinkGroup,
	type SinkGroup,
	type SinkGroupInput
} from './sinkGroupsAdmin';
import {
	buildConfigPackage,
	checkConfigPackageConnectionFields,
	configPackageGroupKey,
	configPackagePushBridge,
	configPackageTagKey,
	describeConfigPackageTagRef,
	planByKey,
	planByName,
	resolvePushSettings,
	serializeConfigPackage,
	type ConfigPackage,
	type ConfigPackageIngestConnection,
	type ConfigPackagePlcConnection,
	type ConfigPackagePushSettings,
	type ConfigPackageImportOptions,
	type ConfigPackageImportSummary,
	type ConfigPackageInspection
} from './configPackage';

/**
 * 監査③（2026-08-12）是正: `settings/+page.svelte` は import 実行前に
 * 収集停止中であることを確認するが（事前ガード）、ガードをすり抜けて
 * import 実行中に別セッションが収集を開始した場合の保険として、
 * `tagRegistryAdmin.ts` の `QueuedWhileRunningError`（202 queued 応答）を
 * ここで検知し、サイレントスキップ（未解決の warning だけ積んで続行）
 * ではなく import 全体を中断したことを呼び出し元に伝える。
 */
export class ConfigPackageImportAbortedError extends Error {
	constructor(message: string) {
		super(message);
		this.name = 'ConfigPackageImportAbortedError';
	}
}

export function isConfigPackageImportAbortedError(
	error: unknown
): error is ConfigPackageImportAbortedError {
	return error instanceof ConfigPackageImportAbortedError;
}

function uniqueWarnings(items: readonly string[]): string[] {
	return [...new Set(items)];
}

function buildConnectionMap(connections: readonly PlcConnection[]): Map<string, PlcConnection> {
	return new Map(connections.map((connection) => [connection.name, connection]));
}

/** 予約の `virtual` 接続（`calc`・`mem`）。パッケージに載せず、取り込みもしない。 */
function isVirtualConnection(connection: { protocol: string }): boolean {
	return connection.protocol === 'virtual';
}

/**
 * 版 3: 既存の `postgres` 接続のパスワードがこの PC で使えるか（応答の
 * `settings.password` が `{"$secret": "set"}`）。`unreadable`（保存した値をこの PC で
 * 読めない）と、値が無い（キーが載らない）は使えない。
 */
function hasUsablePassword(connection: PlcConnection): boolean {
	const value = connection.settings?.password;
	return isSecretStateWire(value) && value.$secret === 'set';
}

/**
 * 版 2（2026-10-19 オーナー決定: グループ名は接続内で一意）: 現在の環境の
 * グループ・タグを、パッケージと同じキー（`configPackageGroupKey`/
 * `configPackageTagKey`）で引けるようにする。接続名・グループ名は id から
 * 引く。引けない行（孤児）はキーを持たない（`undefined`）。
 */
interface RegistryKeys {
	groupKey(group: CollectionGroup): string | undefined;
	tagKey(tag: Tag): string | undefined;
}

function registryKeys(
	connections: readonly Pick<PlcConnection, 'id' | 'name'>[],
	groups: readonly CollectionGroup[]
): RegistryKeys {
	const connectionNameById = new Map(connections.map((c) => [c.id, c.name]));
	const groupById = new Map(groups.map((g) => [g.id, g]));
	const groupKey = (group: CollectionGroup): string | undefined => {
		const connectionName = connectionNameById.get(group.plcConnectionId);
		return connectionName === undefined
			? undefined
			: configPackageGroupKey(connectionName, group.name);
	};
	return {
		groupKey,
		tagKey(tag: Tag): string | undefined {
			const group = groupById.get(tag.collectionGroupId);
			if (!group) return undefined;
			const connectionName = connectionNameById.get(group.plcConnectionId);
			return connectionName === undefined
				? undefined
				: configPackageTagKey(connectionName, group.name, tag.name);
		}
	};
}

function buildKeyedMap<T>(
	items: readonly T[],
	key: (item: T) => string | undefined
): Map<string, T> {
	const map = new Map<string, T>();
	for (const item of items) {
		const k = key(item);
		if (k !== undefined) map.set(k, item);
	}
	return map;
}

/** S6: `sinkGroups`の plan/apply 用（`buildConnectionMap`等と同じ形）。 */
function buildSinkGroupMap(groups: readonly SinkGroup[]): Map<string, SinkGroup> {
	return new Map(groups.map((group) => [group.name, group]));
}

function collectConnectionNames(
	current: readonly PlcConnection[],
	pkg: ConfigPackage
): Set<string> {
	const names = new Set(current.map((connection) => connection.name));
	for (const connection of pkg.plcConnections) {
		names.add(connection.name);
	}
	return names;
}

function collectGroupKeys(
	current: readonly CollectionGroup[],
	keys: RegistryKeys,
	pkg: ConfigPackage
): Set<string> {
	const result = new Set<string>();
	for (const group of current) {
		const key = keys.groupKey(group);
		if (key !== undefined) result.add(key);
	}
	for (const group of pkg.collectionGroups) {
		result.add(configPackageGroupKey(group.plcConnectionName, group.name));
	}
	return result;
}

function collectTagKeys(
	current: readonly Tag[],
	keys: RegistryKeys,
	pkg: ConfigPackage
): Set<string> {
	const result = new Set<string>();
	for (const tag of current) {
		const key = keys.tagKey(tag);
		if (key !== undefined) result.add(key);
	}
	for (const tag of pkg.tags) {
		result.add(configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name));
	}
	return result;
}

function resolveMqttCredentials(
	existing: MqttSettings,
	options: ConfigPackageImportOptions
): { username: string | null; password: string } {
	const username = options.mqttUsername?.trim();
	const password = options.mqttPassword ?? '';
	return {
		username: username === undefined || username === '' ? existing.username : username,
		password
	};
}

export async function loadConfigPackage(): Promise<ConfigPackage> {
	const [plcConnections, collectionGroups, tags, sinkGroups, mqtt, grpc] = await Promise.all([
		listPlcConnections(),
		listCollectionGroups(),
		listTags(),
		listSinkGroups(),
		getMqttSettings(),
		getGrpcSettings()
	]);
	return buildConfigPackage({ plcConnections, collectionGroups, tags, sinkGroups, mqtt, grpc });
}

/**
 * T19 S4（UX-42）: 元は `settings/+page.svelte` ローカル関数だったものを、
 * コマンドパレット（`commands.ts`）からも export できるようここへ移設。
 * 純関数なので vitest でテストできる（`configPackageAdmin.test.ts` 参照）。
 */
export function configPackageExportFilename(): string {
	const now = new Date();
	const y = now.getFullYear();
	const m = String(now.getMonth() + 1).padStart(2, '0');
	const d = String(now.getDate()).padStart(2, '0');
	return `banto-hub-config-${y}-${m}-${d}.json`;
}

/**
 * T19 S4（UX-42）: 設定画面の「JSON をダウンロード」ボタンとコマンド
 * パレットの `config.export` コマンドの両方から呼べる、export の中核処理。
 * `loadConfigPackage()` → `serializeConfigPackage()` → Blob → ブラウザ
 * ダウンロードのみを行い、トースト表示やエラー state は呼び出し側の責務
 * のまま据え置く（設定画面は既存の `configPackageWorking`/
 * `configPackageLoadError` state、パレットは `toastStore` を使う - 呼び出し
 * 元ごとに UI フィードバックの手段が違うため）。失敗時は呼び出し元が
 * catch できるよう例外はそのまま投げる。
 */
export async function exportConfigPackageToDownload(): Promise<void> {
	const pkg = await loadConfigPackage();
	const blob = new Blob([serializeConfigPackage(pkg)], {
		type: 'application/json;charset=utf-8'
	});
	const url = URL.createObjectURL(blob);
	try {
		const a = document.createElement('a');
		a.href = url;
		a.download = configPackageExportFilename();
		a.click();
	} finally {
		URL.revokeObjectURL(url);
	}
}

export async function inspectConfigPackage(pkg: ConfigPackage): Promise<ConfigPackageInspection> {
	const [
		currentConnections,
		currentGroups,
		currentTags,
		currentSinkGroups,
		mqttSettings,
		grpcSettings,
		drivers
	] = await Promise.all([
		listPlcConnections(),
		listCollectionGroups(),
		listTags(),
		listSinkGroups(),
		getMqttSettings(),
		getGrpcSettings(),
		listDrivers()
	]);
	// 版 3（設計 §7.1）: 欠けた欄・秘密の欄をこの Hub のドライバのスキーマで断る。確認の
	// 画面に進む前（読み込みのエラーとして）に出す。
	checkConfigPackageConnectionFields(pkg, drivers);

	const keys = registryKeys(currentConnections, currentGroups);
	const currentConnectionNames = collectConnectionNames(currentConnections, pkg);
	const currentGroupKeys = collectGroupKeys(currentGroups, keys, pkg);
	const connectionPlans = planByName(
		pkg.plcConnections,
		currentConnections.filter((connection) => !isVirtualConnection(connection))
	);
	// 版 2: グループは (接続名, グループ名)、タグは (接続名, グループ名, タグ名) で
	// 突き合わせる（グループ名は接続内でのみ一意 - 2026-10-19 オーナー決定）。
	const groupPlans = planByKey(
		pkg.collectionGroups,
		currentGroups,
		(group) => configPackageGroupKey(group.plcConnectionName, group.name),
		keys.groupKey
	);
	const tagPlans = planByKey(
		pkg.tags,
		currentTags,
		(tag) => configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name),
		keys.tagKey
	);
	const sinkGroupPlans = planByName(pkg.sinkGroups, currentSinkGroups);
	const warnings: string[] = [];

	for (const group of pkg.collectionGroups) {
		if (!currentConnectionNames.has(group.plcConnectionName)) {
			warnings.push(
				`collection group '${group.name}' の接続 '${group.plcConnectionName}' は現在の環境で見つかりません`
			);
		}
	}
	for (const tag of pkg.tags) {
		if (
			!currentGroupKeys.has(configPackageGroupKey(tag.plcConnectionName, tag.collectionGroupName))
		) {
			warnings.push(
				`tag '${tag.name}' の接続 '${tag.plcConnectionName}' のグループ '${tag.collectionGroupName}' は現在の環境で見つかりません`
			);
		}
	}
	// S6: sink group が参照する接続・タグが現在の環境に存在するか
	// （このパッケージ内で作成される予定のものも許容 - `currentConnectionNames`
	// は`collectConnectionNames`がパッケージ内の名前も既に足し込んでいる）。
	const currentTagKeys = collectTagKeys(currentTags, keys, pkg);
	for (const sinkGroup of pkg.sinkGroups) {
		if (!currentConnectionNames.has(sinkGroup.dbConnectionName)) {
			warnings.push(
				`sink group '${sinkGroup.name}' の接続 '${sinkGroup.dbConnectionName}' は現在の環境で見つかりません`
			);
		}
		for (const ref of sinkGroup.tags) {
			if (
				!currentTagKeys.has(
					configPackageTagKey(ref.plcConnectionName, ref.collectionGroupName, ref.tagName)
				)
			) {
				warnings.push(
					`sink group '${sinkGroup.name}' のタグ '${describeConfigPackageTagRef(ref)}' は現在の環境で見つかりません`
				);
			}
		}
	}

	return {
		counts: {
			plcConnections: {
				create: connectionPlans.create.length,
				update: connectionPlans.update.length
			},
			collectionGroups: { create: groupPlans.create.length, update: groupPlans.update.length },
			tags: { create: tagPlans.create.length, update: tagPlans.update.length },
			sinkGroups: { create: sinkGroupPlans.create.length, update: sinkGroupPlans.update.length }
		},
		warnings: uniqueWarnings(warnings),
		mqttCredentialsRequired: pkg.mqtt.enabled,
		mqttSettings,
		grpcSettings,
		// S1（実装指示5）: export に password を含めない
		// （`CONFIG_PACKAGE_EXCLUDED_SECRETS`）ので、パッケージが持つ postgres 接続は
		// 必ずパスワード無しで import される。ただし apply 側は name が一致する既存接続を
		// `settings.password` 抜きで update する（秘密のキーの省略は「保つ」、REST の
		// tri-state）ので、使えるパスワードを持つ既存 postgres 接続を更新するだけの
		// ケースでは再入力は不要 - 過剰に「全件再設定要」と案内しないよう、実際に再設定が
		// 必要な接続だけに絞る（2026-09 レビュー是正）:
		// (a) 新規作成される postgres 接続（このパッケージにしか存在しない）
		// (b) 既存の接続を更新するが、今の `settings.password` が `{"$secret": "set"}` で
		//     ないもの（未設定・この PC で読めない `unreadable`・postgres 以外からの切り替え）
		//     - 版 3 で `passwordSet` の代わりに `$secret` の状態で見る（設計 §7.1）
		dbConnectionsPasswordRequired: [
			...connectionPlans.create
				.filter((connection) => connection.protocol === 'postgres')
				.map((connection) => connection.name),
			...connectionPlans.update
				.filter(
					({ incoming, existing }) =>
						incoming.protocol === 'postgres' && !hasUsablePassword(existing)
				)
				.map(({ incoming }) => incoming.name)
		],
		// R2a-2e（docs/banto-hub-ingest-design.md §5.1）: API キーは移らない。
		ingestConnections: pkg.plcConnections
			.filter((connection) => connection.protocol === 'push')
			.map((connection) => connection.name)
	};
}

/**
 * R2a-2e: 接続を取り込む順。push 接続のブリッジは読む `postgres` 接続を id で
 * 持つ（§10.2）ので、ブリッジを持つ push 接続は他の全接続の**後**に作成・更新
 * する（同じパッケージで作る postgres 接続の新しい id を引くため）。それ以外の
 * 並びはパッケージのまま。
 */
export function connectionImportOrder(
	connections: readonly ConfigPackagePlcConnection[]
): ConfigPackagePlcConnection[] {
	const hasBridge = (c: ConfigPackagePlcConnection) => configPackagePushBridge(c) !== undefined;
	return [...connections.filter((c) => !hasBridge(c)), ...connections.filter(hasBridge)];
}

export async function applyConfigPackage(
	pkg: ConfigPackage,
	options: ConfigPackageImportOptions = {}
): Promise<ConfigPackageImportSummary> {
	try {
		return await applyConfigPackageInner(pkg, options);
	} catch (err) {
		if (isQueuedWhileRunningError(err)) {
			throw new ConfigPackageImportAbortedError(
				'収集が稼働中のため構成パッケージの取り込みを中断しました。収集を停止してから再実行してください。'
			);
		}
		throw err;
	}
}

async function applyConfigPackageInner(
	pkg: ConfigPackage,
	options: ConfigPackageImportOptions
): Promise<ConfigPackageImportSummary> {
	const [currentConnections, currentGroups, currentTags, currentSinkGroups, currentMqtt, drivers] =
		await Promise.all([
			listPlcConnections(),
			listCollectionGroups(),
			listTags(),
			listSinkGroups(),
			getMqttSettings(),
			listDrivers()
		]);
	// 版 3: 確認の画面（`inspectConfigPackage`）と同じ確かめを、書き込む前にもう一度する
	// （確認の後にドライバの構成が変わっても、欠けた欄を既定で埋めて書き込まない）。
	checkConfigPackageConnectionFields(pkg, drivers);

	const warnings: string[] = [];
	const connectionByName = buildConnectionMap(currentConnections);
	const ingestConnections: ConfigPackageIngestConnection[] = [];
	for (const pkgConnection of connectionImportOrder(pkg.plcConnections)) {
		if (isVirtualConnection(pkgConnection)) {
			warnings.push(`virtual connection '${pkgConnection.name}' は構成パッケージに含めません`);
			continue;
		}
		let settings: Record<string, unknown> = { ...pkgConnection.settings };
		if (pkgConnection.protocol === 'push') {
			// R2a-2e（§3.1・§5.1）: ブリッジの読む DB の名前を、取り込み先の id に
			// 付け直す（上の順で postgres 接続は作成・更新済み）。引けなければ
			// 壊れた参照で作らず、警告を積んでこの接続を飛ばす。
			const resolved = resolvePushSettings(
				pkgConnection.settings as ConfigPackagePushSettings,
				(name) => connectionByName.get(name)?.id
			);
			if (resolved === null) {
				warnings.push(
					`push 接続 '${pkgConnection.name}' のブリッジが読む接続 '${configPackagePushBridge(pkgConnection)?.dbConnectionName}' を解決できませんでした`
				);
				continue;
			}
			settings = { ...resolved };
		}
		// 版 3: REST の `settings` は丸ごと置き換え。パッケージは秘密のキーを持たない
		// （`checkConfigPackageConnectionFields` が値に関わらずキーの存在で断る。REST は
		// null・空文字を「消す」と読むので、ここに秘密のキーが来ると保存済みの秘密が消える）ので、既存の接続の更新では
		// 秘密のキーの省略 =「保存済みを保つ」になる（postgres のパスワードを消さない）。
		const connection: PlcConnectionInput = {
			name: pkgConnection.name,
			protocol: pkgConnection.protocol,
			enabled: pkgConnection.enabled,
			simulation: pkgConnection.simulation,
			settings
		};
		const existing = connectionByName.get(connection.name);
		const saved = existing
			? await updatePlcConnection(existing.id, connection)
			: await createPlcConnection(connection);
		connectionByName.set(saved.name, saved);
		if (saved.protocol === 'push') ingestConnections.push({ name: saved.name, id: saved.id });
	}

	// 版 2: 現在の環境のグループ・タグをパッケージと同じキーで引く。接続名は
	// 取り込みで変わらない（接続は名前で突き合わせる）ので、取り込み前の
	// 接続一覧から引いた名前のままでよい。
	const keys = registryKeys(currentConnections, currentGroups);
	const groupPlan = planByKey(
		pkg.collectionGroups,
		currentGroups,
		(group) => configPackageGroupKey(group.plcConnectionName, group.name),
		keys.groupKey
	);
	const tagPlan = planByKey(
		pkg.tags,
		currentTags,
		(tag) => configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name),
		keys.tagKey
	);
	const groupByKey = buildKeyedMap(currentGroups, keys.groupKey);
	for (const group of pkg.collectionGroups) {
		const connection = connectionByName.get(group.plcConnectionName);
		if (!connection) {
			warnings.push(
				`collection group '${group.name}' の接続 '${group.plcConnectionName}' を解決できませんでした`
			);
			continue;
		}
		const input: CollectionGroupInput = {
			name: group.name,
			plcConnectionId: connection.id,
			periodMs: group.periodMs,
			enabled: group.enabled,
			defaultWritable: group.defaultWritable,
			// S3（docs/banto-hub-external-db-design.md §4.1・§4.2）: postgres
			// 接続配下のグループでは非 undefined、それ以外では `pkg` 側が
			// 既に省略している（`configPackage.ts::sanitizeGroup`）ので
			// そのまま渡すだけでよい（「pkg の値をそのまま転記する」扱い）。
			querySql: group.querySql
		};
		const groupKey = configPackageGroupKey(group.plcConnectionName, group.name);
		const existing = groupByKey.get(groupKey);
		if (existing) {
			const updated = await updateCollectionGroup(existing.id, input);
			groupByKey.set(groupKey, updated);
		} else {
			const created = await createCollectionGroup(input);
			groupByKey.set(groupKey, created);
		}
	}

	const tagByKey = buildKeyedMap(currentTags, keys.tagKey);
	for (const tag of pkg.tags) {
		const group = groupByKey.get(
			configPackageGroupKey(tag.plcConnectionName, tag.collectionGroupName)
		);
		if (!group) {
			warnings.push(
				`tag '${tag.name}' の接続 '${tag.plcConnectionName}' のグループ '${tag.collectionGroupName}' を解決できませんでした`
			);
			continue;
		}
		const input: TagInput = {
			name: tag.name,
			collectionGroupId: group.id,
			address: tag.address,
			dataType: tag.dataType,
			stringLength: tag.stringLength,
			stringEncoding: tag.stringEncoding,
			rawLo: tag.rawLo,
			rawHi: tag.rawHi,
			engLo: tag.engLo,
			engHi: tag.engHi,
			unit: tag.unit,
			decimals: tag.decimals,
			enabled: tag.enabled,
			writable: tag.writable,
			tagKind: tag.tagKind,
			expression: tag.expression,
			retain: tag.retain
		};
		const tagKey = configPackageTagKey(tag.plcConnectionName, tag.collectionGroupName, tag.name);
		const existing = tagByKey.get(tagKey);
		if (existing) {
			const updated = await updateTag(existing.id, {
				...input,
				expectedRevision: existing.revision
			});
			tagByKey.set(tagKey, updated);
		} else {
			const created = await createTag(input);
			tagByKey.set(tagKey, created);
		}
	}

	// S6（docs/banto-hub-external-db-design.md §5.2・§6 item 6）: sink group
	// はタグ・接続の両方を参照するため、上の両ループ（`connectionByName`/
	// `tagByKey`が最新化済み）の**後**に適用する - `collectionGroups`が
	// `connectionByName`の後に来るのと同じ順序の理由。
	const sinkGroupByName = buildSinkGroupMap(currentSinkGroups);
	for (const sinkGroup of pkg.sinkGroups) {
		const connection = connectionByName.get(sinkGroup.dbConnectionName);
		if (!connection) {
			warnings.push(
				`sink group '${sinkGroup.name}' の接続 '${sinkGroup.dbConnectionName}' を解決できませんでした`
			);
			continue;
		}
		const tagIds: number[] = [];
		let missingTag = false;
		for (const ref of sinkGroup.tags) {
			const tag = tagByKey.get(
				configPackageTagKey(ref.plcConnectionName, ref.collectionGroupName, ref.tagName)
			);
			if (!tag) {
				warnings.push(
					`sink group '${sinkGroup.name}' のタグ '${describeConfigPackageTagRef(ref)}' を解決できませんでした`
				);
				missingTag = true;
				continue;
			}
			tagIds.push(tag.id);
		}
		if (missingTag && tagIds.length === 0) {
			// タグを1件も解決できなければ create/update 自体を送らない -
			// サーバー側 `tagIds` 必須（1件以上）検証に必ず落ちるだけなので、
			// 分かりやすい warning（上のループで既に積んでいる）で足りる。
			continue;
		}
		const input: SinkGroupInput = {
			name: sinkGroup.name,
			dbConnectionId: connection.id,
			mode: sinkGroup.mode,
			intervalMs: sinkGroup.intervalMs,
			tableName: sinkGroup.tableName,
			storeBad: sinkGroup.storeBad,
			enabled: sinkGroup.enabled,
			tagIds
		};
		const existing = sinkGroupByName.get(sinkGroup.name);
		if (existing) {
			const updated = await updateSinkGroup(existing.id, input);
			sinkGroupByName.set(updated.name, updated);
		} else {
			const created = await createSinkGroup(input);
			sinkGroupByName.set(created.name, created);
		}
	}

	const mqtt = resolveMqttCredentials(currentMqtt, options);
	await saveMqttSettings({
		enabled: pkg.mqtt.enabled,
		host: pkg.mqtt.host,
		port: pkg.mqtt.port,
		clientId: pkg.mqtt.clientId,
		username: mqtt.username,
		password: mqtt.password,
		prefix: pkg.mqtt.prefix,
		qos: pkg.mqtt.qos,
		minIntervalMs: pkg.mqtt.minIntervalMs
	});
	await saveGrpcSettings(pkg.grpc);

	return {
		counts: {
			plcConnections: {
				create: pkg.plcConnections.filter(
					(connection) => !currentConnections.some((current) => current.name === connection.name)
				).length,
				update: pkg.plcConnections.filter((connection) =>
					currentConnections.some((current) => current.name === connection.name)
				).length
			},
			collectionGroups: {
				create: groupPlan.create.length,
				update: groupPlan.update.length
			},
			tags: {
				create: tagPlan.create.length,
				update: tagPlan.update.length
			},
			sinkGroups: {
				create: pkg.sinkGroups.filter(
					(group) => !currentSinkGroups.some((current) => current.name === group.name)
				).length,
				update: pkg.sinkGroups.filter((group) =>
					currentSinkGroups.some((current) => current.name === group.name)
				).length
			}
		},
		mqttApplied: true,
		grpcApplied: true,
		warnings: uniqueWarnings(warnings),
		ingestConnections
	};
}
