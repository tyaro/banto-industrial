/**
 * `offsetCopy.ts` の `isDeviceNameBasedName`/`offsetCopyName`/
 * `buildOffsetCopyRows`/`offsetCopyRowsToTagInputs` に対するユニットテスト
 * （`structRegistration.test.ts`/`continuousRegistration.test.ts` と同じ
 * describe/it スタイル）。
 */
import { describe, expect, it } from 'vitest';
import {
	buildOffsetCopyRows,
	isDeviceNameBasedName,
	offsetCopyName,
	offsetCopyRowsToTagInputs,
	type OffsetCopyRow
} from './offsetCopy';
import type { Tag } from './tagRegistryAdmin';

function makeTag(overrides: Partial<Tag> & Pick<Tag, 'id' | 'name' | 'address'>): Tag {
	return {
		collectionGroupId: 1,
		dataType: 'i16',
		stringLength: null,
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
		thresholdH: null,
		thresholdHh: null,
		thresholdL: null,
		thresholdLl: null,
		enabled: true,
		writable: false,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 0,
		...overrides
	};
}

describe('isDeviceNameBasedName', () => {
	it('名前がアドレスの正規形と一致すれば true（大文字）', () => {
		expect(isDeviceNameBasedName('D3000', 'D3000')).toBe(true);
	});

	it('名前が小文字でも大文字小文字を無視して true', () => {
		expect(isDeviceNameBasedName('d3000', 'D3000')).toBe(true);
	});

	it('アドレス自体が小文字表記でも true（アドレス側は parseSlmpAddress が吸収）', () => {
		expect(isDeviceNameBasedName('D3000', 'd3000')).toBe(true);
	});

	it('bit サフィックス付きアドレスは bit を含む正規形と一致するときだけ true（bit は word デバイスにのみ付与できる）', () => {
		expect(isDeviceNameBasedName('D100.5', 'D100.5')).toBe(true);
		expect(isDeviceNameBasedName('D100', 'D100.5')).toBe(false);
	});

	it('意味のある名前は false', () => {
		expect(isDeviceNameBasedName('temp01', 'D3000')).toBe(false);
		expect(isDeviceNameBasedName('pressure', 'D3000')).toBe(false);
	});

	it('Modbus 参照番号は name が address と完全一致すれば true', () => {
		expect(isDeviceNameBasedName('40001', '40001')).toBe(true);
		expect(isDeviceNameBasedName('AI40001', '40001')).toBe(false);
	});

	it('前後の空白は無視する', () => {
		expect(isDeviceNameBasedName('  D3000  ', 'D3000')).toBe(true);
	});

	it('名前またはアドレスが空なら false', () => {
		expect(isDeviceNameBasedName('', 'D3000')).toBe(false);
		expect(isDeviceNameBasedName('D3000', '')).toBe(false);
	});
});

describe('offsetCopyName', () => {
	it('デバイス名由来（大文字）は新アドレスの大文字表記になる', () => {
		const taken = new Set<string>();
		expect(offsetCopyName({ name: 'D3000', address: 'D3000' }, 'D3100', taken)).toBe('D3100');
	});

	it('デバイス名由来（小文字）は新アドレスの小文字表記になる', () => {
		const taken = new Set<string>();
		expect(offsetCopyName({ name: 'd3000', address: 'D3000' }, 'D3100', taken)).toBe('d3100');
	});

	it('末尾に数字を持つ意味名は数字を+1する', () => {
		const taken = new Set<string>();
		expect(offsetCopyName({ name: 'temp01', address: 'D3000' }, 'D3100', taken)).toBe('temp02');
	});

	it('末尾に数字を持たない意味名は2から始まる', () => {
		const taken = new Set<string>();
		expect(offsetCopyName({ name: 'pressure', address: 'D3000' }, 'D3100', taken)).toBe(
			'pressure2'
		);
	});

	it('taken と衝突する場合は衝突しない数字まで進める', () => {
		const taken = new Set<string>(['pressure2', 'pressure3']);
		expect(offsetCopyName({ name: 'pressure', address: 'D3000' }, 'D3100', taken)).toBe(
			'pressure4'
		);
	});

	it('同一バッチ内で連続生成すると taken に積まれ、次の呼び出しで衝突を避ける', () => {
		const taken = new Set<string>();
		const first = offsetCopyName({ name: 'temp01', address: 'D3000' }, 'D3100', taken);
		const second = offsetCopyName({ name: 'temp01', address: 'D3002' }, 'D3102', taken);
		expect(first).toBe('temp02');
		// 2つ目も同じ元名"temp01"から出発するので、taken に temp02 が
		// 既に入っている分だけ先に進む。
		expect(second).toBe('temp03');
	});

	it('Modbus 参照番号のデバイス名由来コピーは新しい参照番号そのものになる', () => {
		const taken = new Set<string>();
		expect(offsetCopyName({ name: '40001', address: '40001' }, '40101', taken)).toBe('40101');
	});
});

describe('buildOffsetCopyRows', () => {
	it('デバイス名由来タグをオフセットコピーすると、新アドレス名で1行生成される', () => {
		const source = makeTag({ id: 1, name: 'D3000', address: 'D3000', collectionGroupId: 10 });
		const result = buildOffsetCopyRows([source], 100, [source]);
		expect(result.errors).toEqual([]);
		expect(result.rows).toHaveLength(1);
		expect(result.rows[0]).toMatchObject({ name: 'D3100', address: 'D3100', sourceId: 1 });
	});

	it('意味名タグをオフセットコピーすると、末尾数字を進めた名前になる', () => {
		const source = makeTag({
			id: 2,
			name: 'temp01',
			address: 'D3000',
			collectionGroupId: 10,
			unit: '℃',
			decimals: 1,
			rawLo: 0,
			rawHi: 4095,
			engLo: 0,
			engHi: 400
		});
		const result = buildOffsetCopyRows([source], 100, [source]);
		expect(result.errors).toEqual([]);
		expect(result.rows).toHaveLength(1);
		expect(result.rows[0]).toMatchObject({
			name: 'temp02',
			address: 'D3100',
			unit: '℃',
			decimals: 1,
			rawLo: 0,
			rawHi: 4095,
			engLo: 0,
			engHi: 400
		});
	});

	it('デバイス番号の上限を超えるオフセットはアドレスエラーになる（行は生成されない）', () => {
		const source = makeTag({
			id: 3,
			name: 'D16777200',
			address: 'D16777200',
			collectionGroupId: 10
		});
		const result = buildOffsetCopyRows([source], 100, [source]);
		expect(result.rows).toEqual([]);
		expect(result.errors).toHaveLength(1);
		expect(result.errors[0].sourceId).toBe(3);
		expect(result.errors[0].message).toMatch(/算出できません/);
	});

	it('負のオフセットで下限を割るとアドレスエラーになる', () => {
		const source = makeTag({ id: 4, name: 'D10', address: 'D10', collectionGroupId: 10 });
		const result = buildOffsetCopyRows([source], -100, [source]);
		expect(result.rows).toEqual([]);
		expect(result.errors).toHaveLength(1);
	});

	it('オフセット0は元アドレスと重なるため衝突エラーになる（行は残る）', () => {
		const source = makeTag({ id: 5, name: 'D3000', address: 'D3000', collectionGroupId: 10 });
		const result = buildOffsetCopyRows([source], 0, [source]);
		expect(result.rows).toHaveLength(1);
		expect(result.errors.some((e) => e.sourceId === 5)).toBe(true);
	});

	it('非整数オフセットは全 source をエラーにする', () => {
		const a = makeTag({ id: 6, name: 'D10', address: 'D10' });
		const b = makeTag({ id: 7, name: 'D20', address: 'D20' });
		const result = buildOffsetCopyRows([a, b], 1.5, [a, b]);
		expect(result.rows).toEqual([]);
		expect(result.errors).toHaveLength(2);
		expect(result.errors.every((e) => e.message.includes('整数'))).toBe(true);
	});

	it('既存タグとアドレス範囲が重なるコピー先は衝突エラーになる', () => {
		const source = makeTag({ id: 8, name: 'temp01', address: 'D3000', collectionGroupId: 10 });
		const blocker = makeTag({
			id: 9,
			name: 'other',
			address: 'D3100',
			collectionGroupId: 10
		});
		const result = buildOffsetCopyRows([source], 100, [source, blocker]);
		expect(result.rows).toHaveLength(1);
		expect(result.errors.some((e) => e.sourceId === 8 && e.message.includes('other'))).toBe(true);
	});

	it('別グループの既存タグとはアドレスが重なっても衝突扱いしない', () => {
		const source = makeTag({ id: 10, name: 'temp01', address: 'D3000', collectionGroupId: 10 });
		const otherGroupTag = makeTag({
			id: 11,
			name: 'other',
			address: 'D3100',
			collectionGroupId: 99
		});
		const result = buildOffsetCopyRows([source], 100, [source, otherGroupTag]);
		expect(result.rows).toHaveLength(1);
		expect(result.errors).toEqual([]);
	});

	it('同一バッチ内で2つの選択タグが同じアドレスへ衝突するとエラーになる', () => {
		const a = makeTag({ id: 12, name: 'D3000', address: 'D3000', collectionGroupId: 10 });
		const b = makeTag({ id: 13, name: 'D3100', address: 'D3100', collectionGroupId: 10 });
		// a は +100 で D3100 に、b は +0 相当... ではなく b 自体は offset 100
		// を受けて D3200 になるので、意図的に重ならせるため offset を
		// 揃えつつ b の元アドレスを a の移動先に一致させる。
		const result = buildOffsetCopyRows([a, b], 100, [a, b]);
		// a: D3000 -> D3100 (と衝突: 既存タグ b の D3100 と重なる)
		// b: D3100 -> D3200 (衝突なし)
		expect(result.errors.some((e) => e.sourceId === 12)).toBe(true);
		expect(result.errors.some((e) => e.sourceId === 13)).toBe(false);
	});

	it('Modbus 参照番号タグのデバイス名由来コピー', () => {
		const source = makeTag({ id: 14, name: '40001', address: '40001', collectionGroupId: 10 });
		const result = buildOffsetCopyRows([source], 100, [source]);
		expect(result.errors).toEqual([]);
		expect(result.rows[0]).toMatchObject({ name: '40101', address: '40101' });
	});

	it('複数選択（デバイス名由来＋意味名）を1回のバッチで処理できる', () => {
		const a = makeTag({ id: 15, name: 'D3000', address: 'D3000', collectionGroupId: 10 });
		const b = makeTag({ id: 16, name: 'pressure', address: 'D3001', collectionGroupId: 10 });
		const result = buildOffsetCopyRows([a, b], 100, [a, b]);
		expect(result.errors).toEqual([]);
		expect(result.rows).toEqual([
			expect.objectContaining({ sourceId: 15, name: 'D3100', address: 'D3100' }),
			expect.objectContaining({ sourceId: 16, name: 'pressure2', address: 'D3101' })
		]);
	});
});

describe('offsetCopyRowsToTagInputs', () => {
	it('TagInput 相当へ変換し、単位・スケーリング・しきい値を保持する', () => {
		const rows: OffsetCopyRow[] = [
			{
				sourceId: 1,
				sourceName: 'temp01',
				sourceAddress: 'D3000',
				name: 'temp02',
				address: 'D3100',
				collectionGroupId: 10,
				dataType: 'f32',
				stringLength: undefined,
				unit: '℃',
				decimals: 1,
				rawLo: 0,
				rawHi: 4095,
				engLo: 0,
				engHi: 400,
				thresholdH: 350,
				thresholdHh: 380,
				thresholdL: null,
				thresholdLl: null,
				enabled: true,
				writable: true
			}
		];
		const inputs = offsetCopyRowsToTagInputs(rows);
		expect(inputs).toEqual([
			{
				name: 'temp02',
				collectionGroupId: 10,
				address: 'D3100',
				dataType: 'f32',
				stringLength: undefined,
				unit: '℃',
				decimals: 1,
				rawLo: 0,
				rawHi: 4095,
				engLo: 0,
				engHi: 400,
				thresholdH: 350,
				thresholdHh: 380,
				thresholdL: null,
				thresholdLl: null,
				enabled: true,
				writable: true
			}
		]);
	});
});
