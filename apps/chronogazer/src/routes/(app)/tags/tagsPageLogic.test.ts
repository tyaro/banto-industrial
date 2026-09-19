/**
 * `tagsPageLogic.ts`のユニットテスト（#391レビュー対応、Refs #383/#391）。
 * 依存ゼロの純関数なので`categories.test.ts`と同じdescribe/itスタイルで
 * 直接importできる。
 */
import { describe, expect, it } from 'vitest';
import {
	isSaveStillCurrent,
	runGuardedSave,
	isDeleteStillCurrent,
	runGuardedDelete,
	isListLoadCurrent,
	runGuardedListLoad,
	schemaWireFields,
	splitServerFieldErrors,
	joinFieldErrorMessages,
	wireFieldName,
	listSectionView,
	showsRetry,
	listRows,
	createFormGate,
	type SaveGuardToken,
	type DeleteGuardToken,
	type ListLoadState,
	type ListSectionView,
	type CreateFormGate
} from './tagsPageLogic';

// --- A: isSaveStillCurrent / runGuardedSave --------------------------------

describe('isSaveStillCurrent', () => {
	const storeA = { tag: 'storeA' };
	const storeB = { tag: 'storeB' };

	it('ID・ストアの両方が一致すれば true', () => {
		const pending: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		expect(isSaveStillCurrent(pending, { id: 1, store: storeA })).toBe(true);
	});

	it('IDが違えば false（別の行が選ばれている）', () => {
		const pending: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		expect(isSaveStillCurrent(pending, { id: 2, store: storeA })).toBe(false);
	});

	it('ストア参照が違えば false（同じIDでもフォームが作り直されている）', () => {
		const pending: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		expect(isSaveStillCurrent(pending, { id: 1, store: storeB })).toBe(false);
	});

	it('現在の選択が無い（null/undefined）場合も false', () => {
		const pending: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		expect(isSaveStillCurrent(pending, { id: null, store: storeA })).toBe(false);
		expect(isSaveStillCurrent(pending, { id: undefined, store: storeA })).toBe(false);
	});
});

/** 手動で resolve/reject を制御できる Promise（#391 レビュー A の「遅延する更新応答」を再現するため）。 */
function deferred<T>(): {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (err: unknown) => void;
} {
	let resolve!: (value: T) => void;
	let reject!: (err: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

describe('runGuardedSave', () => {
	it('一致していれば成功応答を適用できる（applied）', async () => {
		const store = { form: 'A' };
		const pending: SaveGuardToken<typeof store> = { id: 1, store };
		const outcome = await runGuardedSave(
			pending,
			Promise.resolve({ id: 1, name: 'A更新後' }),
			() => ({
				id: 1,
				store
			})
		);
		expect(outcome).toEqual({ kind: 'applied', entity: { id: 1, name: 'A更新後' } });
	});

	it('一致していれば失敗応答も適用できる（error）', async () => {
		const store = { form: 'A' };
		const pending: SaveGuardToken<typeof store> = { id: 1, store };
		const err = new Error('boom');
		const outcome = await runGuardedSave(pending, Promise.reject(err), () => ({ id: 1, store }));
		expect(outcome).toEqual({ kind: 'error', err });
	});

	// #391レビューA本体: タグAを保存中にタグBへ選択が移り、Aの応答が
	// 遅れて返ってくるシナリオ。応答後も「更新対象IDとフォーム内容が一致」
	// し、古い応答の成功・失敗が別フォームに反映されないことを固定する。
	it('保存中に別行へ選択が移ると、遅れて届く成功応答は stale-success になり適用されない', async () => {
		const storeA = { form: 'A' };
		const storeB = { form: 'B' };
		let currentId: number | null = 1;
		let currentStore: typeof storeA | typeof storeB = storeA;

		const pendingA: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		const { promise, resolve } = deferred<{ id: number; name: string }>();

		const outcomePromise = runGuardedSave(pendingA, promise, () => ({
			id: currentId,
			store: currentStore
		}));

		// Aの応答を待つ間にBの行を選択（`selectTag`相当: IDとストアの両方が変わる）。
		currentId = 2;
		currentStore = storeB;

		// Aの更新は成立して返ってくる。
		resolve({ id: 1, name: 'Aの新しい値' });
		const outcome = await outcomePromise;

		expect(outcome).toEqual({ kind: 'stale-success' });
		// Bの選択・フォームは影響を受けていないことも併せて固定する。
		expect(currentId).toBe(2);
		expect(currentStore).toBe(storeB);
	});

	it('保存中に別行へ選択が移ると、遅れて届く失敗応答は stale-error になりフォームに適用されない', async () => {
		const storeA = { form: 'A' };
		const storeB = { form: 'B' };
		let currentId: number | null = 1;
		let currentStore: typeof storeA | typeof storeB = storeA;

		const pendingA: SaveGuardToken<typeof storeA> = { id: 1, store: storeA };
		const { promise, reject } = deferred<{ id: number; name: string }>();

		const outcomePromise = runGuardedSave(pendingA, promise, () => ({
			id: currentId,
			store: currentStore
		}));

		currentId = 2;
		currentStore = storeB;

		reject(new Error('Aの名前が重複しています'));
		const outcome = await outcomePromise;

		expect(outcome).toEqual({ kind: 'stale-error' });
	});

	it('選び直した先が同じIDでも、ストアが作り直されていれば別対象として扱う', async () => {
		// selectTag() は同じ行をクリックし直した場合もフォームストアを
		// 新しいインスタンスに作り直す。ID一致だけを見ると誤って適用して
		// しまうため、ストア参照も見ることを固定する。
		const storeA1 = { form: 'A-1回目' };
		const storeA2 = { form: 'A-2回目' };
		let currentStore: typeof storeA1 | typeof storeA2 = storeA1;

		const pending: SaveGuardToken<typeof storeA1> = { id: 1, store: storeA1 };
		const { promise, resolve } = deferred<{ id: number }>();
		const outcomePromise = runGuardedSave(pending, promise, () => ({ id: 1, store: currentStore }));

		currentStore = storeA2;
		resolve({ id: 1 });

		expect(await outcomePromise).toEqual({ kind: 'stale-success' });
	});
});

// --- A': isDeleteStillCurrent / runGuardedDelete（#394 追補） ---------------

describe('isDeleteStillCurrent', () => {
	it('IDが一致すれば true', () => {
		expect(isDeleteStillCurrent({ id: 1 }, 1)).toBe(true);
	});

	it('IDが違えば false（別の行が選ばれている）', () => {
		expect(isDeleteStillCurrent({ id: 1 }, 2)).toBe(false);
	});

	it('現在の選択が無い（null/undefined）場合も false', () => {
		expect(isDeleteStillCurrent({ id: 1 }, null)).toBe(false);
		expect(isDeleteStillCurrent({ id: 1 }, undefined)).toBe(false);
	});
});

describe('runGuardedDelete', () => {
	it('一致していれば成功応答を適用できる（applied）', async () => {
		const pending: DeleteGuardToken = { id: 1 };
		const outcome = await runGuardedDelete(pending, Promise.resolve(), () => 1);
		expect(outcome).toEqual({ kind: 'applied' });
	});

	it('一致していれば失敗応答も適用できる（error）', async () => {
		const pending: DeleteGuardToken = { id: 1 };
		const err = new Error('boom');
		const outcome = await runGuardedDelete(pending, Promise.reject(err), () => 1);
		expect(outcome).toEqual({ kind: 'error', err });
	});

	// #394追補本体: 行Aの削除中に行Bへ選択が移り、Aの削除応答が遅れて
	// 返ってくるシナリオ。応答後もBの選択が保たれ（selectedX = null に
	// ならない）、Bのフォーム内容（未保存の入力）が消えないことを固定する。
	it('削除中に別行へ選択が移ると、遅れて届く成功応答は stale-success になり選択は変わらない', async () => {
		let currentId: number | null = 1;

		const pendingA: DeleteGuardToken = { id: 1 };
		const { promise, resolve } = deferred<void>();

		const outcomePromise = runGuardedDelete(pendingA, promise, () => currentId);

		// Aの削除応答を待つ間にBの行を選択する。
		currentId = 2;

		// Aの削除は成立して返ってくる。
		resolve();
		const outcome = await outcomePromise;

		expect(outcome).toEqual({ kind: 'stale-success' });
		// Bの選択は影響を受けていない（selectedX = null にされていない）。
		expect(currentId).toBe(2);
	});

	it('削除中に別行へ選択が移ると、遅れて届く失敗応答は stale-error になりトーストを出さない', async () => {
		let currentId: number | null = 1;

		const pendingA: DeleteGuardToken = { id: 1 };
		const { promise, reject } = deferred<void>();

		const outcomePromise = runGuardedDelete(pendingA, promise, () => currentId);

		currentId = 2;

		reject(new Error('この接続を使用している収集グループが1件あるため削除できません'));
		const outcome = await outcomePromise;

		expect(outcome).toEqual({ kind: 'stale-error' });
	});
});

// --- B: splitServerFieldErrors / schemaWireFields --------------------------

describe('wireFieldName', () => {
	it('接頭辞を剥がして先頭を小文字化する', () => {
		expect(wireFieldName('tagEdit', 'tagEditRawLo')).toBe('rawLo');
		expect(wireFieldName('plcCreate', 'plcCreateName')).toBe('name');
	});
});

describe('schemaWireFields', () => {
	it('スキーマのフィールド名一覧から wire フィールド名一覧を作る', () => {
		const fields = [
			{ name: 'tagEditName' },
			{ name: 'tagEditRawLo' },
			{ name: 'tagEditRawHi' },
			{ name: 'tagEditEngLo' },
			{ name: 'tagEditEngHi' }
		];
		expect(schemaWireFields('tagEdit', fields)).toEqual([
			'name',
			'rawLo',
			'rawHi',
			'engLo',
			'engHi'
		]);
	});
});

describe('splitServerFieldErrors', () => {
	const TAG_WIRE_FIELDS = [
		'name',
		'collectionGroupId',
		'address',
		'dataType',
		'rawLo',
		'rawHi',
		'engLo',
		'engHi',
		'unit',
		'decimals',
		'enabled'
	];

	it('スキーマに在るフィールドはフォーム行きになる', () => {
		const result = splitServerFieldErrors('tagEdit', TAG_WIRE_FIELDS, [
			{ field: 'name', message: '既に使用されています' }
		]);
		expect(result).toEqual({
			formErrors: [{ field: 'tagEditName', message: '既に使用されています' }],
			toastMessages: []
		});
	});

	// #391レビューB本体: `scaling`はスキーマに存在しないフィールド名だが、
	// 黙って捨てず4項目へ明示的に展開する。
	it('scaling（スキーマに無いフィールド）は4項目全部へ展開される', () => {
		const result = splitServerFieldErrors('tagEdit', TAG_WIRE_FIELDS, [
			{
				field: 'scaling',
				message: 'raw_lo/raw_hi/eng_lo/eng_hi は全て指定するか、全て未指定にしてください'
			}
		]);
		expect(result.toastMessages).toEqual([]);
		expect(result.formErrors).toEqual(
			['tagEditRawLo', 'tagEditRawHi', 'tagEditEngLo', 'tagEditEngHi'].map((field) => ({
				field,
				message: 'raw_lo/raw_hi/eng_lo/eng_hi は全て指定するか、全て未指定にしてください'
			}))
		);
	});

	it('scalingの展開先すら無いスキーマではトーストへフォールバックする', () => {
		// 4項目のうち1つもスキーマに無い（将来スキーマが変わった場合の保険）。
		const result = splitServerFieldErrors(
			'groupEdit',
			['name', 'periodMs'],
			[{ field: 'scaling', message: '想定外のフィールド' }]
		);
		expect(result.formErrors).toEqual([]);
		expect(result.toastMessages).toEqual(['想定外のフィールド']);
	});

	it('スキーマに無い未知のフィールドはトーストへフォールバックする（無表示を起こさない）', () => {
		const result = splitServerFieldErrors('tagEdit', TAG_WIRE_FIELDS, [
			{ field: 'unknownField', message: 'サーバー内部の理由' }
		]);
		expect(result.formErrors).toEqual([]);
		expect(result.toastMessages).toEqual(['サーバー内部の理由']);
	});

	it('既知フィールドと未知フィールドが混在する場合、それぞれ適切な行き先へ振り分けられる', () => {
		const result = splitServerFieldErrors('tagEdit', TAG_WIRE_FIELDS, [
			{ field: 'name', message: '既に使用されています' },
			{ field: 'scaling', message: 'raw_lo と raw_hi は異なる値にしてください' },
			{ field: 'unknownField', message: 'サーバー内部の理由' }
		]);
		expect(result.formErrors).toEqual([
			{ field: 'tagEditName', message: '既に使用されています' },
			{ field: 'tagEditRawLo', message: 'raw_lo と raw_hi は異なる値にしてください' },
			{ field: 'tagEditRawHi', message: 'raw_lo と raw_hi は異なる値にしてください' },
			{ field: 'tagEditEngLo', message: 'raw_lo と raw_hi は異なる値にしてください' },
			{ field: 'tagEditEngHi', message: 'raw_lo と raw_hi は異なる値にしてください' }
		]);
		expect(result.toastMessages).toEqual(['サーバー内部の理由']);
	});
});

// --- C: joinFieldErrorMessages ---------------------------------------------

describe('joinFieldErrorMessages', () => {
	it('validation（field_errors 1件）はそのメッセージをそのまま返す', () => {
		expect(
			joinFieldErrorMessages([
				{ field: 'id', message: 'この接続を使用している収集グループが3件あるため削除できません' }
			])
		).toBe('この接続を使用している収集グループが3件あるため削除できません');
	});

	it('field_errors が複数あれば " / " で区切って連結する', () => {
		expect(
			joinFieldErrorMessages([
				{ field: 'name', message: '既に使用されています' },
				{ field: 'periodMs', message: '周期は許可された値のいずれかです' }
			])
		).toBe('既に使用されています / 周期は許可された値のいずれかです');
	});

	it('field_errors が空なら空文字列', () => {
		expect(joinFieldErrorMessages([])).toBe('');
	});
});

// --- D: 一覧の3状態（未読込 / 読み込み失敗 / 読み込めて0件） ---------------

describe('listSectionView / showsRetry / createFormGate', () => {
	interface Row {
		id: number;
	}
	const ROW: Row = { id: 1 };

	/**
	 * 状態の総当たり表（#394レビュー P1-3）。列は「一覧セクションの見せ方」
	 * 「再試行の導線を出すか」「その一覧に依存する作成フォームの出し方」。
	 * ここが崩れると「読めていない」が「0件」に化けて、誤った案内が出る。
	 */
	const table: {
		label: string;
		state: ListLoadState<Row>;
		view: ListSectionView;
		retry: boolean;
		gate: CreateFormGate;
	}[] = [
		{
			label: '未読込（まだ読んでいない）',
			state: { items: null, error: null },
			view: 'loading',
			retry: false,
			gate: 'dependency-loading'
		},
		{
			label: '読み込み失敗（一度も読めていない）',
			state: { items: null, error: '接続できません' },
			view: 'failed',
			retry: true,
			gate: 'dependency-failed'
		},
		{
			label: '読めて0件',
			state: { items: [], error: null },
			view: 'grid',
			retry: false,
			gate: 'needs-prerequisite'
		},
		{
			label: '読めて1件以上',
			state: { items: [ROW], error: null },
			view: 'grid',
			retry: false,
			gate: 'form'
		},
		{
			label: '読めていたが再取得に失敗（前回の内容が残っている）',
			state: { items: [ROW], error: '接続できません' },
			view: 'grid',
			retry: true,
			gate: 'form'
		}
	];

	for (const row of table) {
		it(`${row.label}: view=${row.view} / retry=${row.retry} / gate=${row.gate}`, () => {
			expect(listSectionView(row.state)).toBe(row.view);
			expect(showsRetry(row.state)).toBe(row.retry);
			expect(createFormGate(row.state)).toBe(row.gate);
		});
	}

	it('読み込み失敗を「0件」に潰さない（グリッドを描かず、案内も出さない）', () => {
		const failed: ListLoadState<Row> = { items: null, error: '接続できません' };
		expect(listSectionView(failed)).not.toBe('grid');
		// 「先に○○を作成してください」に相当するゲートは、読めて0件のときだけ。
		expect(createFormGate(failed)).not.toBe('needs-prerequisite');
	});

	it('listRows は未読込でも空配列を返す（グリッド自体は描かない前提）', () => {
		expect(listRows<Row>({ items: null, error: null })).toEqual([]);
		expect(listRows<Row>({ items: [ROW], error: null })).toEqual([ROW]);
	});
});

// --- A'': isListLoadCurrent / runGuardedListLoad（レビュー P2-C） -----------

describe('isListLoadCurrent', () => {
	it('自分が最新の再取得なら適用してよい', () => {
		expect(isListLoadCurrent(3, 3)).toBe(true);
	});

	it('あとから別の再取得が始まっていれば適用しない（古い一覧で巻き戻さない）', () => {
		expect(isListLoadCurrent(3, 4)).toBe(false);
	});
});

describe('runGuardedListLoad', () => {
	it('世代が変わっていなければ取得した一覧を適用する', async () => {
		const generation = 1;
		const outcome = await runGuardedListLoad(
			generation,
			Promise.resolve([{ id: 1 }]),
			() => generation
		);
		expect(outcome).toEqual({ kind: 'applied', items: [{ id: 1 }] });
	});

	it('遅延した応答の最中に新しい再取得が完了すると、古い応答は捨てられる', async () => {
		// 実際の再現: 作成 → 保存 と続けて操作すると GET が 2 本飛び、
		// 着順は発行順と一致しない。古い方が後に着いて一覧を巻き戻していた。
		let generation = 0;
		const slow = deferred<{ id: number }[]>();
		const fast = deferred<{ id: number }[]>();

		generation += 1;
		const first = runGuardedListLoad(generation, slow.promise, () => generation);

		generation += 1;
		const second = runGuardedListLoad(generation, fast.promise, () => generation);

		// 新しい方が先に着く（新しい一覧）。
		fast.resolve([{ id: 1 }, { id: 2 }]);
		expect(await second).toEqual({ kind: 'applied', items: [{ id: 1 }, { id: 2 }] });

		// 古い方があとから着く（更新前のスナップショット）。適用してはいけない。
		slow.resolve([{ id: 1 }]);
		expect(await first).toEqual({ kind: 'stale' });
	});

	it('世代が変わっていなければ失敗はそのまま返る（失敗の表示と再試行の導線は出す）', async () => {
		const generation = 5;
		const boom = new Error('boom');
		const outcome = await runGuardedListLoad<{ id: number }>(
			generation,
			Promise.reject(boom),
			() => generation
		);
		expect(outcome).toEqual({ kind: 'error', err: boom });
	});

	it('古い再取得の失敗は表示しない（新しい再取得の結果を上書きしない）', async () => {
		let generation = 1;
		const slow = deferred<{ id: number }[]>();
		const first = runGuardedListLoad(generation, slow.promise, () => generation);

		generation += 1; // 新しい再取得が始まった
		slow.reject(new Error('古い方が失敗した'));

		expect(await first).toEqual({ kind: 'stale' });
	});
});
