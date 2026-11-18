# 画面の編集履歴（Undo/Redo）の作り方（recipe）

作成日: 2026-11-17
状態: **記録（共通部品は作らない）**。2026-11-17 のオーナー決定で、#762 の評価の結果を recipe として残して #762 を閉じる。banto の部品にはしない。

関連: Issue #762（評価の経緯と根拠）、SCADA の `apps/banto-scada/src/lib/history.ts`・`history.test.ts`（tyaro/temp-scada、`3fa85885` で確認）。

## 1. この文書の位置

画面の手元の下書きに Undo/Redo を付けるときの型を残す。2026-11-17 の時点で使っているのは SCADA だけで、banto（v7.2.0）にも Industrial にも同じ物は無い。
共通部品にしない理由（#762 の評価）は次のとおり。

- 共通にできるのは「snapshot の列・上限・失敗したら何も変えない差し替え」の約 75 行だけ。
- 価値の大半は、使う側が決めること（§3）に残る。
- ChronoGazer の表示グループの下書きに当ててみたが、owner も revision も無く、うまく合わなかった。Undo が要るという要望もまだ無い。

**見直す条件**: 使う側が 2 つそろい、実際の要望が出たとき。そのときは banto に §2 の `createHistory` と `HistoryAdapter` の型だけを提案し、§4 の試験を移す。

banto v7.1/v7.2 の「レコード履歴」は、監査ログを対象のレコードで絞って見せるタブで、この文書の編集履歴とは別物。

## 2. 汎用部の形

#762 の評価で、domain に依存しない形に書き直した物。SCADA の振る舞いはすべてこれで表せた（vitest で 14 本）。

```ts
interface EditHistory {
	past: string[]; // 古い順の snapshot
	future: string[];
}

interface HistoryAdapter<T> {
	serialize(value: T): string;
	/** 検証込み。不正なら throw。毎回新しい物を返す。 */
	parse(text: string): T;
	sameOwner(a: T, b: T): boolean;
	/** 既定は参照の同一性。 */
	isNoop?(prev: T, next: T): boolean;
	/** 復元したときの後処理（revision +1 など）。結果は parse で検証し直す。 */
	onRestore?(restored: T, current: T): T;
}

function createHistory<T>(
	adapter: HistoryAdapter<T>,
	limit = 50
): {
	record(
		value: T,
		history: EditHistory,
		edit: (v: T) => T,
		mode: 'edit' | 'preview'
	): { value: T; history: EditHistory };
	travel(
		value: T,
		history: EditHistory,
		direction: 'undo' | 'redo',
		mode: 'edit' | 'preview'
	): { value: T; history: EditHistory };
};
```

振る舞いの約束:

- `record`
  - `edit` が throw したら（無効な編集）、値も履歴も変えない。
  - no-op（`isNoop`）なら履歴に積まない。
  - 積んだら `future` を空にし、`past` は上限で古い物から捨てる。
- `travel`
  - 戻る先が無ければ、何もしない。
  - snapshot を `parse` で検証し、`sameOwner` で別の対象の履歴を断る。`onRestore` の結果も検証し直す。
  - どこで失敗しても、渡された値と履歴は変えない（新しい値と履歴を返すだけで、引数を書き換えない）。
- preview のときは、どちらも断る。SCADA では `record` の mode は `applyCommand` の中で見ている。

## 3. 使う側が決めること

| 決めること                       | SCADA の答え                                                                   | ChronoGazer の表示グループで当てたときの注意                                                                                                                 |
| -------------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 何を 1 つの対象とするか（owner） | Project（`manifest.projectId`）                                                | 下書きに id が無い。履歴は「1 つのグループを開いている間」だけ意味を持つので、`sameOwner` で断るより、グループを移る・読み直すときに履歴を作り直すほうが自然 |
| 何を無効な編集とするか           | `applyCommand` が throw                                                        | `draftProblems` は使えない。`addPen` がタグを選んでいない行を作るため、途中の状態を不正にできない。検証は構造だけ                                            |
| 復元したときの revision          | 戻さずに +1（あふれは断る）。保存の世代は過去へ戻さない                        | 下書きに revision は無い。`expectedRevision` は保存のときだけ使う。`onRestore` は要らない                                                                    |
| 保存したときの履歴               | 保存しても残す                                                                 | 案: 保存に成功したら空にする。revision の競合なら読み直して空にする（確かめるダイアログにするかは要判断）。ほかの失敗では残す                                |
| 外から変わったとき               | 該当なし                                                                       | MCP・REST（#1161）で保存済みの設定が変わると、手元の履歴とずれる。競合の扱いと同じにする                                                                     |
| 積む粒度                         | 1 つの編集の命令ごと                                                           | 入力欄の 1 文字ごとに積まない。確定（blur など）でまとめるのは呼び出し側                                                                                     |
| 上限                             | 50                                                                             | 下書きは小さいので 50 でも軽い                                                                                                                               |
| 画像などの資産                   | 現在・past・future の snapshot から参照を集めて保持する（`history-assets.ts`） | 該当なし                                                                                                                                                     |

## 4. 試験に入れること

SCADA の `history.test.ts` を domain 抜きにした物。新しく作るときも、この一覧を当てる。

1. Undo と Redo で内容が戻り、対象の id は変わらない（revision を進める場合は、それも確かめる）。
2. Undo の後に新しく編集すると、Redo が消える。
3. no-op の編集では履歴が変わらない（同じ参照が返る）。
4. 無効な編集は throw し、履歴は変わらない。
5. 上限を超えると、古い物から捨てる。
6. preview では編集も移動も断る。
7. 別の対象の履歴は断る。
8. 壊れた snapshot・後処理の失敗・今の値の検証の失敗では throw し、渡した値と履歴は変わらない。
9. 復元した値は snapshot の元と別の物で、書き換えても履歴に響かない。
