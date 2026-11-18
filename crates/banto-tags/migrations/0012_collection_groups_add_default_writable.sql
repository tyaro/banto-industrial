-- T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2「`writable` の既定 —
-- 既定 ON。ただし収集グループ単位で変更可」、2026-09-02 オーナー決定）:
-- `collection_groups.default_writable` を追加し、そのグループへ新規タグを
-- 登録するときのフォーム側チェックボックス（`writable`）の初期値を
-- グループ単位で持てるようにする。
--
-- **2026-09-02 オーナー判断（実装後の追加決定）**: 当初は banto-hub の
-- `localStorage` に留める設計で実装したが、「まだ本番で使っていない今の
-- うちに、波及コストを払ってでも DB 列として正しく持たせる」という判断
-- により本列を追加する。`banto_tags` は relay-wright / banto-collect とも
-- 共有するクレートのため、`CollectionGroupInput`/`CollectionGroup` の
-- フィールド追加はそれらのビルド・テストにも影響する（構造体リテラルを
-- 網羅的に更新した - `collection_group.rs`/`rest.rs` の doc comment
-- 参照）。ただし**挙動そのもの**（収集・書き込みの動作）は banto-hub の
-- 新規タグ登録フォームの既定値だけに影響し、relay-wright/banto-collect の
-- 既存動作は変えない。
--
-- **これは登録時の検証ルールを一切変えない、UI 向けの既定値だけの列**
-- （実装指示「8段ゲートは撤去しない。変わるのはゲート2（writable）の
-- 既定値だけ」）。`tags.writable` 自体の CHECK・拒否ロジック
-- （`crates/banto-tags/src/tag.rs::validate_tag_input` -
-- computed タグ拒否・Modbus 読み取り専用領域拒否）はこの列と無関係に
-- そのまま効く。
--
-- 既定値は `1`（TRUE）: UX-34 の全体方針「既定 ON」に合わせ、既存の
-- グループも移行直後から「新規タグは既定で書込可」として振る舞う
-- （個々のタグの登録可否は上記の通り既存の検証がそのまま守る）。
--
-- `word_order`（migration 0010）と同じ理由でプレーンな `ADD COLUMN` 1本に
-- している: 追加する CHECK は新規列自身しか参照しないため、SQLite の
-- 「ADD COLUMN で CHECK 制約を追加できるのは新規列のみを参照する場合に
-- 限る」という制約に収まり、0004/0007 のようなテーブル再構築は不要。

ALTER TABLE collection_groups
    ADD COLUMN default_writable INTEGER NOT NULL DEFAULT 1
    CHECK (default_writable IN (0, 1));
