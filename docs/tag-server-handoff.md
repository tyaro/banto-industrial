# タグサーバー設計セッション 引き継ぎメモ

**このファイルは一時的なセッション引き継ぎ用。PR マージ前に削除すること。**

作成: 2026-08-04。前セッションはネットワーク許可リスト
（`*.roboticsware.com` 追加）の反映のため終了。許可はコンテナ起動時
スナップショットのため、この新セッションから有効のはず。

## 現在地

- ブランチ: `claude/plc-modbus-tag-server-4e67n7`（push 済み、PR 未作成 —
  オーナーから指示があるまで作成しない）
- 成果物: [tag-server-design.md](tag-server-design.md)（設計本体）と
  [plan.md](plan.md) §4c（T系マイルストーン T0〜T7）
- コミット履歴（全て docs のみ、コード変更なし）:
  1. `8262658` 初版（タグ空間 = banto-collect 現在値キャッシュ、REST/WS/MQTT/gRPC、OPC UA 後回し）
  2. `3144a54` 中央レジストリ構想（catalog = バインディング契約、§4.1/§7）
  3. `a30c0a5` FA-Server 概念対応表（§1.1）
  4. `61a973a` 演算タグのサーバー側実装（§4.2）+ オンライン動的変更（§4.3、T6/T7）

## オーナー決定済み（再確認不要）

1. タグサーバーを独立アプリとして設計先行する（実機に依存しない期間の作業）
2. 外部 IF は REST / WebSocket / MQTT / gRPC。OPC UA は後回し、OPC DA/DDE は追わない
3. ロガー・日報・帳票機能は作らない（記録は ChronoGazer の専管）
4. **中央レジストリ構想**: 今後の関連アプリ（SCADA 等)は自前でタグ定義せず、
   タグサーバーの catalog をバインドして使う
5. アクション機能（スクリプト・メール等）は不要
6. **演算タグ・内部タグはタグサーバー側で一元実装**（クライアント各自ではなく）
7. **オンライン動的変更**を FA-Server との差別化要件とする
   （FA-Server は稼働中の構成変更ができない、というのがオーナーの不満点）

## 次セッションの作業候補（優先順）

### 1. FA-Server マニュアルの直接参照（許可反映の確認）

**WebFetch は使わない**こと — 先方サイトのボット対策で 403（許可リストと
無関係）。コンテナから curl + ブラウザ UA で取得する:

```sh
curl -sS -L -A "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36" \
  -o toc.html "https://docs.roboticsware.com/ja/6.0.17/fa-server/contents/"
```

見たいページ（前セッションは検索スニペット経由でしか読めていない）:

- 目次: `https://docs.roboticsware.com/ja/6.0.17/fa-server/contents/`
- タグ編（3層構造・タグ種別の詳細）: `.../cmn_tag.html`, `.../cmn_tag_overview.html`
- インターフェース編（Panel IF の購読仕様 — §5.2 WebSocket 設計との比較材料）:
  `.../cmn_interface_overview.html`
- 演算タグ/内部タグ関連（式の仕様 — §4.2 式文法の参考）
- ネットワークタグ: `.../cmn_tag_overview_6.html`

目的: 設計 doc §1.1 の対応表を一次情報で検証・補強し、§4.2 式文法と
§5.2 購読プロトコルの設計判断材料を得る。読めたら §1.1 の「マニュアル調査
2026-08-04」を更新。

### 2. オーナーの未決事項回答があれば反映

[tag-server-design.md](tag-server-design.md) §10 に 12 件。特に T6 着手前に
必要なのは式文法（§10-12）、T2 前に必要なのは I1 スキーマ拡張の置き場所
（§10-2）と broker 抽出（§10-3）。

### 3. その先（オーナーと合意してから）

- REST / WS の API 詳細仕様化（OpenAPI / メッセージスキーマの厳密化）
- T0 骨格の実装着手（設計承認後）

## 環境メモ

- このリポジトリは docs 先行中で、既存コード（crates/, apps/）には触れていない
- 設計の根拠にした既存コードの要所: `banto-collect`（接続毎1タスク・
  `CurrentValuesHandle`・`EventSink`・スナップショット再構築）、
  `apps/relay-wright/core/src/engine/broker.rs`（read/write 単一セッション、
  I6 抽出候補）、`banto_server`（banto 側: auth/CSRF/SSE）、axum 0.8 は
  workspace 依存に既存
- コミットメッセージは日本語・Conventional Commits 風（既存履歴に合わせる）
