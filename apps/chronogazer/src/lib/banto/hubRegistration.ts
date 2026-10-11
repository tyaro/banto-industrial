/**
 * 「Hub から追加」のダイアログの判断（#383 段階3 P4、2026-10-11 オーナー決定）。
 * **純関数だけ**を置き、`hubRegistration.test.ts` で総当たりに固定する
 * （checklist §5「判断は純関数に出して、状態の総当たりを表でテストする」）。
 * 画面（`routes/(app)/tags/HubImportDialog.svelte`）はここの結果を出すだけ。
 *
 * 規則の正はサーバー（`chronogazer_core::hub_tags` のモジュール doc）で、
 * ここは**同じ規則で先に選べなくする**だけ（サーバーも同じ理由で断る）:
 *
 * - 登録できるのは数値と bit（`isRegistrableDataType`）。文字列・構造体は
 *   理由付きで選べなくする。
 * - 登録済みの外部名は「登録済み」として選べなくする。
 * - P4 より前の設定画面で選んでいたタグ（`legacySelection`）は、選べるものだけ
 *   **1 度だけ**事前にチェックし、注記を出す（自動ではタグにしない - グループと
 *   周期を持たないため）。
 * - 名前の衝突は黙って改名しない - サーバーの行ごとのエラーをその行に出す。
 */
import type { HubCatalog, HubTag } from './hubAdmin';
import type {
	CollectionGroup,
	HubRegistrationResult,
	HubTagRegistration,
	PlcConnection
} from './tagRegistryAdmin';

/** 登録できるデータ型（`banto_tags::NUMERIC_DATA_TYPES` と同じ。数値と bit）。 */
export const REGISTRABLE_DATA_TYPES: readonly string[] = [
	'bit',
	'i16',
	'u16',
	'i32',
	'u32',
	'f32',
	'i64',
	'u64',
	'f64'
];

/** 新しいグループの既定の収集周期（ms）。 */
export const DEFAULT_HUB_GROUP_PERIOD_MS = 1000;

export function isRegistrableDataType(dataType: string): boolean {
	return REGISTRABLE_DATA_TYPES.includes(dataType);
}

/** 1 行の選べる/選べないと、その理由。 */
export type HubTagAvailability =
	{ selectable: true } | { selectable: false; reason: 'registered' | 'unsupportedType' };

export function hubTagAvailability(
	tag: Pick<HubTag, 'externalName' | 'dataType'>,
	registered: ReadonlySet<string>
): HubTagAvailability {
	if (registered.has(tag.externalName)) return { selectable: false, reason: 'registered' };
	if (!isRegistrableDataType(tag.dataType)) {
		return { selectable: false, reason: 'unsupportedType' };
	}
	return { selectable: true };
}

/** 選べない理由の文言。 */
export function availabilityLabel(availability: HubTagAvailability, dataType: string): string {
	if (availability.selectable) return '';
	return availability.reason === 'registered'
		? '登録済み'
		: `${dataType} 型は記録できません（数値と bit のみ）`;
}

/**
 * 検索（外部名・名前・単位・型の部分一致、大文字小文字を区別しない）。空白区切りの
 * 語は**すべて**含む行だけを残す。並びは catalog のまま。
 */
export function filterHubTags<
	T extends Pick<HubTag, 'externalName' | 'name' | 'unit' | 'dataType'>
>(tags: readonly T[], query: string): T[] {
	const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length === 0) return [...tags];
	return tags.filter((tag) => {
		const haystack = [tag.externalName, tag.name, tag.unit ?? '', tag.dataType]
			.join(' ')
			.toLowerCase();
		return words.every((word) => haystack.includes(word));
	});
}

/** ダイアログを開いた直後の選択と、移行の注記に出すもの。 */
export interface InitialSelection {
	/** 事前にチェックする外部名（移行前の選択のうち、今の catalog で選べるもの）。 */
	selected: string[];
	/** 移行前の選択があるか（注記を出すか）。 */
	showLegacyNotice: boolean;
	/**
	 * 移行前の選択のうち、事前にチェックできなかったもの（Hub から消えた・
	 * 登録済み・記録できない型）。注記で「チェックしていません」と添える。
	 */
	legacySkipped: string[];
}

/**
 * 開いた直後の選択（純関数）。`legacyHandled` が真（このダイアログで既に 1 度
 * 扱った・破棄した）なら事前チェックしない。catalog を読めていない（`null`）
 * ときは何もチェックしない（読めるまで判断しない）。
 */
export function initialSelection(catalog: HubCatalog, legacyHandled: boolean): InitialSelection {
	const legacy = legacyHandled ? [] : catalog.legacySelection;
	if (legacy.length === 0 || catalog.tags === null) {
		return { selected: [], showLegacyNotice: legacy.length > 0, legacySkipped: [] };
	}
	const registered = new Set(catalog.registered);
	const byName = new Map(catalog.tags.map((tag) => [tag.externalName, tag]));
	const selected: string[] = [];
	const legacySkipped: string[] = [];
	for (const name of legacy) {
		const tag = byName.get(name);
		if (tag && hubTagAvailability(tag, registered).selectable) selected.push(name);
		else legacySkipped.push(name);
	}
	return { selected, showLegacyNotice: true, legacySkipped };
}

/** 移行の注記の文言。 */
export function legacyNoticeText(initial: InitialSelection): string {
	const base =
		'以前の Hub 接続の画面で選んでいたタグを、あらかじめチェックしています（以前の選択はタグとしては登録されていません）。登録すると、以前の選択は消えます。';
	if (initial.legacySkipped.length === 0) return base;
	return `${base} 次のタグは Hub に無いか、登録済みか、記録できない型のためチェックしていません: ${initial.legacySkipped.join('、')}`;
}

/** 登録先（既存の Hub グループか、新しいグループ）。 */
export type RegistrationTarget =
	| { kind: 'existing'; collectionGroupId: number | null }
	| { kind: 'new'; name: string; periodMs: number };

/**
 * Hub 接続の下の収集グループ（登録先の候補）。Hub 接続がまだ無ければ空
 * （そのときは新しいグループだけ - サーバーが Hub 接続ごと作る）。
 */
export function hubGroupsOf(
	connections: readonly PlcConnection[],
	groups: readonly CollectionGroup[]
): CollectionGroup[] {
	const hubIds = new Set(connections.filter((c) => c.protocol === 'hub').map((c) => c.id));
	return groups.filter((group) => hubIds.has(group.plcConnectionId));
}

/** 既定の登録先: Hub のグループがあれば最初のもの、無ければ新しいグループ。 */
export function defaultTarget(hubGroups: readonly CollectionGroup[]): RegistrationTarget {
	return hubGroups.length > 0
		? { kind: 'existing', collectionGroupId: hubGroups[0].id }
		: { kind: 'new', name: '', periodMs: DEFAULT_HUB_GROUP_PERIOD_MS };
}

/** 登録できない理由（ボタンを止める理由）。`null` なら登録できる。 */
export function registrationBlocker(
	selectedCount: number,
	target: RegistrationTarget
): string | null {
	if (selectedCount === 0) return '登録する Hub のタグを選んでください。';
	if (target.kind === 'existing' && target.collectionGroupId === null) {
		return '登録先の収集グループを選んでください。';
	}
	if (target.kind === 'new' && target.name.trim() === '') {
		return '新しい収集グループの名前を入力してください。';
	}
	return null;
}

/**
 * 送る内容（純関数）。行は **catalog の並び**（選んだ順ではない）で、
 * 選べない行は送らない（サーバーも断る）。
 */
export function buildRegistration(
	tags: readonly HubTag[],
	selected: ReadonlySet<string>,
	registered: ReadonlySet<string>,
	target: RegistrationTarget
): HubTagRegistration {
	const rows = tags
		.filter((tag) => selected.has(tag.externalName))
		.filter((tag) => hubTagAvailability(tag, registered).selectable)
		.map((tag) => ({
			externalName: tag.externalName,
			name: tag.name,
			dataType: tag.dataType,
			unit: tag.unit,
			decimals: tag.decimals
		}));
	return target.kind === 'existing'
		? { collectionGroupId: target.collectionGroupId, newGroup: null, tags: rows }
		: {
				collectionGroupId: null,
				newGroup: { name: target.name.trim(), periodMs: target.periodMs },
				tags: rows
			};
}

/** 行のエラーを外部名ごとに（その行の下に出す）。 */
export function rowErrorsByExternalName(
	result: Pick<HubRegistrationResult, 'rowErrors'>
): Map<string, string[]> {
	const map = new Map<string, string[]>();
	for (const row of result.rowErrors) {
		const messages = row.fieldErrors.map((error) => error.message);
		map.set(row.externalName, [...(map.get(row.externalName) ?? []), ...messages]);
	}
	return map;
}

/** 行に属さないエラーの文言（登録先・新しいグループ）。 */
export function targetErrorMessages(result: Pick<HubRegistrationResult, 'fieldErrors'>): string[] {
	return result.fieldErrors.map((error) => error.message);
}

/** 登録できたときのトースト。収集への反映は「収集を再起動」（他の編集と同じ約束）。 */
export function registrationSuccessMessage(
	result: Pick<HubRegistrationResult, 'tags' | 'groupCreated' | 'connectionCreated'>
): string {
	const parts = [`Hub のタグを ${result.tags.length} 件登録しました。`];
	if (result.connectionCreated) parts.push('Hub 接続を作成しました。');
	if (result.groupCreated) parts.push('収集グループを作成しました。');
	parts.push('収集中なら「収集を再起動」で記録が始まります。');
	return parts.join('');
}

/**
 * 未保存の入力があるか（閉じる・離れるときの確認）。チェックの変更・新しい
 * グループの名前の入力を数える。事前チェック（移行前の選択）だけなら未保存では
 * ない（利用者はまだ何もしていない）。
 */
export function dialogDirty(
	selected: ReadonlySet<string>,
	initial: readonly string[],
	target: RegistrationTarget
): boolean {
	if (target.kind === 'new' && target.name.trim() !== '') return true;
	if (selected.size !== initial.length) return true;
	return initial.some((name) => !selected.has(name));
}
