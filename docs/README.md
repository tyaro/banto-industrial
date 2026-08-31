# docs 地図（まずここを読む）

banto-industrial のドキュメント全体の入口。「どの文書が何の**正**か」「今どこまで進んでいるか」を
1 画面で引くための地図。詳細は各文書へ辿る。最終更新: 2026-08-31。

> この地図は索引に徹する。実装状況・設計判断の本体は各文書側にあり、状態の**正**は
> 常にリンク先の `状態:` 行と各表とする（CLAUDE.md H8 の状態欄同期規約）。

## 現状ひとめ（2026-08-31）

- **構成**: Rust workspace + SvelteKit/Tauri。アプリは **banto-hub**（タグサーバー）/ **chronogazer**
  （記録計）/ **relay-wright**。上流 `banto` は git tag / `@banto/*` を消費（現行 **v1.2.0**）。
- **I 系（基盤 I0〜I5）**: 実装済み。
- **W 系（relay-wright）**: W5 まで実装済み（実機検証のみ残）。
- **T 系（banto-hub、T0〜T18）**: T0〜T4 / T6〜T18-5b 実装済み。**残るは T18-5c/d（Windows 実機往復・
  狭幅/倍率・72h soak = オーナー同席の実機検証）と P3-b（SLMP CPU 種別/アクセスルート露出 =
  需要ドリブンのバックログ）**。T18-5a は「全タグのクライアント保持（上限 10,000 タグ）」を正式仕様化
  （windowed 化はバックログ降格）。
- **Hardening（H1〜H10）**: すべて完了。H9（SLMP 構造化エラー + transport 共通化）は 2026-08-14 完了。
- **出荷ゲート**: T5-5 = 72h soak 実行 + 実機最終サインオフのみ残（実機必須）。
- **banto-tagclient**: **S2b-2b完了、S3未完了（2026-08-31）**。読み取り専用DTO、Endpoint/Secret境界、
  stable ID resolver、REST catalog/values transport、WS wire純粋解析、bounded publish gate、認証付き
  WebSocket handshake、on_change subscribe、単一世代workerとwatchによるatomic latest snapshot配信を実装済み。
  公開Handle、再接続、rebinding、shutdownはS3で未実装。Hubのrelease tagは未確定。

## まず読む順

1. **CLAUDE.md**（ルート）— AI 作業規約・役割分担・開発規約。
2. **この地図**（docs/README.md）— 全体像と各文書の役割。
3. 目的別に下表の該当文書へ。banto-hub の現状を最短で掴むなら
   [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md)（残作業の索引・最新の全体像）。

## 文書地図

### 現行（現状の正・参照先）

| 文書                                                       | 何の正か / 役割                                                                                        |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| [plan.md](plan.md)                                         | **全体計画の親**。I/R/W/T 系マイルストーンと依存の一覧。                                               |
| [tag-server-design.md](tag-server-design.md)               | **banto-hub 設計の一次ソース**。タグ空間モデル・外部 IF・書き込み安全。実装状況は §9（T 系）表が正。   |
| [banto-tagclient-design.md](banto-tagclient-design.md)     | **banto-tagclient の実装前設計**。読み取り専用SDKのREST/WS、binding、再接続、停止、テストゲートの正。  |
| [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)     | **banto-hub 運転計画（T14〜T18）・UI/UX 決定台帳**。§9.3〜9.5 が T18 タグ登録 UX の受け入れの正。      |
| [banto-hub-operations.md](banto-hub-operations.md)         | **banto-hub 運用ガイド**（起動・ポート・API/MQTT/gRPC・サービス化・soak 手順）。現状の運用を引く入口。 |
| [banto-hub-t14-design.md](banto-hub-t14-design.md)         | T14 詳細設計（ランタイム状態管理・制御面分離）。                                                       |
| [banto-hub-t16-design.md](banto-hub-t16-design.md)         | T16 詳細設計（デスクトップシェル・タスクトレイ）。                                                     |
| [banto-hub-t17-design.md](banto-hub-t17-design.md)         | T17 詳細設計（SCM 管理・profile・UAC・インストーラ）。                                                 |
| [banto-hub-t18-design.md](banto-hub-t18-design.md)         | T18 詳細設計（タグ登録 UI/UX・性能検証）の実装分解索引（受け入れは desktop-plan §9.4 が正）。          |
| [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) | banto-hub **残作業の優先順位・着手順の索引**（他文書を正とする）。銘柄横断で現状を掴む最短路。         |
| [improvement-plan.md](improvement-plan.md)                 | Hardening（H1〜H10）の設計・進捗ログ。                                                                 |
| [recorder-requirements.md](recorder-requirements.md)       | 記録計（chronogazer）R0 要件定義。R1〜R4 スコープの正。                                                |
| [r1-plan.md](r1-plan.md)                                   | 記録計 R1 実施計画。                                                                                   |

### トピック別の「正」（重複時はここを見る）

- **T18 タグ登録 UX の受け入れ**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3〜9.5。
  実装分解は [banto-hub-t18-design.md](banto-hub-t18-design.md)、運用手順は [banto-hub-operations.md](banto-hub-operations.md)。
- **pending queue（運転中編集のキュー化）**: [banto-hub-desktop-plan.md](banto-hub-desktop-plan.md) §9.3 TAG-P0-3。
- **SLMP 構造化エラー（H9）**: [h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)。
- **タグ定義の single source of truth**: [tag-server-design.md](tag-server-design.md)。

### アーカイブ（役目終了・経緯として保存。現行仕様ではない）

各文書の冒頭に**アーカイブ・バナー**を付与済み。リンクは生きている（過去の rationale として参照可）。

| 文書                                                         | 状態                                                                                                  |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| [ux-plan.md](ux-plan.md)                                     | UX 改善計画（T9〜T13）。T13-2/3 は desktop-plan/T18 へ移管済み。T9〜T12 の rationale として参照のみ。 |
| [h10-3-read-scope-proposal.md](h10-3-read-scope-proposal.md) | H10③ read スコープの比較検討（案 B 採用、PR #75 で決着済み）。                                        |
| [t5-handoff.md](t5-handoff.md)                               | T5 セッション引き継ぎメモ（内容は operations / desktop-plan に反映済み）。                            |
| [r1a-readme-gaps.md](r1a-readme-gaps.md)                     | 上流 banto の README 手順の穴（外部フィードバック用チェックリスト。本リポの仕様ではない）。           |

## 補足: なぜ状態ヘッダが厚くなるか

CLAUDE.md は「設計判断はオーナー決定として日付付きで docs に記録」「実装状況が変わる PR で `状態:` 行を更新」
（H8）を定めており、各文書の状態ヘッダに日付付きの決定・進捗が積層する。経緯は意図的に保存する方針のため、
現状だけを素早く引きたいときは本地図と各文書の**表**（§9 表など）を先に見るとよい。
