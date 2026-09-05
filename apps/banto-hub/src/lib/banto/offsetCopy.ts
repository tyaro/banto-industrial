/**
 * T20 機能②b オフセットコピー（docs/banto-hub-t20-design.md §3.2、
 * 2026-09-04 オーナー決定「テンプレートの永続保存はしないが、既存タグ群
 * （＝構造体1インスタンス）からのオフセットコピーは有用」）。
 *
 * 既に登録済みの複数タグ（画面のタグ一覧で複数選択したもの）を、アドレスに
 * 一定オフセット（ワード数）を加えて複製する。構造体登録②a
 * （`structRegistration.ts`）が「1つのベースアドレスから複数フィールドを
 * 新規に割り付ける」のに対し、こちらは「既存タグ群をまるごと平行移動して
 * 複製する」— アドレス算術・衝突検出は②a/連続登録の実装をそのまま再利用し、
 * 本ファイルは「コピー先の名前をどう決めるか」（{@link offsetCopyName}）と
 * 「複数selectionを一度に処理する」({@link buildOffsetCopyRows})だけを追加する。
 *
 * ## 命名ルール（オーナー決定、2026-09-05）
 *
 * コピー先タグの名前は、元タグ名の性質に応じて**タグごとに適応的に**決める:
 *
 * - **デバイス名由来**（{@link isDeviceNameBasedName}）: 名前がそのアドレスの
 *   デバイス表記そのもの（例 アドレス `D3000` に対し名前 `D3000`/`d3000`）
 *   の場合 → コピー先の**新アドレスのデバイス名**にする（例 +100 で
 *   `D3100`/`d3100`。元の大文字/小文字の流儀を踏襲）。この場合、コピー先の
 *   名前は「そのアドレスのデバイス名」という意味を保つことが利用者の直感に
 *   合うため、既存タグ名との衝突があっても**名前をずらして回避したりしない**
 *   — 衝突はそのまま {@link buildOffsetCopyRows} のエラーとして報告し、
 *   利用者に判断させる（アドレス自体をずらすか、コピー元を見直すか）。
 * - **それ以外**（意味のある構造体名、例 `temp01`/`pressure`）の場合 →
 *   **名前の末尾に数字を付ける**形でコピーする。末尾が既に数字なら+1した
 *   値を、末尾に数字が無ければ `2` から開始し、`taken`（既存タグ名＋この
 *   一括で既に採った名前）と衝突しない最初の値まで数字を進める（この branch
 *   だけが積極的な衝突回避を行う — 数字を進めるだけで簡単に回避できる上、
 *   構造体を大量複製する運用でコピーごとに手直しさせないため）。
 *
 * ## オフセットの扱い（2026-09-05 オーナー決定「正の整数のみ」）
 *
 * `offset` は **1以上の整数ワード数のみ**を仕様とする。0・負・非整数は
 * いずれも不正な入力として {@link buildOffsetCopyRows} が**先頭で**弾き、
 * source を1件も処理せず（アドレス算出も衝突検出も行わない）、全 source を
 * 対象にした一律のエラー（「オフセットは1以上の整数で指定してください。」
 * 相当のメッセージ）を返す（`rows` は空）:
 * - `offset === 0` は「ずらさない」＝新アドレスが元アドレスと同一になり
 *   衝突検出でも結果的に検出できるが、それに頼らず明示的に弾く。
 * - 負のオフセットは、小さな負値（例 `-1`）だとデバイス番号が下限
 *   （0未満）を割らずに `incrementAddress` が成功してしまう場合があり、
 *   衝突検出にも引っかからない可能性があるため、アドレス算出に委ねず
 *   明示的に弾く。
 * - 非整数（例 `1.5`）は算術として無意味なので、これも同じ入口で弾く。
 * - UI 側（`+page.svelte` のオフセットコピーパネル）も `<input min="1">`
 *   に加え、JS 側の disabled 条件・エラー表示でこの仕様を弾く（本体は
 *   この JS 側のバリデーション - `min` 属性は実行時の入力を防がない）。
 *
 * **ビット付きアドレス（例 `M100.5`）の注意**: `incrementAddress` は
 * bit サフィックス付きアドレスを「1ワード=16bit の1本の数直線」として
 * 扱うため、そのようなタグに対する `offset` は実質「ビット位置を offset
 * 個分進める」ことになる（ワード単位の移動にはならない）。オフセット
 * コピーの主用途はワードアドレス（構造体のフィールド群）の複製のため、
 * 本実装ではこの分岐を特別に禁止しない（`incrementAddress` の既存の
 * 意味論をそのまま踏襲する）。
 */
import type { StringEncoding, Tag, TagDataType, TagInput } from './tagRegistryAdmin';
import { addressIncrement, incrementAddress } from './continuousRegistration';
import { detectStructAddressCollisions, type StructAllocatedRow } from './structRegistration';
import { formatSlmpAddress, parseSlmpAddress } from './slmpDeviceTable';

/**
 * タグ名が、そのアドレスの「デバイス表記そのもの」由来かどうかを判定する。
 *
 * - `address` が SLMP デバイス記法として解釈できる場合: `name`（前後空白を
 *   除き、大文字小文字を無視）が、そのアドレスの正規形（`formatSlmpAddress`
 *   が返す表記 - bit サフィックス付きならそれも含む、例 `M100.5`）と一致
 *   するかどうか。
 * - `address` が SLMP として解釈できない場合（Modbus 参照番号 `40001` 等）:
 *   `address` 自体が数字のみで構成されているとき、`name`（前後空白を除く）
 *   が `address` と完全一致するかどうか（Modbus 参照番号には大文字小文字の
 *   概念が無いため大文字小文字は区別してよい - 数字しか含まれないため実質
 *   意味を持たない）。
 * - どちらにも該当しなければ `false`（意味のある構造体名として扱う）。
 */
export function isDeviceNameBasedName(name: string, address: string): boolean {
	const trimmedName = name.trim();
	const trimmedAddress = address.trim();
	if (trimmedName === '' || trimmedAddress === '') return false;

	const parsed = parseSlmpAddress(trimmedAddress);
	if (parsed) {
		const canonical = formatSlmpAddress(parsed.mnemonic, parsed.number, parsed.bit);
		return trimmedName.toUpperCase() === canonical.toUpperCase();
	}

	if (/^\d+$/.test(trimmedAddress)) {
		return trimmedName === trimmedAddress;
	}

	return false;
}

/** 名前の非数字部分がすべて小文字（＝大文字を1つも含まない）かどうか。 */
function isLowerCaseStyle(name: string): boolean {
	const trimmed = name.trim();
	return trimmed === trimmed.toLowerCase();
}

/**
 * 「末尾に数字を付ける」ブランチの本体。末尾が既に10進数字列なら+1した値を
 * （元の桁数を `padStart` で維持する - 例 `temp01` → `temp02`）、末尾に
 * 数字が無ければ `2` から開始し、`taken` と衝突しない最初の値まで数字を
 * 1ずつ進める。`taken` に生成した名前を追加する（呼び出し元が後続の
 * 呼び出しでも衝突を避け続けられるようにするための意図的な副作用）。
 */
function nextAvailableNumberedName(baseName: string, taken: Set<string>): string {
	const trimmed = baseName.trim();
	const match = /^(.*?)(\d+)$/.exec(trimmed);
	const prefix = match ? match[1] : trimmed;
	const digits = match ? match[2] : null;
	const width = digits ? digits.length : 0;
	let n = digits ? Number.parseInt(digits, 10) + 1 : 2;
	let candidate = `${prefix}${String(n).padStart(width, '0')}`;
	while (taken.has(candidate)) {
		n += 1;
		candidate = `${prefix}${String(n).padStart(width, '0')}`;
	}
	taken.add(candidate);
	return candidate;
}

/**
 * コピー先タグの名前を1件ぶん決める（ドキュメント冒頭「命名ルール」参照）。
 *
 * `taken` は「衝突とみなす名前の集合」で、この関数は生成した名前を
 * **`taken` に追加する副作用を持つ**（同一バッチ内の後続呼び出しが、
 * この呼び出しで採った名前を衝突として認識できるようにするため）。
 * デバイス名由来ブランチは `taken` を消費しない（衝突回避のための
 * 書き換えを行わない設計 - ドキュメント冒頭参照）が、生成した名前は
 * 後続呼び出しのために `taken` へ追加する。
 */
export function offsetCopyName(
	source: { name: string; address: string },
	newAddress: string,
	taken: Set<string>
): string {
	if (isDeviceNameBasedName(source.name, source.address)) {
		const candidate = isLowerCaseStyle(source.name)
			? newAddress.toLowerCase()
			: newAddress.toUpperCase();
		taken.add(candidate);
		return candidate;
	}
	return nextAvailableNumberedName(source.name, taken);
}

/** {@link buildOffsetCopyRows} が生成する1行分。プレビュー表示と
 * {@link offsetCopyRowsToTagInputs} への変換の両方に使う共通の中間表現。 */
export interface OffsetCopyRow {
	/** コピー元タグの id（プレビューでの対応付け・エラーとの突き合わせに使う）。 */
	sourceId: number;
	sourceName: string;
	sourceAddress: string;
	name: string;
	address: string;
	collectionGroupId: number;
	dataType: TagDataType;
	stringLength?: number | null;
	stringEncoding?: StringEncoding | null;
	unit?: string | null;
	decimals: number;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	thresholdH?: number | null;
	thresholdHh?: number | null;
	thresholdL?: number | null;
	thresholdLl?: number | null;
	enabled: boolean;
	writable: boolean;
}

/** {@link buildOffsetCopyRows} が返す1件分のエラー。`sourceId` で対応する
 * {@link OffsetCopyRow}（存在すれば）と突き合わせられる - アドレス算出
 * 自体に失敗した場合は対応する行が無い。 */
export interface OffsetCopyError {
	sourceId: number;
	sourceName: string;
	sourceAddress: string;
	message: string;
}

export interface OffsetCopyResult {
	rows: OffsetCopyRow[];
	errors: OffsetCopyError[];
}

/**
 * 選択済みタグ群 `sources` を、アドレスに `offset` ワード分を加えて複製する
 * ための行を組み立てる。
 *
 * - 各 source について `newAddress = incrementAddress(source.address, 1,
 *   offset)`。`null`（範囲外・形式未対応）は対応する行を作らずエラーにする。
 * - `newName = offsetCopyName(source, newAddress, taken)` - `taken` は
 *   コピー先と同じ収集グループ（`source.collectionGroupId`）内の既存タグ名
 *   ＋この一括で既に採った名前（グループ単位でスコープする - タグ名は
 *   グループ内一意という既存の前提 `structRegistration.ts` 参照）。
 * - コピー先グループは source と同じ `collectionGroupId`。型・
 *   `stringLength`・`writable` に加え、単位・スケーリング（raw/eng）・
 *   しきい値も source から引き継ぐ（`tagDuplicate.ts::buildDuplicateFormValues`
 *   の「複製は名前とアドレスのみ変更、他は引き継ぐ」という既存作法に
 *   合わせる - 構造体登録②aの `structRowsToTagInputs` が引き継がない
 *   のとは意図的に異なる。あちらは「新規フィールド定義」なのでスケーリング
 *   概念が無いのに対し、オフセットコピーは「既存タグの複製」なので
 *   複製と同じ作法にする）。
 * - **衝突検出**: 生成した行を収集グループ単位でまとめ、②aの
 *   {@link detectStructAddressCollisions} をそのまま再利用してアドレス
 *   範囲の重なり・名前の重複（グループ内の既存タグとの重複、および
 *   このバッチ内の行同士の重複）を検出する。検出された行は `rows` には
 *   残したまま（プレビュー表に「行として」出し、警告を添えられるように
 *   - UI 側は `errors` に同じ `sourceId` があるかで警告表示を判断する）、
 *   `errors` に追記する。
 */
export function buildOffsetCopyRows(
	sources: Tag[],
	offset: number,
	existingTags: Tag[]
): OffsetCopyResult {
	const rows: OffsetCopyRow[] = [];
	const errors: OffsetCopyError[] = [];

	if (!Number.isInteger(offset) || offset < 1) {
		for (const source of sources) {
			errors.push({
				sourceId: source.id,
				sourceName: source.name,
				sourceAddress: source.address,
				message: 'オフセットは1以上の整数で指定してください。'
			});
		}
		return { rows, errors };
	}

	const takenByGroup = new Map<number, Set<string>>();
	function takenSetFor(groupId: number): Set<string> {
		let set = takenByGroup.get(groupId);
		if (!set) {
			set = new Set(existingTags.filter((t) => t.collectionGroupId === groupId).map((t) => t.name));
			takenByGroup.set(groupId, set);
		}
		return set;
	}

	for (const source of sources) {
		const newAddress = incrementAddress(source.address, 1, offset);
		if (newAddress === null) {
			errors.push({
				sourceId: source.id,
				sourceName: source.name,
				sourceAddress: source.address,
				message: `アドレス "${source.address}" から ${offset >= 0 ? '+' : ''}${offset} 先のアドレスを算出できません（形式が未対応か、デバイス番号の範囲外です）。`
			});
			continue;
		}

		const taken = takenSetFor(source.collectionGroupId);
		const name = offsetCopyName(source, newAddress, taken);

		rows.push({
			sourceId: source.id,
			sourceName: source.name,
			sourceAddress: source.address,
			name,
			address: newAddress,
			collectionGroupId: source.collectionGroupId,
			dataType: source.dataType,
			stringLength: source.dataType === 'string' ? source.stringLength : undefined,
			stringEncoding: source.dataType === 'string' ? source.stringEncoding : undefined,
			unit: source.unit,
			decimals: source.decimals,
			rawLo: source.rawLo,
			rawHi: source.rawHi,
			engLo: source.engLo,
			engHi: source.engHi,
			thresholdH: source.thresholdH,
			thresholdHh: source.thresholdHh,
			thresholdL: source.thresholdL,
			thresholdLl: source.thresholdLl,
			enabled: source.enabled,
			writable: source.writable
		});
	}

	// 衝突検出は収集グループ単位（detectStructAddressCollisions の契約）。
	const rowIndicesByGroup = new Map<number, number[]>();
	rows.forEach((row, index) => {
		const list = rowIndicesByGroup.get(row.collectionGroupId);
		if (list) {
			list.push(index);
		} else {
			rowIndicesByGroup.set(row.collectionGroupId, [index]);
		}
	});

	for (const [groupId, indices] of rowIndicesByGroup) {
		const groupRows: StructAllocatedRow[] = indices.map((i) => {
			const row = rows[i];
			return {
				name: row.name,
				address: row.address,
				dataType: row.dataType,
				stringLength: row.stringLength,
				words: addressIncrement(row.dataType, row.stringLength)
			};
		});
		const collisions = detectStructAddressCollisions(groupRows, existingTags, groupId);
		for (const collision of collisions) {
			const row = rows[indices[collision.index]];
			errors.push({
				sourceId: row.sourceId,
				sourceName: row.sourceName,
				sourceAddress: row.sourceAddress,
				message: collision.message
			});
		}
	}

	return { rows, errors };
}

/**
 * {@link buildOffsetCopyRows} の行を `POST /api/tags/batch`
 * （{@link createTagsBatch}）に渡せる {@link TagInput} 配列へ変換する。
 * 構造体登録②aの `structRowsToTagInputs` とは異なり、単位・スケーリング・
 * しきい値も引き継ぐ（上の doc コメント参照）。
 */
export function offsetCopyRowsToTagInputs(rows: OffsetCopyRow[]): TagInput[] {
	return rows.map((row) => ({
		name: row.name,
		collectionGroupId: row.collectionGroupId,
		address: row.address,
		dataType: row.dataType,
		stringLength: row.stringLength,
		stringEncoding: row.stringEncoding ?? undefined,
		unit: row.unit,
		decimals: row.decimals,
		rawLo: row.rawLo,
		rawHi: row.rawHi,
		engLo: row.engLo,
		engHi: row.engHi,
		thresholdH: row.thresholdH,
		thresholdHh: row.thresholdHh,
		thresholdL: row.thresholdL,
		thresholdLl: row.thresholdLl,
		enabled: row.enabled,
		writable: row.writable
	}));
}
