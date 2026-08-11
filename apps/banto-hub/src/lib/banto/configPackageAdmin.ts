import {
	createCollectionGroup,
	createPlcConnection,
	createTag,
	listCollectionGroups,
	listPlcConnections,
	listTags,
	updateCollectionGroup,
	updatePlcConnection,
	updateTag,
	isVirtualConnection,
	isQueuedWhileRunningError,
	type CollectionGroup,
	type CollectionGroupInput,
	type PlcConnection,
	type Tag,
	type TagInput
} from './tagRegistryAdmin';
import { getGrpcSettings, saveGrpcSettings } from './grpcSettingsAdmin';
import { getMqttSettings, saveMqttSettings, type MqttSettings } from './mqttSettingsAdmin';
import {
	buildConfigPackage,
	planByName,
	type ConfigPackage,
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

function buildGroupMap(groups: readonly CollectionGroup[]): Map<string, CollectionGroup> {
	return new Map(groups.map((group) => [group.name, group]));
}

function buildTagMap(tags: readonly Tag[]): Map<string, Tag> {
	return new Map(tags.map((tag) => [tag.name, tag]));
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

function collectGroupNames(current: readonly CollectionGroup[], pkg: ConfigPackage): Set<string> {
	const names = new Set(current.map((group) => group.name));
	for (const group of pkg.collectionGroups) {
		names.add(group.name);
	}
	return names;
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
	const [plcConnections, collectionGroups, tags, mqtt, grpc] = await Promise.all([
		listPlcConnections(),
		listCollectionGroups(),
		listTags(),
		getMqttSettings(),
		getGrpcSettings()
	]);
	return buildConfigPackage({ plcConnections, collectionGroups, tags, mqtt, grpc });
}

export async function inspectConfigPackage(pkg: ConfigPackage): Promise<ConfigPackageInspection> {
	const [currentConnections, currentGroups, currentTags, mqttSettings, grpcSettings] =
		await Promise.all([
			listPlcConnections(),
			listCollectionGroups(),
			listTags(),
			getMqttSettings(),
			getGrpcSettings()
		]);

	const currentConnectionNames = collectConnectionNames(currentConnections, pkg);
	const currentGroupNames = collectGroupNames(currentGroups, pkg);
	const connectionPlans = planByName(
		pkg.plcConnections,
		currentConnections.filter((connection: PlcConnection) => !isVirtualConnection(connection))
	);
	const groupPlans = planByName(pkg.collectionGroups, currentGroups);
	const tagPlans = planByName(pkg.tags, currentTags);
	const warnings: string[] = [];

	for (const group of pkg.collectionGroups) {
		if (!currentConnectionNames.has(group.plcConnectionName)) {
			warnings.push(
				`collection group '${group.name}' の接続 '${group.plcConnectionName}' は現在の環境で見つかりません`
			);
		}
	}
	for (const tag of pkg.tags) {
		if (!currentGroupNames.has(tag.collectionGroupName)) {
			warnings.push(
				`tag '${tag.name}' の group '${tag.collectionGroupName}' は現在の環境で見つかりません`
			);
		}
	}

	return {
		counts: {
			plcConnections: {
				create: connectionPlans.create.length,
				update: connectionPlans.update.length
			},
			collectionGroups: { create: groupPlans.create.length, update: groupPlans.update.length },
			tags: { create: tagPlans.create.length, update: tagPlans.update.length }
		},
		warnings: uniqueWarnings(warnings),
		mqttCredentialsRequired: pkg.mqtt.enabled,
		mqttSettings,
		grpcSettings
	};
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
	const [currentConnections, currentGroups, currentTags, currentMqtt] = await Promise.all([
		listPlcConnections(),
		listCollectionGroups(),
		listTags(),
		getMqttSettings()
	]);

	const warnings: string[] = [];
	const connectionByName = buildConnectionMap(currentConnections);
	for (const connection of pkg.plcConnections) {
		if (isVirtualConnection(connection)) {
			warnings.push(`virtual connection '${connection.name}' は構成パッケージに含めません`);
			continue;
		}
		const existing = connectionByName.get(connection.name);
		if (existing) {
			const updated = await updatePlcConnection(existing.id, connection);
			connectionByName.set(updated.name, updated);
		} else {
			const created = await createPlcConnection(connection);
			connectionByName.set(created.name, created);
		}
	}

	const groupByName = buildGroupMap(currentGroups);
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
			enabled: group.enabled
		};
		const existing = groupByName.get(group.name);
		if (existing) {
			const updated = await updateCollectionGroup(existing.id, input);
			groupByName.set(updated.name, updated);
		} else {
			const created = await createCollectionGroup(input);
			groupByName.set(created.name, created);
		}
	}

	const tagByName = buildTagMap(currentTags);
	for (const tag of pkg.tags) {
		const group = groupByName.get(tag.collectionGroupName);
		if (!group) {
			warnings.push(
				`tag '${tag.name}' の group '${tag.collectionGroupName}' を解決できませんでした`
			);
			continue;
		}
		const input: TagInput = {
			name: tag.name,
			collectionGroupId: group.id,
			address: tag.address,
			dataType: tag.dataType,
			stringLength: tag.stringLength,
			rawLo: tag.rawLo,
			rawHi: tag.rawHi,
			engLo: tag.engLo,
			engHi: tag.engHi,
			unit: tag.unit,
			decimals: tag.decimals,
			thresholdH: tag.thresholdH,
			thresholdHh: tag.thresholdHh,
			thresholdL: tag.thresholdL,
			thresholdLl: tag.thresholdLl,
			enabled: tag.enabled,
			writable: tag.writable,
			tagKind: tag.tagKind,
			expression: tag.expression,
			retain: tag.retain
		};
		const existing = tagByName.get(tag.name);
		if (existing) {
			const updated = await updateTag(existing.id, {
				...input,
				expectedRevision: existing.revision
			});
			tagByName.set(updated.name, updated);
		} else {
			const created = await createTag(input);
			tagByName.set(created.name, created);
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
				create: pkg.collectionGroups.filter(
					(group) => !currentGroups.some((current) => current.name === group.name)
				).length,
				update: pkg.collectionGroups.filter((group) =>
					currentGroups.some((current) => current.name === group.name)
				).length
			},
			tags: {
				create: pkg.tags.filter((tag) => !currentTags.some((current) => current.name === tag.name))
					.length,
				update: pkg.tags.filter((tag) => currentTags.some((current) => current.name === tag.name))
					.length
			}
		},
		mqttApplied: true,
		grpcApplied: true,
		warnings: uniqueWarnings(warnings)
	};
}
