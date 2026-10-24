/**
 * テスト用: REST（`parse-addresses` / `offset-addresses`）の偽。SLMP の `D` 系（10 進のワード・
 * `.N` のビット）を真似る。本物の規則（16 進のデバイス・Modbus・範囲外）は Rust 側の
 * 統合テスト（`apps/banto-hub/host/tests/offset_addresses.rs`）が確かめる。
 * 本番のコードからは import しない（`*.test.ts` からだけ）。
 */
import type { AddressApi } from './addressSpan';
import type { OffsetAddressItem, ParsedAddress, ParseAddressItem } from './tagRegistryAdmin';

const WORDS: Record<string, number> = {
	bit: 1,
	i16: 1,
	u16: 1,
	i32: 2,
	u32: 2,
	f32: 2,
	i64: 4,
	u64: 4,
	f64: 4
};

function wordCountOf(item: { dataType: string; stringLength?: number | null }): number {
	return item.dataType === 'string' ? (item.stringLength ?? 1) : (WORDS[item.dataType] ?? 1);
}

/** `D100`・`d100.5` のような `D` 系のアドレスを解析した結果の偽。 */
export function fakeParse(
	item: Pick<ParseAddressItem, 'address' | 'dataType' | 'stringLength' | 'tagKind'>
): ParsedAddress {
	const m = /^([A-Za-z]+)(\d+)(?:\.([0-9A-Fa-f]))?$/.exec(item.address.trim());
	if (!m) return { ok: false, message: `アドレス "${item.address}" を解釈できません` };
	const area = m[1].toUpperCase();
	const index = Number(m[2]);
	const bit = m[3] === undefined ? null : Number.parseInt(m[3], 16);
	return {
		ok: true,
		canonical: `${area}${index}${bit === null ? '' : `.${m[3].toUpperCase()}`}`,
		area,
		writableArea: true,
		index,
		bit,
		wordCount: wordCountOf(item)
	};
}

export type FakeAddressApi = AddressApi & {
	parseCalls: { connectionId: number; items: ParseAddressItem[] }[];
	offsetCalls: OffsetAddressItem[][];
};

/** 呼び出しを記録する偽の API。`points` は型のワード数 × n、`words` は n ワード動かす。 */
export function fakeAddressApi(): FakeAddressApi {
	const parseCalls: FakeAddressApi['parseCalls'] = [];
	const offsetCalls: OffsetAddressItem[][] = [];
	return {
		parseCalls,
		offsetCalls,
		parse: async (connectionId, items) => {
			parseCalls.push({ connectionId, items });
			return items.map(fakeParse);
		},
		offset: async (items) => {
			offsetCalls.push(items);
			return items.map((item) => {
				const base = fakeParse({ ...item, tagKind: item.tagKind ?? 'plc' });
				if (!base.ok || base.index === null) return base;
				const step = item.unit === 'points' ? item.n * (base.wordCount ?? 1) : item.n;
				const index = base.index + step;
				const bitSuffix = base.bit === null ? '' : `.${base.bit.toString(16).toUpperCase()}`;
				return { ...base, canonical: `${base.area}${index}${bitSuffix}`, index };
			});
		}
	};
}
