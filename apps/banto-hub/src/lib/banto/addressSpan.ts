/**
 * R2b-6d-2a（docs/banto-hub-driver-ui-design.md §3.6・§4.3・§7）: アドレスの位置と占有の
 * 比べ方、REST（`parse-addresses` / `offset-addresses`）を呼ぶ薄い層の共通部分。
 *
 * アドレスの文法（デバイス名・基数・ビットの軸）を TS に写さない。連続登録・オフセットコピー・
 * 構造体登録は、ドライバが返す `area`・`index`・`wordCount` だけで「重なり」を判定する:
 *
 * - 同じ `area` で、`[index, index + wordCount - 1]` が重なれば重なる。`index` は `bit` を
 *   含まない（ワード内ビットはそのワードの番号）ので、ワード内ビットは `bit` を見ずに
 *   ワードを占め、同じワードの別のビットどうしも重なる（見落としを避ける安全側）。
 * - `index` が `null`（数値の位置を持たないドライバ）・`area` が無いものは `canonical` の一致だけ。
 *
 * 純関数（{@link spanOfParsed}・{@link spansOverlap}）と、REST を呼ぶ薄い層
 * （{@link fetchExistingSpans}。呼び出しは {@link AddressApi} で差し替えられる）に分けてある。
 */
import {
	MAX_ADDRESS_ITEMS_PER_REQUEST,
	offsetTagAddresses,
	parseTagAddresses,
	type ParsedAddress,
	type StringEncoding,
	type TagDataType,
	type TagKind
} from './tagRegistryAdmin';

/** アドレスが占める範囲（ドライバが返した値）。 */
export interface AddressSpan {
	canonical: string;
	area: string | null;
	index: number | null;
	wordCount: number;
}

/** 解析に成功した結果から {@link AddressSpan} を作る。失敗は `null`。 */
export function spanOfParsed(parsed: ParsedAddress | undefined): AddressSpan | null {
	if (!parsed?.ok) return null;
	return {
		canonical: parsed.canonical,
		area: parsed.area,
		index: parsed.index,
		wordCount: Math.max(1, parsed.wordCount ?? 1)
	};
}

/** 2 つの占有が重なるか（上のコメントの規則）。 */
export function spansOverlap(a: AddressSpan, b: AddressSpan): boolean {
	if (a.area !== null && b.area !== null && a.area !== b.area) return false;
	if (a.area !== null && a.area === b.area && a.index !== null && b.index !== null) {
		return a.index <= b.index + b.wordCount - 1 && b.index <= a.index + a.wordCount - 1;
	}
	return a.canonical === b.canonical;
}

/** REST を呼ぶ口。テストでは偽の実装に差し替える。 */
export interface AddressApi {
	parse: typeof parseTagAddresses;
	offset: typeof offsetTagAddresses;
}

export const realAddressApi: AddressApi = {
	parse: parseTagAddresses,
	offset: offsetTagAddresses
};

/** 配列を `size` 件ずつに分ける。 */
export function chunked<T>(items: readonly T[], size: number): T[][] {
	const out: T[][] = [];
	for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
	return out;
}

/** {@link fetchExistingSpans} が既存タグに要求する最小限の形。 */
export interface ExistingTagForSpan {
	id: number;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	stringLength: number | null;
	stringEncoding: StringEncoding;
	tagKind: TagKind;
}

/** {@link existingTagsKey} が見る既存タグの形（衝突判定に効く欄だけ）。 */
export interface ExistingTagForKey extends ExistingTagForSpan {
	name: string;
}

/**
 * 構造体登録・オフセットコピーの衝突判定が頼る既存タグの内容のキー（R2b-6d-2a、レビュー P2）。
 * 対象の収集グループのタグだけを、衝突判定に効く欄（`id`・`name`・`collectionGroupId`・`address`・
 * `dataType`・`stringLength`・`stringEncoding`・`tagKind`）に絞って id の順に並べた文字列を返す。
 * 一覧を読み直しても内容が同じなら同じ文字列（再計算しない）、別のクライアントがタグを足す・動かす・
 * 消す・書き換えると変わる（判定をやり直す）。
 */
export function existingTagsKey(
	tags: readonly ExistingTagForKey[],
	groupIds: ReadonlySet<number>
): string {
	return JSON.stringify(
		tags
			.filter((t) => groupIds.has(t.collectionGroupId))
			.sort((a, b) => a.id - b.id)
			.map((t) => [
				t.id,
				t.name,
				t.collectionGroupId,
				t.address,
				t.dataType,
				t.stringLength,
				t.stringEncoding,
				t.tagKind
			])
	);
}

/**
 * 既存タグ全件の占有を、`parse-addresses` で求める（接続ごとにまとめ、上限
 * {@link MAX_ADDRESS_ITEMS_PER_REQUEST} 件を超えるときは分けて呼ぶ）。`plc` 以外のタグと、
 * 接続の分からないタグは対象外（結果の Map に入れない）。解析できなかったタグは
 * `canonical` を保存済みの文字列にした位置無しの占有にする（アドレスの完全一致だけ見る）。
 * 通信の失敗は例外（呼び出し側は「判定不能」として扱い、衝突なしに倒さない）。
 */
export async function fetchExistingSpans(
	tags: readonly ExistingTagForSpan[],
	connectionIdForGroup: (collectionGroupId: number) => number | undefined,
	api: Pick<AddressApi, 'parse'> = realAddressApi
): Promise<Map<number, AddressSpan>> {
	const byConnection = new Map<number, ExistingTagForSpan[]>();
	for (const tag of tags) {
		if (tag.tagKind !== 'plc') continue;
		const connectionId = connectionIdForGroup(tag.collectionGroupId);
		if (connectionId === undefined) continue;
		const list = byConnection.get(connectionId) ?? [];
		list.push(tag);
		byConnection.set(connectionId, list);
	}
	const spans = new Map<number, AddressSpan>();
	for (const [connectionId, list] of byConnection) {
		for (const part of chunked(list, MAX_ADDRESS_ITEMS_PER_REQUEST)) {
			const results = await api.parse(
				connectionId,
				part.map((t) => ({
					tagKind: t.tagKind,
					address: t.address,
					dataType: t.dataType,
					stringLength: t.stringLength,
					stringEncoding: t.stringEncoding
				}))
			);
			part.forEach((tag, i) => {
				spans.set(
					tag.id,
					spanOfParsed(results[i]) ?? {
						canonical: tag.address,
						area: null,
						index: null,
						wordCount: 1
					}
				);
			});
		}
	}
	return spans;
}
