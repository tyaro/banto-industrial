# banto-hub / SCADA server 冗長化 設計草案

作成日: 2026-09-30  
最終更新: 2026-09-30（オーナーレビュー 6 回目を反映: pending の abort は調停者上の fenced CAS だけで行いローカル timeout では開け直さない（4.3、5.3）、Alarm の範囲同期は任意の replica から origin ごとに回収し watermark は origin ごと（6.3）、authority 候補集合は固定で変更は計画停止の手順のみ（6.3）、PLC の多ワード値は double buffer + index word で一貫性を作る（5.3）、HLC はリモート受信時にも merge（6.3）。同日: オーナーレビュー 5 回目を反映: 書き込みゲートに調停者の committed generation を直接入れ prepare から switch 完了までは閉じる（4.3）、候補 A のワードは slot / committed / commit_req で同じ 96 bit の generation token を比較し epoch 類は 32 bit に（5.3）、Alarm の barrier は quorum 方式で fail-closed（6.3）、Sink の期限判定は `clock_timestamp()` と行ロック先行に（4.8）。同日: オーナーレビュー 4 回目を反映: 設定の有効化を prepare / commit に分け commit point を調停者に置く、primary 昇格は調停者の committed generation と一致する台だけ（4.3、5.3）、Alarm の watermark は欠番なく連続した最大 seq、新 authority はサービス開始前に catch-up barrier、同期複製の既定は到達可能な候補全台（6.3）、Sink の flush guard に `expires_at >= now()`（4.8）、client 側 write gate の解除条件（§7）。同日: オーナーレビュー 3 回目を反映: 設定の「配布」と「有効化」を分け generation で書き込みを開ける（primary も例外にしない、4.3）、操作者イベントの同期複製は依存する `opened` を束ねる因果複製に（6.3）、PLC Action の fencing は bounded-delay と明記し strict は PLC 側 command gate（5.1 / 5.3）、Sink のリースは sink group 単位（4.8）、HLC の永続化と MQTT `t` の記述削除（6.3 / 4.7）。同日: オーナーレビュー 2 回目を反映: リースの権利を副作用まで切れ目なく伝える。期限は最後に成功した Grant + ttl（5.1）、Sink はサイドカー自身が DB リースを持ち INSERT と同一トランザクションで検査（4.8）、設定の鮮度確認と `config-primary` のリース化（4.3）、ACK は別 1 台への同期複製後に応答（6.3）、`origin_seq` の永続化（6.3）、候補 B の MQTT 巻き戻りを許容条件として明記（4.7）。同日: オーナーレビュー 1 回目を反映: Alarm occurrence の採番を `alarm-authority` に（6.3）、役割を重複許容 / 排他必須に分け排他必須は競合窓の無い調停に限定（5.1）、設定不一致時の PLC 書き込みを fail-closed に（4.3）、イベントの順序を origin seq + HLC に（6.3）、履歴統合を区間ごとの正ソース方式に（6.2）。同日: 初版。scada-design.md §13.2 の原則を受けて、3 層の冗長化とリースの抽象を草案化）  
状態: **草案（オーナー議論用）。実装は scada-server と Hub の単一構成が動いてから（scada-design.md §22 #19）**  
対象: banto-hub（タグサーバー）、SCADA server core、PLC 接続層（banto-plc / banto-collect / banto-broker）、
banto-tagclient

関連: [scada-design.md](scada-design.md) §13.2（原則の決定）、§22 #19、
[tag-server-design.md](tag-server-design.md)、
[banto-hub-external-db-design.md](banto-hub-external-db-design.md)、
[banto-tagclient-design.md](banto-tagclient-design.md)、
[banto-hub-t16-design.md](banto-hub-t16-design.md) / [banto-hub-t17-design.md](banto-hub-t17-design.md)

---

## 0. 目的と範囲

現場の可用性を 1 台の PC に依存させないために、**PLC（ドライバ層）、Hub、SCADA server の 3 層**を
それぞれ N 台化する方法を決める。scada-design.md §13.2 で決めた原則は本書の前提であり、再検討しない:

- 3 層は**独立に**冗長化する。PLC の冗長系はドライバ層が隠し、Hub から見える接続とタグの外部名は変えない
- Hub と SCADA server は **「読み取りと評価は全台、副作用は 1 台」**。recorder と Alarm 評価は台数無制限の
  Active/Active、副作用（常時実行 Action、MQTT publish、Sink の DB 書き込み）は**リースの抽象**で 1 台に限定
- 3 台以上の分断対策の調停は Hub 同士の過半数（Raft 系）ではなく **PLC 調停**を第一候補とする
- 名前束縛（scada-design.md §9.6）が前提。client は別インスタンスへ再接続して名前で再バインドできる
- 現場の可用性は Hub で頭打ちになる。SCADA server の冗長化だけを先行させない

本書で決めるのは、**リースの契約と実装候補、各層で「複製するもの / 各台が独立に持つもの / 読み出し時に
統合するもの」の仕分け、client の切替、障害時の振る舞い、段階導入の順序**。実装の詳細（API の形、schema）
は各層の設計文書に持ち帰る。

**非対象**: PLC が無いと設備が動かない制御ロジックの冗長化（PLC 側の責務）、Windows クラスタや共有
ストレージによる OS レベルの HA、DB（PostgreSQL）自体の冗長化。

---

## 1. 現状の事実（2026-09-30 時点、コード照合）

冗長化に関わる現状を先に固定する。**いずれの層にも冗長化・フェイルオーバー・リース・ハートビートの実装は
無い**。

| 項目                 | 現状                                                                                                                                                                                                      |
| -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Hub の設定           | profile ごとの SQLite 1 本（`profiles/<id>/config/banto-hub.sqlite3`）。設定パッケージの export / import は**クライアント側 TS の機能**で、非トランザクション。API キーのハッシュ・パスワード類は含まない |
| Hub の `revision`    | **プロセス内カウンタ**（起動で 0 → `commit_catalog` で 1）。`run_id` も同様にプロセスごと。別インスタンス間で値の意味は無い                                                                               |
| Hub の排他           | profile lock（Windows は名前付き mutex、他は flock）。同一 profile の二重起動を拒否する。ポートの lock は無く bind 失敗で検出                                                                             |
| 現在値               | メモリのみ。`ptime` は収集 PC の時計。品質 Stale は読み出し時に導出                                                                                                                                       |
| 内部タグ（`mem`）    | `retain=true` のものだけ SQLite に保存し、起動時に復元。インスタンス間の複製は無い                                                                                                                        |
| API キー             | SHA-256 ハッシュを SQLite に保存。設定パッケージには含まれない（t17 P6）                                                                                                                                  |
| MQTT publish         | Hub 内蔵、`client_id` 既定 `banto-hub`、retain=true、`$state` に LWT。インスタンス間の調停は無い                                                                                                          |
| DB Sink              | 別プロセス（SCM サービス）。Hub が設定を渡し状態を受け取る。at-least-once、キューはメモリのみ。行の一意性は `(ts, tag_id)` で DBA 任せ。`tag_id` は**インスタンスごとの SQLite id**                       |
| PLC 接続             | `plc_connections` は `host` / `port` **1 組**。protocol は modbus-tcp / slmp / virtual / postgres。接続ごとに 1 task、再接続は 1 s → 30 s の backoff                                                      |
| MELSEC SLMP          | 4E binary、`io_id` 既定 0x03FF（自局 CPU）。冗長系の指定（制御系 / 待機系 / A 系 / B 系）は**未対応**。registry から `network_id` / `pc_id` / `io_id` を指定できない（既知の制約）                        |
| 実機の接続数         | R08ENCPU はポートあたり 1 セッション、Modbus KM-D1-ETN は 1 接続（tag-server-design.md §6）                                                                                                               |
| PLC 書き込み         | REST / gRPC → `execute_write` → broker。相関は oneshot、再試行なし。監査は `hub_write_audit`（インスタンスごと）                                                                                          |
| banto-tagclient      | **単一エンドポイント**。エンドポイントを変えるには `TagClientHandle::restart` で作り直す。1008（認証拒否）は再接続しない。書き込みは 1 回の POST で再試行なし                                             |
| ChronoGazer / tstore | 日次 SQLite ファイル、group ごとの `samples_<n>(ptime PRIMARY KEY)`。同一 `ptime` は上書き。**プロセス間の排他は無い**（同じ `data.dir` を 2 プロセスで開くのは未防止）。Hub 経由の記録（段階 3）は未着手 |
| SCADA server core    | 未作成（scada-design.md §21 S9b）                                                                                                                                                                         |

この表から、冗長化の前提として**単一構成の段階で入れておく**べきものが決まる（§9 の R0）。

---

## 2. 用語

- **論理サービス（service）**: 現場 / Project 単位の「1 つの Hub」「1 つの SCADA server」。client と運用者が
  名前で指す単位。識別子は `service_id`（設定で与える文字列、例 `line-a-hub`）
- **インスタンス（instance）**: 論理サービスを構成する 1 プロセス / 1 ホスト。識別子は `instance_id`
  （設定で与える。既定はホスト名 + profile）。`(service_id, instance_id)` は一意
- **役割（role）**: 副作用の種類ごとの「実行担当」。例: `mqtt-publisher`、`sink-writer`、`action-executor`、
  `config-primary`。役割はリースで 1 インスタンスに割り当てる
- **リース（lease）**: 役割を期限付きで 1 インスタンスに与える抽象（§5）。保持者は期限内に更新し続ける。
  更新が途切れたら他のインスタンスが取得できる
- **エポック（epoch）**: リースの取得ごとに単調増加する番号。fencing token として副作用に添える
- **調停者（arbiter）**: リースの正を持つ場所。第一候補は PLC の内部ワード（PLC 調停）
- **系（system）**: PLC 冗長系の A 系 / B 系（物理）と、制御系 / 待機系（役割）。ドライバ層の用語で、
  Hub より上には出さない

---

## 3. 層 1: PLC 冗長系のドライバ対応

**原則**: PLC の冗長系は `banto-plc` / `banto-collect` / `banto-broker` の接続層が隠す。Hub の
`plc_connections` から見える接続は 1 つ、タグの外部名 `{connection}.{group}.{tag}` は変えない。

### 3.1 接続の複数エンドポイント化

- `plc_connections` に **endpoint の順序付きリスト**を持てるようにする（現状の `host` / `port` 1 組は
  リスト長 1 の特殊ケース。既存行はそのまま動く）
- 接続層は「今つながっている endpoint」を 1 つ持ち、読み取り失敗・接続断で**次の endpoint へ切替**、
  全部失敗したら先頭から backoff で再試行する。切替はイベント `plc_endpoint_switched{from,to}` として
  `collect_events` に残す（既存の `plc_disconnected` / `plc_reconnected` と同じ流儀）
- 切替中の値は Bad、切替後の最初の成功読み取りで Good に戻る。**切替中に「古い値を Good のまま」出さない**
  （scada-design.md §1.3 の安全な劣化）
- broker の書き込みも同じ endpoint 選択に従う。切替の瞬間に発行された書き込みは失敗を返す（再試行しない、
  現状のまま）。書き込み先が待機系だった場合の扱いは 3.2

### 3.2 MELSEC の冗長系（要実機確認）

MELSEC iQ-R の冗長システムは、SLMP の要求先モジュール I/O 番号（`io_id`）で **制御系 CPU（0x03D0）/
待機系 CPU（0x03D1）/ A 系 CPU（0x03D2）/ B 系 CPU（0x03D3）** を指定できる（現状の既定 0x03FF は自局）。
これを使うと、A 系・B 系どちらの Ethernet ポートに接続していても「制御系」を宛先にできる。

設計:

- `plc_connections` の SLMP 設定に `io_id`（と `network_id` / `pc_id`）を持てるようにする。これは冗長化と
  無関係に既知の制約の解消でもある（tag-server-design.md §6）
- 冗長系の接続は **endpoint = [A 系 IP, B 系 IP]、`io_id` = 0x03D0（制御系）** とする。切替後も宛先が
  制御系なので、待機系に書き込む事故が起きない
- 「システム切替時に IP を引き継ぐ」設定（制御系 IP の引き継ぎ）を使う現場は endpoint 1 つで足りる。
  その場合も `io_id` = 0x03D0 を推奨する

**実機で確認すること**（§10 #1）: 待機系のポートに `io_id` = 0x03D0 で要求したとき、トラッキングケーブル
経由で制御系に届くか。応答の `io_id` エコーが要求と一致するか（現状の `slmp` crate は不一致を framing
error にする）。切替直後の応答時間と、その間の読み取りエラーの種類。

### 3.3 Modbus TCP と他プロトコル

Modbus には冗長系の概念が無い。endpoint リスト（3.1）だけで対応し、どちらが制御系かは機器側の設定
（VRRP や制御系 IP の引き継ぎ）に任せる。virtual / postgres は対象外。

### 3.4 この層で決まらないこと

PLC の冗長系がどう切り替わるか（PLC の責務）。Hub が複数台のとき各 Hub が PLC の接続数を消費する問題は
層 2（4.2）。

---

## 4. 層 2: Hub の N 台化

### 4.1 仕分け

| 種別                     | 対象                                                                                                     | 方針                                                                                  |
| ------------------------ | -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| 全台で同じでなければ困る | 接続・グループ・タグ定義、計算タグ、Sink グループ、MQTT / gRPC 設定、write-control、コミッショニング状態 | **設定パッケージ**で配布し、`active_generation` で有効化して一致を状態で見せる（4.3） |
| 全台で同じだが機微       | API キー、ユーザー、PLC パスワード、MQTT パスワード                                                      | 設定パッケージには入れない（現状どおり）。**別経路**で配布（4.4）                     |
| 各台が独立に持つ         | 現在値、品質、`revision`、`run_id`、Hub 内 tstore（7 日）、`collect_events`、`hub_write_audit`、監査ログ | 複製しない。読み出し側（運用者・SCADA）が必要なら統合する                             |
| 1 台だけが行う           | MQTT publish、Sink の DB 書き込み、設定の編集と配布の宣言（`config-primary`）                            | **リース**（§5）で役割を 1 台に限定。Sink はサイドカーが DB 上のリースを持つ（4.8）   |
| どちらとも言えない       | 内部タグ（`mem`）の値                                                                                    | 初版は**複製しない**（制約として明記）。後続で保持者からの転送を検討（4.6、§10 #4）   |

### 4.2 読み取り: 各台が PLC を直接ポーリングする（Active/Active）

原則どおり全台が PLC を読む。**この層の台数上限は PLC 側の接続数**で決まる。

- R08ENCPU はポートあたり 1 セッション、Modbus KM-D1-ETN は 1 接続。これらでは 2 台目の Hub が同じ
  ポートに入れない
- 第一候補: **インスタンスごとに PLC 側のポートを分ける**（MELSEC は Ethernet 設定で複数の接続を
  開けられる）。`plc_connections` の endpoint を「インスタンス別の上書き」で持てるようにする
  （設定パッケージは共通、`port` だけインスタンスの設定で差し替え）
- 代替（接続数が 1 しか無い機器）: **読み取り中継モード**。読み取り担当（役割 `plc-reader`、リースで
  1 台）だけが PLC を読み、他のインスタンスは担当の Hub を banto-tagclient で購読して同じ外部名で値を
  持つ。担当が落ちたらリースを取った台が直読みに切り替える。**値の遅延と二重の依存が増える**ので、
  接続数を確保できない機器に限る
- 各台の値は各台の時計で刻む（`ptime`）。台間で値の順序を比べることはしない（NTP 同期は運用要件、§10 #6）

### 4.3 設定の配布と有効化（generation）

- **設定の正は 1 台**（役割 `config-primary`、排他必須、リースで決める。2026-09-30 レビュー 2 回目 #1 の
  帰結: 下の規則により primary が居ないと standby は書き込みを受けられないので、primary 固定では primary の
  停止 = 全台で書き込み不可になる）。A / D の調停が無い現場は**固定 primary** を選べるが、その場合は primary
  停止中は全台で書き込み不可（安全側）と明記する。primary 以外の管理 UI は設定編集を読み取り専用にする
  （`/api/v1/status` の `config_primary: false` を見て UI が閉じる）
- **配布と有効化を分ける**（2026-09-30 レビュー 3 回目 #1）。primary を書き込みゲートの例外にすると、
  未配布や部分適用の設定で primary が書き込み、その後 failover した新 primary が古い設定を「正」として
  戻す事故が起きる。よって:
  - **`config_generation = (config_epoch, fingerprint)`**。`config_epoch` は有効化のたびに primary が +1 する
    単調な番号、`fingerprint` はパッケージ内容の正規化ハッシュ（数値 id と機微情報を除く）
  - 各インスタンスは **`active_generation`**（収集と書き込みゲートが使う設定）と **`staged_package`**（配布
    されたが未有効化のパッケージ）を持つ。編集は primary でも **staging に入り、running には触れない**
    （現状の `pending_changes` / `configured_revision` と `running_revision` の区別を流用する）
  - **書き込みゲートは全台で同じ規則**（2026-09-30 レビュー 5 回目 #1）:
    **`running の実 generation == active_generation == 調停者の committed generation`** の 3 者が一致し、
    かつ有効化の途中（下の prepare ack 後〜switch 完了前）でないときだけ開く。ローカル改変や部分適用
    （running ≠ active）、有効化の遅れ（active ≠ committed）のどちらでも閉じる。primary の例外は無い。
    調停者の committed generation はリースの renew と同じ周期（ttl / 3）で読む。commit point だけが先に
    進み各台の switch が遅れた / 失敗した状態で、旧 generation のアドレスへ書き続けることを防ぐため。
    `config-primary` の保持者も、committed generation が自分の active と違う間は書き込みの権威を失う
  - **有効化（activate）は prepare と commit に分け、commit point を調停者に置く**（2026-09-30 レビュー
    4 回目 #1）。「record を受け取った台から切り替える」と、途中の配布失敗で一部の台だけ新 generation に
    なる。手順:
    1. **prepare**: primary が staging を検証し、**配布セット**（設定に書いた peers の全員。運用者が明示的に
       除外した台を除く）へパッケージと `pending_activation (config_epoch, fingerprint)` を push する。各台は
       staged に保存し pending を**永続化してから** ack する。**この段階では誰も切り替えない**が、
       **ack した台は書き込みゲートを閉じる**（`write_gate: activating`）。primary も prepare を始めた時点で
       閉じる。commit と switch の間に旧 generation で書く窓を、調停者の読み取り周期に頼らず無くすため。
       有効化は運用者が起こす短い操作なので、その間の書き込み停止は受容する（UI に「有効化中」）。
       **abort は調停者上の fenced CAS でだけ行う**（2026-09-30 レビュー 6 回目 #1）: primary（または
       昇格した新 primary）が `config-primary` のリース epoch 付きで調停者の `aborted_generation` に pending
       の token を書く（`committed != pending` のときだけ成功。commit と同じ調停者の状態を CAS するので
       両立しない）。prepare 済みの台は**ローカルの経過時間だけでゲートを開け直さない**。調停者を読んで
       `committed == pending` なら switch、`aborted == pending` なら pending を捨てて開け直す。60 s 経っても
       決まらない pending は状態に `stale pending` と出すだけ。ローカル timeout で捨てると、commit 直後
       （次の読み取り前）に timeout が発火し、キャッシュ上は 3 者一致に見える旧 generation でゲートを
       開け直すレースが起きる
    2. **commit**: 全員の ack が揃ったら、primary が調停者の **committed generation**（5.3 の
       `committed_generation` の 96 bit token。候補 D なら `config-primary` のリース行の列）を
       `config-primary` のリース epoch 付きの CAS で書く。**この 1 回の書き込みが唯一の commit point**。
       失敗（リースを失っていた、調停者に届かない）なら activate は失敗で、全台は旧 generation のまま
       （pending は次の prepare で上書きされるか、primary が明示的に abort する）
    3. **switch**: 各台は commit の通知を受けるか、**自分で調停者の committed generation を読んで**（5 s
       ごとの確認で）、それが自分の pending と一致したときだけ staged から running を作り直して切り替える。
       通知を取りこぼしても調停者を読めば追いつく。切り替えの原子性は、running 用の SQLite を staged から
       組み立てて差し替える等で実装時に確保する。import が非トランザクションでも running に途中失敗が混ざらない
  - **除外された台**（配布時に落ちていた台）は `excluded` になり、復帰時にピアから最新のパッケージを取り込み、
    調停者の committed generation と一致するまで（catch-up）、書き込みを受けず primary にもなれない
  - **primary の昇格条件は調停者の committed generation との一致**（2026-09-30 レビュー 4 回目 #2）:
    `config-primary` の `try_acquire` は「自分の `active_generation` == 調停者の committed generation」の
    ときだけ成功する（一致は `config_epoch` と fingerprint の**両方**。候補 A はラダーが
    `slot.generation_token == committed_generation`（5.3、96 bit）を見てから保持者にする。候補 D は
    `WHERE committed_epoch = $my_epoch AND committed_fp = $my_fp` を取得条件に含める）。
    「自分の知る最大の record」を基準にすると、最新を知らずに分断された台が primary 停止後に古い generation
    で昇格して旧設定を正に戻せる。共有された commit point を基準にすればこれは起きない。誰も一致しなければ
    primary 不在 = 全台で書き込み不可（安全側）
  - **固定 primary**（A / D の無い現場）には調停者が無いので commit point も無い。primary が 1 台だけなら
    巻き戻りは「primary 自身を古いバックアップから復元した」場合にしか起きず、それは運用の責任と明記する
  - 旧 primary が復帰したとき、staging に未有効化の編集が残っていれば「未有効化」として UI に出す。running は
    最後に有効化した generation なので、他と同じなら standby として書き込みを受けられる
- **鮮度**（2026-09-30 レビュー 2 回目 #1）: standby は primary の `active_generation` を **5 s ごとに読んで
  確認**し、確認できた時刻 `confirmed_at`（自分の単調時計）を持つ。standby の書き込み許可は「自分のゲートが
  開いている」かつ「自分の `active_generation` == primary の `active_generation`（確認済み）」かつ
  **`now - confirmed_at < confirm_ttl`（既定 30 s）** の全部が要る。**未確認（起動後まだ読めていない）と
  失効は拒否**する。「最後に知った値」で許可し続けると、standby が primary から分断された後に primary 側で
  有効化が進んでも standby は古いまま許可し続けて fail-open になるため（配布セット全員の ack を要求する
  既定ではこの状況は起きにくいが、除外運用と組み合わさると起きる）。primary は `config-primary` のリースを
  保持している間（5.1 の期限内）かつ自分のゲートが開いているときだけ受ける。冗長構成でない（peers が
  空の）ときはこのゲートは無効
- **client 側の判断**: 運用者と SCADA は `active_generation` の一致で「同じ Hub」と判断する。不一致は警告
  （値は出し続ける）。名前での再バインドは設定がずれていても成功するので、この判断は client 側の警告に
  とどめ、拒否は server 側のゲートが担う（2026-09-30 レビュー #3）
- 配布は**設定パッケージ**（既存の export / import）を使う。ただし現状はクライアント側 TS の機能なので、
  **サーバー側の export / import API と staging / activate API** を足す（scada-design.md §5 の Project
  package と同じ発想。SCADA の Hub 連携でも要る）
- 既存の `revision`（プロセス内カウンタ）は**インスタンス間で比較しない**。client の publish gate
  （banto-tagclient の revision 一致判定）はインスタンス内で閉じているので、そのまま使える

### 4.4 API キーとユーザー

- API キー（ハッシュ）はパッケージに含めない決定（t17 P6）を維持する。冗長構成では **client が別インスタンスへ
  切り替えても同じキーで通る**必要があるので、次のどちらかを選ぶ（§10 #5）:
  - (a) **キーの同時発行**: primary で発行したキーを、他インスタンスにも管理 API で**同じ secret のまま**
    登録する（ハッシュだけを運ぶ管理者向け bundle、TLS 前提）
  - (b) **インスタンスごとのキー**: client 側でエンドポイントごとにキーを持つ（banto-hub-bootstrap の
    keyring は `hub:{host}:{port}` 単位なので、構造上は今でも可能）。自動接続（#332）の試運転キー自己発行は
    インスタンスごとに走る
  - 推奨は (b) を初版とし、(a) は運用が増えたら足す。(b) なら Hub 側の変更が無い
- ユーザー（管理 UI のログイン）はインスタンスごとでよい（管理は primary で行う）

### 4.5 書き込み

- どのインスタンスも PLC 書き込みを受ける（書き込みは PLC が最終的な権威なので、複数台から届いても
  「最後の書き込みが勝つ」以上の問題は起きない）。client は接続中のインスタンスに書く
- 再試行しない現状を維持する。**切替中の書き込みは失敗として client に返し、操作者が再操作する**
  （scada-design.md §16 の相関 ID と監査が前提）
- `hub_write_audit` はインスタンスごと。監査の統合は「読み出し時に全台から集めて相関 ID で並べる」で
  よい（SCADA server の execution record と同じ扱い、6.4）
- 内部タグへの書き込みは受けたインスタンスだけに効く（4.1 の制約）
- 4.3 のゲートにより、generation がずれた台は書き込みを `409 config_mismatch` で返す。client はそれを
  操作者に見せ、運用者が有効化を完了させて解消する

### 4.6 内部タグ（`mem`）

初版は複製しない。台間で共有したい状態は **PLC のワードに置く**（PLC が control authority である原則と
一致し、PLC 冗長系でもトラッキングされる）。後続候補: 役割 `mem-primary` の保持者に書き込みを転送し、
保持者が他へ配る（best-effort、順序保証なし）。

### 4.7 MQTT publish（リース対象）

- 役割 `mqtt-publisher` の保持者だけが接続・publish する。他は接続しない（`$state` の LWT が
  保持者のものになる）
- `client_id` は**インスタンスごとに変える**（同じ id で 2 台がつなぐとブローカーに切られる）。既定を
  `banto-hub-{instance_id}` にする
- 取得時の全件 republish は既存の ConnAck 時の republish で足りる（retain=true なので購読側は最新値を
  受け取る）。切替の間の変化は失われる（MQTT は「最新値の鏡」であり履歴ではない、tag-server-design.md §5.3）
- **候補 B で許容する不整合**（2026-09-30 レビュー 2 回目 #6）: B の競合窓（最大 2 周期）では 2 台が
  独立にポーリングした値を publish するので、**retain 値が 1 サンプル分巻き戻る**ことがある（H1 が新値を
  出した後に H2 が古い値を出す）。窓が閉じれば保持者の次の publish で直る。これを B での MQTT の**明示的な
  許容条件**とする。ペイロードの `t` は各 Hub の時計なので**購読側が `t` で巻き戻りを見分けることはできない**
  （4.2、2026-09-30 レビュー 3 回目 #5）。巻き戻りも許容できない現場は `mqtt_strict: true` で
  `mqtt-publisher` を排他必須（A / D のみ）に切り替える

### 4.8 DB Sink（リース対象）

- Sink サイドカーは**インスタンスごとに 1 つ**置き、設定は各自の Hub から取る（現状どおり）
- **リースの単位は Sink group**（2026-09-30 レビュー 3 回目 #4）: `hub_sink_groups` は group ごとに宛先
  DB（`db_connection_id`）を持ち、1 台の Hub が複数 DB へ書ける。よって役割は **`sink-writer:{sink_group}`**
  とし、リース行はその group の宛先 DB に `(service_id, sink_group)` をキーに置く。group ごとに独立に
  保持者が決まるので、H1 が group A、H2 が group B の書き手になることはあり、それでよい（group 内で 1 台
  なら「副作用は 1 台」を満たす）。全 group を 1 台に寄せたい場合は宛先 DB と別の共通の調停者が要るが、
  初版では提供しない。状態表示も group ごとの `{held, epoch, expires_in}` にする
- **各 group の `sink-writer` のリースは、INSERT する当人であるサイドカーが宛先 DB の上で持つ**（候補 D。
  2026-09-30 レビュー 2 回目 #3）。Hub が `writer` フラグを配る方式は、別プロセスのサイドカーが古い
  `writer=true` のまま INSERT を続けられる（設定の再取得は既定 30 s、5 s なのは状態 push）ので fencing に
  ならない。代わりに:
  - 宛先 DB に `banto_sink_lease(service_id, sink_group, holder_instance_id, epoch, expires_at)` を group
    ごとに 1 行置く。期限の判定は**実際の現在時刻**で行う（PostgreSQL の `now()` / `CURRENT_TIMESTAMP` は
    トランザクション開始時刻で固定されるので使わない。**`clock_timestamp()`** を使う。2026-09-30 レビュー
    5 回目 #4）。取得は `UPDATE ... SET holder=$me, epoch=epoch+1, expires_at=clock_timestamp()+ttl WHERE
expires_at < clock_timestamp()`（0 行なら取れていない）
  - **各 flush のトランザクションは lease 行のロックと検証を先に行う**:
    `BEGIN; SELECT ... FROM lease WHERE service_id=$s AND sink_group=$g FOR UPDATE` で行をロックし、
    `holder=$me AND epoch=$epoch AND expires_at >= clock_timestamp()` を検証（違えば ROLLBACK）→
    `INSERT rows` → `UPDATE lease SET expires_at=clock_timestamp()+ttl WHERE holder=$me AND epoch=$epoch
AND expires_at >= clock_timestamp()`（0 行なら ROLLBACK）→ `COMMIT`。行ロックにより、取得を試みる
    他のサイドカーの UPDATE はこのトランザクションの commit / rollback まで待ち、commit 後は期限が延びて
    いるので取れない。INSERT は「その時点で DB 上で期限内の保持者である」ことと原子的にしか commit されない。
    **期限の検査が無い、または `now()` で検査すると、DB 上では期限切れの旧保持者が新保持者の取得直前に
    flush を通して同じ epoch のままリースを延命できる**（2026-09-30 レビュー 4 回目 #5、5 回目 #4）。
    flush のトランザクションは ttl / 2 より短く保つ（`statement_timeout`、バッチ上限）。DB の `expires_at`
    が正で、ローカルの期限（次項）は事前抑止にとどめる。これが真正な fencing で、時間の fencing（5.1）は
    その上の保険
  - サイドカーは自分の単調時計で `local_deadline = 最後に成功した lease 更新の時刻 + ttl` を持ち、期限を
    過ぎたら flush を試みない（DB に届かない = 保持者でないと見なす）
  - Hub は `/api/sink/config` で `lease_priority` と `service_id` を渡すだけで、保持の可否には関与しない。
    サイドカーは状態 push に group ごとの `sink_writer[{group, held, epoch, expires_in}]` を含め、Hub は
    それを状態に転記する
  - 保持者でないサイドカーはキューを溜めず購読だけ続ける（切替時に古い値を大量投入しない）
- 保持者の交代は ttl で区切られ、**重複は「同一インスタンス内の at-least-once 再送」だけ**になる。この重複は
  同じ `ts` を持つので一意制約で吸収できるが、**`tag_id` はインスタンスごとの SQLite id なので台間で一致
  しない**。冗長構成の一意制約は **`(ts, external_name)`** を推奨し、行に `instance_id` と `epoch` を足す
  （external-db-design.md §5.4 の追記、§10 #7）
- 台をまたぐ「同じ物理値が別の `ts` で 2 行」は、各台の `ts` が違うので一意制約では吸収できない
  （2026-09-30 レビュー #5）。A / D では保持者が同時に 2 台にならないのでこの重複は起きず、読み手が区間ごとの
  正を選びたいときは `epoch` で選べる（6.2 と同じ考え方）
- 欠落は「切替の間（ttl + 取得）」に限られ、履歴の正は SCADA server / ChronoGazer の recorder（層 3）にある

### 4.9 状態の見せ方

`/api/v1/status` に足す: `service_id`、`instance_id`、`active_generation: {epoch, fingerprint}`、
`staged_generation`、`config_fingerprint`（running の実 hash）、`config_primary`、`config_confirmed_age_ms`、
`write_gate: open | activating | generation_mismatch | committed_not_switched | unconfirmed | excluded | no_primary`、
`leases: [{role, held, epoch, holder_instance_id, expires_in_ms, guarantee: strict | bounded_delay}]`、
`peers: [{instance_id, endpoint, reachable}]`（peers は設定に書いた静的リスト。発見はしない）。管理 UI のトレイと状態画面はこれを表示する。

---

## 5. リースの抽象

### 5.1 契約

```text
trait Lease {
  // 役割 role のリースを instance が取ろうとする。取れたら epoch を返す
  fn try_acquire(role, instance_id) -> Result<Option<Grant{epoch, ttl}>>;
  // 保持中のリースを更新する。保持者でなくなっていたら Err(Lost)
  fn renew(role, instance_id, epoch) -> Result<Grant>;
  // 明示的に手放す（計画停止）。失敗してもよい（期限切れで回収される）
  fn release(role, instance_id, epoch);
  // 観測: 今の保持者と epoch（表示・診断用。判断には renew の結果を使う）
  fn observe(role) -> Option<{holder, epoch, expires_in}>;
}
```

規律:

- **副作用は `renew` が成功している間だけ行い、`Lost` を受けたら即座に止める**。「止める」が安全側であることが
  前提（MQTT は publish 停止、Sink は INSERT 停止、Action は実行しない）
- **fencing**: 副作用には `epoch` を添える。受け側が epoch を見られる場合（Sink の行、Action の
  execution record、MQTT のペイロードのメタ）は古い epoch を捨てられる。受け側が同じトランザクションで
  検査できる場合（Sink、4.8）はそれが真正な fencing になる。PLC 書き込みは epoch を見られないので、
  リース失効後の書き込みは**時間で**防ぐ
- **時間の fencing の期限**（2026-09-30 レビュー 2 回目 #2）: 保持者の期限は
  **`local_deadline = 最後に成功した Grant / renew を受けた時刻（自分の単調時計）+ ttl`**。renew の失敗は
  期限を延ばさない（「renew 失敗から ttl」と数えると、調停者が新しい保持者を許可した後も旧保持者が
  動ける時間ができる）。renew は `ttl / 3` ごとに行い、成功するたびに期限を更新する。調停者は保持者の
  最後のハートビートから **`ttl + margin`**（margin は ttl の 20% 以上。時計の進み方の差と書き込み遅延の
  分）を過ぎるまで次の保持者を許可しない。**副作用は `now < local_deadline` の間だけ**行い、PLC 書き込みや
  Action の 1 ステップの直前には **`now + その操作のタイムアウト < local_deadline`** を必須の検査にする
  （応答待ちの途中で期限が切れないように。Action の sequence は各ステップの前に検査する）。調停者の時計は
  使わず自分の単調時計で数える
- 取得の優先順位は**静的な優先度**（設定の `lease_priority`、小さいほど優先）で決め、同点は `instance_id` の
  辞書順。優先度の高い台が復帰しても**自動では奪わない**（fail-back しない。運用者が保持者を手放させる）。
  フラッピングを避けるため
- ttl の既定は 10 s、renew 間隔は ttl / 3（約 3 s）、取得の判定は「保持者の更新が ttl + margin を超えて
  途切れた」（5.3 の `holder_ttl_ticks` にも margin を含める）
- 役割ごとに独立（`mqtt-publisher` と `sink-writer` が別の台でもよい）。ただし既定では**全役割を同じ台に
  寄せる**（優先度が同じなら同じ台が取る）。分散させると障害時の状態が読みにくい
- **役割を 2 種に分ける**（2026-09-30 レビュー #2）:
  - **重複許容**: 同じ副作用が短時間 2 台から出ても害がない、または害を許容条件として明記した。
    `mqtt-publisher`（retain の最新値の鏡。B での巻き戻りは 4.7 の許容条件）、`recorder-primary`
    （読み出し時に正のソースを選ぶための印、6.2）
  - **排他必須**: 2 台から出ると不可逆な害がある。`action-executor`（PLC 書き込み等）、
    `sink-writer:{sink_group}`（サイドカーが宛先 DB 上で保持、4.8）、`alarm-authority`（6.3）、
    `config-primary`（4.3）
  - 排他必須の役割は**競合窓の無い調停（5.2 の A または D）でしか付与しない**。競合窓のある調停（B、C）しか
    無い構成では排他必須の役割は誰にも付与されず、その副作用は動かない（fail-closed。状態に「調停が排他必須の
    役割に不十分」と出す）
- 排他必須の役割でも `renew` の遅れは残る。**副作用の直前に `observe` するのは TOCTOU で保証にならない**ので、
  保証は調停側の単一書き手 / CAS（A は PLC が唯一の書き手、D は DB の行ロック）と ttl による時間の fencing で
  作り、`observe` は表示にだけ使う
- **fencing の保証レベル**（2026-09-30 レビュー 3 回目 #3）: 排他必須の役割でも、副作用の**受け側**が epoch を
  検査できるかどうかで保証が違う。役割ごとに明記し、状態の `guarantee` に出す:
  - **strict**: 受け側が同じトランザクション / 同じコマンドで epoch を検査し、遅れて届いた旧保持者の操作を
    捨てる。`sink-writer:{group}`（4.8、flush と lease 更新が同一トランザクション）、`alarm-authority`（6.3、
    イベントログが epoch を持ち、新 authority の epoch より古い採番は捨てる）、`config-primary`（4.3、
    調停者の committed generation をリース epoch 付きの CAS で書く）
  - **bounded-delay**: 受け側（PLC）が epoch を検査しない。A / D が保証するのは「有効なリースが同時に 2 つ
    無い」ことであって、**期限前に発行され遅れて届く旧保持者の PLC 書き込みを拒否することではない**。OS の
    サスペンド、長い scheduler pause、異常なネットワーク遅延で margin を超えると破れる。20% の margin は上限
    保証ではない。**`action-executor` の PLC 書き込みはこの水準**であり、「排他必須」は「有効なリースは 1 つ」
    の意味で、strict fencing ではないと設計上明記する。破れを小さくする手当: 単調時計の跳び（renew 間隔の
    2 倍以上）を検知したら自分のリースを即座に放棄して副作用を止める。書き込みのタイムアウトを短く保つ
    （既定 1 s）
  - strict にしたい PLC Action は **PLC 側の command gate**（5.3 の拡張。ラダーが epoch を検査してから
    適用する）を使う。候補 A が入れられる現場でだけ可能。§10 #17

### 5.2 実装候補

| 候補                                   | 正の置き場所                               | 分断への強さ                                                             | 前提                                 | 位置づけ                                                                               |
| -------------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------ | ------------------------------------ | -------------------------------------------------------------------------------------- |
| **A. PLC 調停（ラダー判定）**          | PLC 内のワード。**PLC が保持者を決める**   | 強い。PLC に届かない台は自動的に失格。PLC が唯一の書き手なので競合が無い | PLC 側にラダー（小さい）を入れられる | **第一候補**                                                                           |
| **B. PLC 調停（ワードのみ）**          | PLC 内のワード。Hub が読んで書く           | 強いが、同時取得の競合窓（1〜2 周期）がある                              | ラダー変更なし。ワードの予約だけ     | **重複許容の役割のみ**（MQTT 等）。A が入れられない現場の既定                          |
| C. 相互ハートビート + 静的優先度       | 各インスタンス（HTTP で相互に見る）        | 弱い。2 台が互いに見えないと**両方が保持者**になりうる                   | 何も要らない                         | 開発 / 検証用のみ。排他必須の役割は付与しない                                          |
| D. 外部の単一点（DB 行、共有ファイル） | PostgreSQL の行ロック、共有フォルダの lock | その単一点の可用性に依存。CAS があるので競合窓は無い                     | Sink 用 DB や共有フォルダがある      | **排他必須の役割の A の代替**（`action-executor` / `sink-writer` / `alarm-authority`） |
| E. 過半数（Raft 系）                   | インスタンス群                             | 強い（N≥3）                                                              | 3 台以上、実装が重い                 | **採らない**（scada-design.md §13.2）                                                  |

**PLC 調停を第一候補にする理由**: PLC が control authority である原則と整合する。PLC に届かない Hub は
そもそも読み取りも書き込みもできないので、「PLC に届く台の中で 1 台」を選ぶのが現場の意味に合う。
PLC 冗長系ではワードがトラッキングで引き継がれる。

### 5.3 PLC 調停のワード仕様（案）

設定で「調停 PLC」の接続と先頭デバイス（例 `D9000`）を指定する。1 論理サービスに 1 ブロック。

```text
D+0        arbiter_magic          固定値（ブロックが初期化済みか）
D+1..2     epoch (32 bit)         保持者が変わるたびに +1（A ではラダーが、B では新保持者が書く）
D+3        holder_slot            保持者のスロット番号（0 = 無し）
D+4        holder_ttl_ticks       保持者の更新猶予（PLC の 100 ms tick 単位、既定 120 = ttl 10 s + margin 2 s）
D+5        committed_index        committed_generation の有効バッファ（0 / 1）。ラダーが本体を書き終えてから 1W で切替
D+6..17    committed_generation[2]  generation token（96 bit = config_epoch 32 bit + fingerprint 先頭 64 bit）× 2
D+18       aborted_index          aborted_generation の有効バッファ（0 / 1）。ラダーが書く
D+19..30   aborted_generation[2]  fenced abort された pending の token × 2（4.3）
D+31       req_index              req の有効バッファ（0 / 1）。primary が本体を書き終えてから 1W で切替
D+32..49   req[2] { kind (1W: 1 = commit, 2 = abort), lease_epoch (32 bit), generation token (96 bit) } = 9W × 2
D+50..     slot[i] { instance_hash (32 bit), heartbeat_counter (32 bit), priority (16 bit),
                     token_index (1W), generation token[2] (96 bit × 2) } = 19W、i = 1..8（D+50..D+201）
D+210..    cmd（command gate、任意）{ epoch (32 bit), seq (32 bit), target (2W), value (2W), ack_seq (32 bit) }
```

- **多ワード値の一貫性**（2026-09-30 レビュー 6 回目 #4）: 96 bit の token や 9W の req を「1 つの値」として
  比較するので、SLMP の一括書き込みや PLC のスキャンの間で多ワード値が原子的に見えることに**暗黙には
  依存しない**（MELSEC はデバイスへの外部アクセスを END 処理で反映するので実機では概ね原子的に見えるが、
  仕様として保証を確認できるまで前提にしない。§10 #1 の実機確認項目に加える）。代わりに **double buffer +
  index word** で作る: 書き手は**有効でない側**のバッファに本体を書き終えてから、1 ワードの `*_index` を
  切り替える（1 ワードの書き込みは原子的）。読み手は index を読み、その側のバッファを読み、index を再読して
  一致していなければ読み直す。有効側のバッファには書き手は触らないので、途中読みは起きない。commit point 用
  データ（`committed_generation`、`req`、`aborted_generation`、slot の token）は全部この形
- **語長と wrap**（2026-09-30 レビュー 5 回目 #2 / #5）: 排他や fencing に使う番号（`epoch`、`config_epoch`、
  `heartbeat_counter`、`cmd.seq`）は **32 bit（2W）**。16 bit だと 65,536 で再利用され、「古い epoch の
  コマンドは必ず拒否する」strict fencing が成り立たない。32 bit は 1 秒に 1 回進めても約 136 年なので
  wrap は扱わない（wrap に達したらブロックを運用者が初期化し直す、と明記する）。generation token は
  `config_epoch`（32 bit）と fingerprint の先頭 64 bit を連結した **96 bit（6W）** とし、**slot /
  committed / req の全部で同じ token を持ち、ラダーと Hub は 6W 全部を比較する**。epoch だけの比較
  では同じ epoch で違う fingerprint の台が候補になり、32 bit の hash では req から完全に転記できない
- 各インスタンスは起動時に**スロット**を設定で固定して持つ（動的割当はしない。設定ミスは
  `instance_hash` の不一致で検出）
- ハートビート: 各台が自分の `heartbeat_counter` を renew 間隔で +1 する（PLC 書き込み 2 ワード、32 bit。
  ラダーは「変化したか」だけを見るので、桁上がりの途中読みは判定に影響しない）
- **A（ラダー判定）**: PLC が各スロットの counter の変化を監視し、`holder_ttl_ticks` の間変化が無ければ
  失格、生きているスロットのうち最小の priority を `holder_slot` に書き `epoch` を +1。Hub は
  `holder_slot` と `epoch` を読むだけ。書き手が PLC 1 つなので競合が無い。`config-primary` の判定では
  さらに `slot.generation_token == committed_generation`（6W 全部の一致）のスロットだけを候補にする
  （4.3 の昇格条件）。commit（4.3）は primary が `req`（非有効側）に `kind = 1`、自分のリース epoch、新
  generation token を書いて `req_index` を切り替え、ラダーが `req.lease_epoch == epoch` のときだけ token を
  `committed_generation`（非有効側）へ転記して `committed_index` を切り替える（保持者以外の req は捨てる）。
  abort（4.3）は `kind = 2` で同じ経路を通り、ラダーは `req.lease_epoch == epoch` かつ `committed != token`
  のときだけ `aborted_generation` へ転記する。commit と abort は同じ req バッファと同じラダーの判定を通るので
  両立しない。各台は自分の slot の token を switch 完了時に更新する（非有効側へ書いて `token_index` を切替）
- **B（ワードのみ）**: 各台が読み取り周期で全スロットを読み、保持者の counter が ttl を超えて止まって
  いたら「取得を試みる」= `holder_slot` と `epoch+1` を書く → 1 周期待って読み直し、`holder_slot` が
  自分なら取得成功、違えば負け。2 台が同時に書いた窓では**最大 2 周期の間、両方が保持者だと思う**。
  この窓があるため **B は重複許容の役割にしか使わない**（5.1）。MQTT の二重 publish は retain 値が
  1 サンプル巻き戻りうるが、4.7 の許容条件の範囲。排他必須の役割（Action、Sink、Alarm 採番、config-primary）
  には A か D を使う
- 役割ごとにブロックを分けるか、1 ブロックで全役割を同じ台に寄せるかは 5.1 の「既定は同じ台」に従い、
  初版は **1 ブロック = 全役割**とする
- **command gate（任意、strict fencing が要る PLC Action 用）**: 同じブロックの `cmd`（上の表）に、
  `action-executor` は対象デバイスへ直接書かず **`epoch`（32 bit）付きで書く**。ラダーは
  `cmd.epoch == epoch`（自分が決めた保持者の epoch）かつ `cmd.seq`（32 bit、epoch ごとに 1 から）が
  未処理のときだけ `target` へ `value` を転記し `ack_seq` を更新する。旧
  保持者の遅れたコマンドは epoch 不一致で捨てられる。1 コマンドずつ（seq の ack 待ち）なのでスループットは
  低いが、常時実行 Action の副作用は本来まれ。操作者の UI からの PLC Write はリースに縛られないので gate を
  通さない（scada-design.md §16 の相関 ID と監査が担う）

### 5.4 調停 PLC が無い / 届かない場合

- 調停 PLC に届かない台は `renew` が失敗し、副作用を止める（fail-stop）。**全台が届かなければ副作用は
  全停止**。これは「PLC が止まっているのに外へ値を出し続ける」より安全側
- PLC の無い構成（postgres / virtual のみ）は候補 D か C。既定は D（Sink 用 DB があれば行ロック）

---

## 6. 層 3: SCADA server の N 台化

前提: scada-server core（scada-design.md §13.2）は Hub の client であり、値は banto-tagclient で受ける。
論理サービス `service_id` と `instance_id` は §2 と同じ。

### 6.1 仕分け

| 種別             | 対象                                                         | 方針                                                                               |
| ---------------- | ------------------------------------------------------------ | ---------------------------------------------------------------------------------- |
| 全台で同じ       | Project package（revision 付き）、記録対象の設定、Alarm 定義 | Project package の配布（scada-design.md §5）。`project_revision` の一致を状態に    |
| 各台が独立に記録 | 履歴（tstore）、execution record、Alarm イベント             | **読み出し時に統合**（6.2、6.4）                                                   |
| 全台に複製       | 操作者の Alarm 状態（ACK / Shelve / Unshelve / コメント）    | **イベントの複製**（6.3）                                                          |
| 1 台だけ         | 常時実行 Action の副作用、Alarm occurrence の id 採番        | リース `action-executor`（6.4）、`alarm-authority`（6.3）。どちらも排他必須（5.1） |

### 6.2 recorder（Active/Active、読み出し時に統合）

- 各インスタンスが Hub（自分が接続している Hub インスタンス）から購読し、**自分の `data.dir`** に記録する。
  tstore にはプロセス間の排他が無いので、**`data.dir` をインスタンスで共有しない**（共有フォルダに置く場合も
  インスタンス別のサブフォルダ）
- 記録に `service_id` / `instance_id` / `project_revision` / Hub の `active_generation` をファイルの
  メタ（`tstore_meta`）に添える
- 読み出し（tsquery）は**複数の `data.dir` を束ねる**が、同一 `ptime` の突き合わせはしない（各台の時計と
  受信タイミングが違うので同じ `ptime` にならず、二重の系列が残る。2026-09-30 レビュー #5）。代わりに
  **「時間区間ごとに正のソースを 1 つ選び、その区間はそのソースの行だけを使い、そのソースの欠測区間だけを
  他のソースで補完する」**。正のソースは役割 `recorder-primary` の保持者で、各インスタンスは自分がその
  役割を保持した区間 `(epoch, from, to)` を自分の tstore に **`tstore_lease_log`** として記録する。読み出しは
  まず全ソースの lease_log を集めて区間表を作り（重なりは `epoch` 大、同点は `lease_priority` 小が勝つ）、
  区間ごとにそのソースを読む。欠測（保持者の行が周期の 2 倍以上空く）だけを他ソースの同区間で埋め、補完した
  行には `source_instance_id` を付けて返す。これで raw query も Replay も**決定的に 1 系列**になる
- `recorder-primary` は副作用の無い「印」なので重複許容の役割（5.1）。候補 B の窓で 2 台が同時に保持しても、
  区間表の勝敗規則で決定的に解決する
- 時計: `ptime` は各台の時計。区間の境界（切替の瞬間）で数百 ms のずれや重なりが出るのは許容し、NTP 同期は
  運用要件（§10 #6）
- tsquery の read API に `sources: [dir...]` と上の統合規則を足す。単一ソースのときは今と同じ
- Replay（scada-design.md §13.3）の前提条件 1〜3 はここに乗る。Replay driver は統合済みの読み出しを使う

### 6.3 Alarm（評価は全台、occurrence の id は 1 台が採番、操作者状態はイベントを複製）

- 評価は各台が自分の受信値で行う（表示と可用性のため）。ただし **Alarm occurrence の同一性は値の時刻からは
  作れない**（各 Hub が独立にポーリングし各自の時計で `t` を刻むので、同じ PLC 変化でも H1 / H2 の `t` は
  一致しない。2026-09-30 レビュー #1）。同一性は**採番の authority を 1 台に置く**ことで作る:
  - 役割 **`alarm-authority`**（排他必須、5.1）の保持者が occurrence id を採番する。id は
    `(alarm_definition_id, epoch, seq)`。`epoch` はリースの epoch、`seq` は epoch 内の単調増加。epoch が
    単調なので failover をまたいでも衝突しない
  - authority は occurrence の `opened` / `returned` / `closed` をイベント（下のイベントログ）として全ピアへ
    複製する。イベントに定義 id と authority 側の発生時刻を添える
  - authority 以外の台は自分の評価で**仮の occurrence**（id 未確定）を持ち、authority の `opened` が届いたら
    `(alarm_definition_id, active)` で突き合わせて id を確定する。届く前は表示は出すが**操作（ACK / Shelve）は
    受けない**（id 未確定を UI に示す。通常は数百 ms。§10 #11）
  - failover: 新しい authority は**サービス開始前に catch-up barrier を通す**（2026-09-30 レビュー 4 回目
    #4、5 回目 #3）。barrier は **quorum 方式**で、「応答不能を確認したから安全」とはしない: authority 候補
    N 台のうち **自分を含む `⌈(N+1)/2⌉` 台**と範囲同期（各 replica の API から、origin ごとに自分の
    watermark 以降の不足範囲を取り込む。origin 本体が死んでいてもよい）が完了するまで
    採番も操作の受付も始めない。ACK 側の同期複製（下）も同じ `⌈(N+1)/2⌉` 台の永続化を成功条件にするので、
    書き込み側と読み出し側の集合が必ず交わり、**単一ノード故障 + 分断の組み合わせでも ACK 済みが
    未 ACK に戻らない**（S1 / S2 に ACK、S3 が分断中 → S1 停止 → S3 が調停者には届き authority を取っても、
    S2 と同期できなければ barrier を抜けられない）。quorum が揃わない間は**fail-closed**（リースは保持、
    採番と操作は止め、UI に「引き継ぎ中: 同期待ち」）。運用者は `force-authority` で強制開始できるが、
    その場合は ACK の継続性を保証しないと明記する（§10 #19）。barrier 後、自分の評価で active な
    occurrence について、複製済みイベントに `opened` があり `closed` が無いものは**その id を引き継ぎ**、
    無いものだけ新しい `seq` で採番する。**N=2 は quorum = 2 なので相手が死ぬと barrier を抜けられない**
    （degraded で受けた ACK は保証外）。Alarm 操作の継続性を求める現場は N=3 を推奨する
  - **authority 候補集合（membership）は固定**（2026-09-30 レビュー 6 回目 #3）: quorum の交差は「同じ N 台の
    majority」が前提なので、**Alarm のイベントログが存在する間は候補集合を直接置き換えない**（旧集合
    {S1,S2,S3} で {S1,S2} に保存された ACK は、新集合 {S3,S4,S5} の quorum {S3,S4} と交わらない）。設定
    ファイルの peers を書き換えただけでは有効にならず、**ログ内の `membership_revision` イベントが正**。
    初版の変更手順は**計画停止のみ**: authority を止める → 全候補の watermark を全 origin で揃える → 旧集合の
    quorum で `membership_revision` を永続化 → 新集合で再開。無停止の joint transition（旧・新両方の quorum
    を要求する遷移）は後続（§10 #21）
  - 各台の評価結果が食い違う（一方だけが発報している）ときは authority の判断が正。authority 以外の台の
    仮 occurrence は、`opened` が来ないまま自分の評価で復帰したら黙って消す（履歴には残さない）
- 操作者イベント（ACK / Shelve / Unshelve / コメント）は **append-only のイベントログ**として各台が
  永続化（SQLite、commit 後に応答）し、受けたインスタンスが**全ピアへ転送**する。各台は受け取った
  イベントを自分の Alarm 状態に適用する
- **ACK の耐久性は同期複製で作る**（2026-09-30 レビュー 2 回目 #4）: 受けた台はローカルに永続化した後、
  ピアへ転送して**`ack_replicas` 台が永続化を確認するまで操作者へ成功を返さない**（転送のタイムアウト
  2 s）。`ack_replicas` の既定は**自分を含む `⌈(N+1)/2⌉` 台**（quorum。barrier と対にする。2026-09-30
  レビュー 5 回目 #3。N=2 なら両方、N=3 なら 2 台）。到達可能な台が多ければ全台へ送るが、成功条件は
  quorum。best-effort の非同期転送では「ローカル保存 → 成功応答 → 転送前に停止」で
  ACK が失われ、failover 後に未 ACK に戻る。保証は**「ACK 時点で別 1 台に届いていれば、1 台故障まで
  ACK は残る」**。確認が取れないとき（ピアが全部落ちている）の既定は **degraded**: ローカルに永続化して
  成功を返すが `replicated: false` を応答と UI に出す（単一台運用と同じ状態。§10 #13）。`strict` を選べば
  拒否する。ピアが復帰したら差分同期で追いつく
- **因果複製**（2026-09-30 レビュー 3 回目 #2）: ACK は `opened` を参照するので、**ACK だけを同期複製しても
  `opened` が届いていない台では孤児になる**（`opened(A)` 未転送 → `ACK(A)` だけ同期保存 → S1 停止 → S2 は
  `opened(A)` を持たず `B` を採番し、`ACK(A)` は宙に浮く）。よって操作者イベントの同期複製では、**そのイベント
  が依存するイベント（同じ occurrence の `opened` と先行する操作者イベント）のうちピアの watermark に含まれて
  いないものを束ねて送り**、ピアは束ねられた分をまとめて永続化してから ack する。これで ACK が残る台には
  必ず `opened` も残り、failover 先は同じ id を引き継げる。`opened` / `returned` / `closed` 単体は非同期の
  複製でよい（操作者イベントが無い occurrence は、authority が死んでも新 authority が採番し直すだけ）。
  依存関係は occurrence 単位で閉じているので、束ねる量は小さい
- イベントの識別と順序（2026-09-30 レビュー #4）: `event_id = (origin_instance_id, origin_seq)`。
  `origin_seq` は発生元ごとの単調増加番号で、**イベント本体と同じトランザクションで永続化する**（ログの
  `max(seq) + 1`。再起動で 1 に戻らないので、ピアの watermark と衝突しない。2026-09-30 レビュー 2 回目
  #5。`boot_id` を id に含める案は不要）。各台は **origin ごとに watermark = 欠番なく連続して受信済みの最大
  `origin_seq`** を持つ（2026-09-30 レビュー 4 回目 #3。「受信した最大値」にすると、seq=10 が未着のまま
  11 が先に届いた瞬間に 10 を永久に取りこぼす。HTTP の再送と因果 bundle がある以上、順不同の到着は普通に
  起こる）。watermark より先のイベントは受信済みとして永続化するが watermark は進めず、欠番が埋まったときに
  まとめて進める。**各 replica は、自分が保持する全 origin のイベントを `(origin_id, from_seq, to_seq)` で
  提供する API を持ち、差分同期と barrier は origin 本体ではなく到達できる任意の replica（quorum の相手）に
  origin ごとの不足範囲を要求する**（2026-09-30 レビュー 6 回目 #2。origin だけに問い合わせる方式では、
  ACK の origin=S1 が停止し S2 が複製を持っていても S3 が回収できない）。replica は seq 順で返す（UUID は
  順序が無いのでカーソルにしない）。競合（同じ
  occurrence に別の台で別の操作）の順序は **HLC（hybrid logical clock）+ `origin_instance_id`** の全順序で
  決め、最後が勝つ。wall-clock の順序は使わない。**HLC の high-watermark は永続化する**（2026-09-30
  レビュー 3 回目 #5）: イベントの永続化と同じトランザクションで `last_hlc` を更新し、起動時は
  `max(last_hlc, ログ内の最大 HLC, 受信済みイベントの最大 HLC, wall-clock)` から seed する。時計が後退した
  状態で再起動しても論理部が進むので、新しい操作が過去のイベントに LWW で負けない。**稼働中も、リモートの
  イベントを永続化するたびに同じトランザクションで `local_hlc = merge(local_hlc, remote_hlc, wall_clock)` を
  更新し、以後のローカルイベントは必ずその先から採番する**（2026-09-30 レビュー 6 回目 #5。S1 の
  ACK(HLC=100) を受けた S2 が HLC=50 のままだと、その後の S2 での Unshelve が過去扱いになって LWW で負ける）
- Alarm API はどのインスタンスも同じ内容を返す（イベント適用の遅延分だけずれる）。Alarm Viewer は
  接続中のインスタンスを見る
- 転送が失敗して届かなかったイベントは、ピアが復帰したときに watermark からの範囲同期で追いつく。同期は
  定期（5 s）にも行い、欠番を放置しない
- `alarm-authority` は既定で `action-executor` と同じ台に寄せる（5.1 の既定）。排他必須なので調停が A / D で
  無い構成では付与されず、その場合は**冗長構成での Alarm 操作は不可**（単一台の運用に戻すか、調停を整える）

### 6.4 常時実行 Action（リース対象）と execution record

- engine は Action の実行前に `renew` の成否（保持者か）を見る（scada-design.md §21 S10b の
  「自分が実行担当か」の口）。保持者でなければ**評価はするが実行しない**（評価結果は記録する）
- execution record（scada-design.md §16）に `instance_id` と `epoch` を添える。統合は「読み出し時に全台
  から集めて相関 ID で並べる」。同じ相関 ID が複数台にあれば二重実行の証拠として警告する（A / D では
  起きない想定なので、起きたら調停の不具合として扱う）

### 6.5 client（画面）の切替

- 共有サービス接続モード（scada-design.md §13.2）の client は**SCADA server のエンドポイントのリスト**を
  持ち、§7 と同じ規則で切り替える
- 画面はどのインスタンスに接続していても同じ Project revision を見る。不一致は「未接続」と同じ扱いで
  表示し、書き込み（Action）は 403

---

## 7. client の切替（banto-tagclient）

- `Endpoint` を**順序付きリスト**に拡張する（長さ 1 が現状）。`RestClient` は「今のエンドポイント」を
  持ち、失敗の種類で次へ進む:
  - `Transport` / `CatalogUnavailable`: 同じエンドポイントで backoff 再試行を **2 回**（1 s、2 s）まで、
    その後**次のエンドポイント**へ。全部回ったら backoff を伸ばして先頭から
  - 1008（認証拒否、`Unauthorized`）: そのエンドポイントは**終端**にし、次へ進む（4.4 の (b) では
    エンドポイントごとにキーが違うので、キーもエンドポイントごとに持つ）
  - `config_changed` → 同じエンドポイントで rebind（現状どおり）
- **世代の同一性**（ChronoGazer の「正規化 endpoint + 外部名集合」）はエンドポイントが変わると世代が
  変わる。publish gate は世代内で閉じるので変更不要。**再バインドは名前**なので別インスタンスでも通る
  （scada-design.md §9.6 の決定の効き所）
- `active_generation` を catalog 取得時に読んで、**client が受け入れた generation（`accepted_generation`）と
  違えば警告イベント**を出し、**書き込みを client 側でも止める**（Hub 側のゲート（4.3）と二重にする。値は
  出す）。**解除条件**（2026-09-30 レビュー 4 回目 #6）: 正常な有効化でも generation は変わるので、
  「新 generation の catalog で rebind が成功し、かつ server の `/api/v1/status` で `write_gate == open` と
  同じ `active_generation` を確認した」時点で `accepted_generation` を更新して書き込みを再開する。それまでは
  止めたまま。Hub 側の設定が台間でずれている検出は client でもできるようにする
- 書き込みは今のエンドポイントへ 1 回。切替中は失敗を返す（現状どおり）
- fail-back しない（つながっている限り戻らない）。優先順位は運用者がリストの順で表す
- 状態に `endpoint_index` と `endpoint` を出し、SCADA の状態画面は「どの Hub インスタンスを見ているか」を
  表示する

---

## 8. 障害シナリオ

前提: Hub 2 台（H1 優先、H2）、SCADA server 2 台（S1 優先、S2）、PLC 冗長系（A / B）、調停は PLC
（候補 A）。

| #   | 事象                                                             | 期待する振る舞い                                                                                                                                                                                                                                                                                                                                          |
| --- | ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | H1 のプロセス停止                                                | H1 のハートビートが止まり ttl + margin 後に H2 が Hub 側の役割（`config-primary`、MQTT）を取得。H1 側サイドカーの DB リースも期限切れになり H2 側サイドカーが取得して INSERT 開始（台をまたぐ重複行は無い）。client は Transport 失敗で H2 へ切替、名前で再バインド。H2 は新 primary なので書き込みを受ける。切替の間（ttl + margin + backoff）の値は欠落 |
| 2   | H1 の NIC 断（PLC には届くが LAN に出ない）                      | client は H2 へ。H1 は PLC には届くので PLC 調停上は保持者のまま → **MQTT は H1 が続けようとする**（ブローカーに届くかは別）。Sink は H1 側サイドカーが DB に届かず DB リースが切れ、H2 側が取る。H2 は primary を確認できず（鮮度失効）書き込みを受けない → 運用者が H1 の役割を手放させる。この事象は**調停が PLC 側にある弱点**として §10 #8 に残す    |
| 3   | H1 と PLC の間の断（LAN は生きている）                           | H1 の renew が失敗し副作用停止。H2 が取得。H1 の値は Bad、client は値が Bad でも接続は維持（H1 が PLC に届かないだけで Hub は生きている）→ **client 側の切替条件に「全接続が Bad」を足すか**は §10 #9                                                                                                                                                     |
| 4   | PLC の系切替（A → B）                                            | 各 Hub の接続層が endpoint を切替。切替中は Bad。調停ワードはトラッキングで引き継がれるので保持者は変わらない                                                                                                                                                                                                                                             |
| 5   | H1 と H2 の相互断、両方 PLC には届く                             | 調停は PLC なので保持者は 1 台のまま。client は自分が届く方へ。**分断しても副作用は 1 台**                                                                                                                                                                                                                                                                |
| 6   | 全 Hub が PLC に届かない                                         | 全役割が失効、副作用は全停止。client は値 Bad。安全側                                                                                                                                                                                                                                                                                                     |
| 7   | S1 停止                                                          | S2 が `action-executor` / `alarm-authority` を取得し、複製済みの `opened` から occurrence id を引き継ぐ（ACK 済みは ACK 済みのまま）。recorder は S2 が続けており、区間表で S2 が正になる。画面は S2 へ切替                                                                                                                                               |
| 8   | 設定変更（primary H1 で編集、H2 に未有効化）                     | 編集は H1 の staging に入り running には触れない。activate は H2 の ack が要るので、H2 が分断されていれば失敗して全台が旧 generation のまま。**H1 も新設定では書けない**（primary の例外なし）。運用者が有効化を完了させる。除外運用で H2 抜きに有効化した場合、H2 は `excluded` で書き込みを受けず primary にもなれない                                  |
| 9   | 全停止からの再起動                                               | 起動順に依存しない。各台が起動 → 調停ワードを読む → 保持者が居なければ優先度順に取得。MQTT の `$state` は保持者が `online` にする                                                                                                                                                                                                                         |
| 10  | 時計のずれ                                                       | リースは各台の単調時計で数えるので影響なし。履歴の区間境界（6.2）に数百 ms の影響。Alarm イベントの順序は HLC なので影響なし → NTP を運用要件に                                                                                                                                                                                                           |
| 11  | 調停が候補 B / C しか無い構成                                    | 重複許容の役割（MQTT、recorder-primary）だけが付与され、`action-executor` / `alarm-authority` / `config-primary` は誰も持たない（Sink は DB があれば D で持てる）。常時実行 Action の副作用と Alarm 操作は止まり、書き込みは固定 primary を選んだときだけ primary で受ける。状態に理由が出る（fail-closed）                                               |
| 12  | S1 が ACK 直後に停止                                             | ACK は依存する `opened` と束ねて S2 の永続化確認後に成功を返しているので、S2 は同じ occurrence id で ACK 済みを引き継ぐ。S2 が ACK 時点で落ちていた（degraded で受けた）場合は失われうる。2 台同時故障は保証外                                                                                                                                            |
| 13  | primary H1 が staging に未有効化の編集を残して停止               | 調停者の committed generation は最後に commit したもの。H2 の `active_generation` がそれと一致するので H2 が primary を取得し書き込みを受ける。H1 の編集は復帰後に「未有効化」として残る                                                                                                                                                                  |
| 15  | 有効化の途中で配布失敗（H2 は prepare 済み、H3 に届かない）      | commit point に達していないので誰も切り替えない。H2 の pending は次の prepare で上書きされる。primary は旧 generation のまま書き込みを受ける                                                                                                                                                                                                              |
| 16  | H2 が最新の有効化を知らないまま分断され、その間に primary が停止 | H2 の `active_generation` は調停者の committed generation と一致しないので `config-primary` を取れない。誰も一致しなければ書き込み不可（安全側）。H2 は復帰後に catch-up                                                                                                                                                                                  |
| 17  | N=3 で S1 が ACK 直後に停止、S3 が authority を取得              | ACK は quorum（2 台）に同期複製済み。S3 は barrier で S2 と同期してから採番するので同じ id を引き継ぐ。S2 にも届かなければ quorum が揃わず fail-closed（採番せず待つ）                                                                                                                                                                                    |
| 18  | commit 直後、H2 の switch が遅れる                               | H2 は prepare ack の時点で書き込みを閉じており、switch 完了まで開けない。仮に閉じ忘れても調停者の committed generation と自分の active が違うのでゲートは閉じる。旧 generation のアドレスへの書き込みは起きない                                                                                                                                           |
| 19  | S3 が分断中に S1 / S2 で ACK、その後 S1 停止                     | S3 が調停者に届いて authority を取っても、S2 と同期できなければ barrier を抜けられない（fail-closed）。S2 と通じれば S2 の replica API から origin=S1 の範囲を回収して引き継ぐ。`force-authority` は保証外                                                                                                                                                |
| 20  | prepare 済みの H2 で 60 s 経過、その直前に commit                | H2 はローカル timeout ではゲートを開け直さない（`stale pending` 表示のみ）。次の読み取りで調停者の committed == pending を見て switch する。abort されていれば aborted == pending を見て開け直す                                                                                                                                                          |
| 21  | ACK の origin S1 が停止、複製は S2 にだけ                        | 新 authority は S2 の replica API に origin=S1 の不足範囲を要求して回収する。origin にしか問い合わせない方式では取れない                                                                                                                                                                                                                                  |
| 14  | `action-executor` の S1 が OS サスペンド                         | S1 の renew が止まり ttl + margin 後に S2 が取得。S1 が復帰した瞬間に期限前に発行済みだった PLC 書き込みが遅れて届く余地があり（bounded-delay、5.1）、S1 は単調時計の跳びを検知して即座にリースを放棄する。strict が要る Action は command gate（5.3）                                                                                                    |

---

## 9. 段階導入

scada-design.md §22 #19 の「単一構成が動いてから」に従い、**単一構成の段階で入れておくもの（R0）**と
冗長化本体を分ける。

- **R0（単一構成に先に入れる。冗長化の有無に関わらず価値がある）**
  - `service_id` / `instance_id` / `active_generation` / `config_fingerprint` を Hub の `/api/v1/status` に出す
  - 設定パッケージのサーバー側 export / import API（SCADA の Hub 連携でも使う）
  - SLMP の `io_id` / `network_id` / `pc_id` を registry から指定できるようにする（既知の制約の解消）
  - `plc_connections` の endpoint リスト化（長さ 1 で既存と同じ）
  - banto-tagclient の endpoint リスト化（長さ 1 で既存と同じ）と `active_generation` の観測
  - MQTT `client_id` の既定を `banto-hub-{instance_id}` に
  - scada-server core の engine に「自分が実行担当か」の口（scada-design.md §21 S10b）
- **R1 リース**: `Lease` trait と役割の 2 種（5.1）、期限の規則（最後に成功した Grant + ttl、操作前の
  `now + timeout < deadline`）、候補 B（PLC ワードのみ。重複許容の役割用）、候補 D（DB 行の CAS。排他必須の
  役割用）、候補 C（開発用）の実装、保証レベル（strict / bounded-delay）の表示。MQTT を B で。Sink サイドカーの
  group ごとの DB リースと flush 同一トランザクションの検査（4.8）。候補 A のラダーは実機で試作
- **R2 Hub 2 台**: 設定の generation（staging / prepare / 調停者の commit point / switch / 昇格条件）と鮮度、
  3 者一致の書き込みゲートと有効化中の閉鎖、fenced abort（4.3）、PLC ワードの double buffer（5.3）、`config-primary` のリース化と固定 primary の選択肢、API キーは (b)、client の
  `accepted_generation`（§7）、状態画面。実機で §8 の #1 / #2 / #3 / #5 / #8 / #9 / #13 / #15 / #16 を確認
- **R3 SCADA server 2 台**: recorder の区間表による統合（`tstore_lease_log`）、`alarm-authority` の採番と
  occurrence イベント、操作者イベントの因果複製（依存する `opened` を束ねる、origin seq と HLC の永続化、
  origin ごとの連続 watermark と任意 replica からの範囲同期、HLC の受信時 merge、`ack_replicas` = quorum）、
  昇格前の quorum barrier、固定 membership と計画停止の変更手順、execution record の統合、画面の切替
- **R4 N≥3 と PLC 冗長系**: スロット 3 以上の実機確認、MELSEC 冗長系の `io_id` = 0x03D0 の確認、
  読み取り中継モード（接続数 1 の機器）、PLC 側 command gate（strict fencing、5.3）
- ChronoGazer 単体（記録計商品）は R3 の recorder 統合を crate 単位で共有するが、単体商品としての
  冗長化は対象外

---

## 10. 未決事項（オーナー判断待ち）

1. **MELSEC 冗長系の宛先指定**（3.2）: `io_id` = 0x03D0 を待機系ポートに投げたときの挙動と、応答エコーの
   扱い。実機確認が要る。手元の R08ENCPU（非冗長）では確認できない。あわせて SLMP の一括書き込みが
   スキャンに対して原子的に見えるかを確認する（5.3 は保証を前提にせず double buffer で組む）
2. **PLC 接続数と Hub 台数**（4.2）: インスタンス別ポートを第一候補としてよいか。接続数 1 の機器向けの
   読み取り中継モードを初版に含めるか（推奨: 含めない、R4）
3. **排他必須の役割の調停**（5.1、5.2）: `action-executor` / `sink-writer` / `alarm-authority` は候補 A
   （PLC ラダー）か D（DB 行ロック）でしか付与しない案でよいか。A のラダーを現場に求められないなら D が
   既定になり、Sink 用 DB も無い現場では排他必須の役割が動かない（重複許容の役割だけ冗長化される）
4. **内部タグの複製**（4.6）: 初版は複製しない案でよいか
5. **API キーの配布**（4.4）: (b) インスタンスごとのキーを初版としてよいか
6. **時計同期を運用要件にする**（6.2、§8 #10）: NTP 必須と文書化してよいか
7. **Sink の一意制約と列**（4.8）: 冗長構成では `(ts, external_name)` を推奨に変え、`instance_id` / `epoch`
   列を足す。リースは sink group 単位で宛先 DB に置く（external-db-design.md §5.4 の追記）
8. **PLC には届くが LAN に出ない Hub**（§8 #2）: 調停に「client から到達できること」を混ぜるか。混ぜると
   調停が PLC だけで閉じなくなる。推奨: 混ぜず、運用者の手動切替と監視で対応
9. **client の切替条件に「値が全部 Bad」を含めるか**（§8 #3）: 含めると PLC 停止時に client が Hub 間を
   往復する。推奨: 含めず、Bad は Bad として表示する
10. **固定 primary を選択肢として残すか**（4.3）: A / D の無い現場向けに固定 primary を残す案でよいか
    （primary 停止中は全台で書き込み不可）。残さないなら、そうした現場では冗長構成での書き込みは不可になる
11. **id 未確定の Alarm の扱い**（6.3）: authority の `opened` が届くまで操作（ACK / Shelve）を受けない案で
    よいか（表示はする）。受ける案は、仮 id で受けて確定後に読み替える複雑さが増える
12. **書き込み fail-closed の粒度**（4.3）: インスタンス単位（fingerprint 不一致なら全タグ拒否）でよいか。
    タグ単位（差分のあるタグだけ拒否）は設定差分の計算が要る。推奨: インスタンス単位
13. **ACK の複製確認が取れないときの既定**（6.3）: degraded（ローカル永続化で成功を返し `replicated: false`
    を表示）を既定にしてよいか。strict（拒否）を既定にすると、ピアが落ちている間は操作者が ACK できない。
    `ack_replicas` の既定は quorum（自分を含む `⌈(N+1)/2⌉` 台）
14. **候補 B での MQTT の巻き戻りを許容条件にしてよいか**（4.7）: 許容できない現場は `mqtt_strict` で A / D に
    寄せる。既定は許容
15. **`confirm_ttl`（鮮度）の既定 30 s**（4.3）: primary の確認が 30 s 途切れたら standby は書き込みを止める。
    短いほど安全で、長いほど一時的な遅延に強い
16. **有効化の配布セットの既定**（4.3）: peers 全員の ack を要求する（落ちている台があると有効化できない。
    運用者が明示的に除外する）案でよいか。過半数にすると除外なしで進むが、取り残された台の catch-up が常態化する
17. **PLC 側 command gate を標準にするか**（5.1、5.3）: `action-executor` の PLC 書き込みは既定では
    bounded-delay の時間 fencing。strict が要る Action にだけ command gate を使う案でよいか。標準にすると
    候補 A のラダーが必須になる
18. **commit point の置き場所**（4.3、5.3）: 候補 A では PLC のワード（ラダーが保持者の commit だけ転記）、
    候補 D では `config-primary` のリース行。両方ある現場でどちらを正にするか（推奨: リースと同じ調停者）
19. **Alarm の barrier を quorum で fail-closed にする**（6.3）: N=2 では相手が死ぬと authority が開始できず、
    運用者の `force-authority`（保証外）が要る。既定を fail-closed にしてよいか。Alarm 操作の継続性が要る
    現場に N=3 を推奨する文言でよいか
20. **有効化中の書き込み停止**（4.3）: prepare ack から switch 完了まで全台で書き込みを閉じる案でよいか。
    有効化は運用者の操作なので短いが、その間は操作者の PLC Write も 409 になる。primary が commit も abort も
    できずに死んだ場合は、新 primary の昇格（committed と一致する台）を待って fenced abort する
21. **Alarm の membership 変更を計画停止のみにする**（6.3）: 初版は候補の追加・削除に authority の停止と
    watermark の一致確認を要求する。無停止の joint transition は後続でよいか

---

## 11. 決定記録

| 日付       | 決定                                                                                                                                                                                                                                                                                                                                                                                                                                             | 出所                              |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------- |
| 2026-09-30 | 3 層独立、読み取り・評価は全台、副作用は 1 台（リース）、PLC 調停第一候補、名前束縛が前提、SCADA server だけ先行しない                                                                                                                                                                                                                                                                                                                           | scada-design.md §13.2（オーナー） |
| 2026-09-30 | 本草案を作成。§3〜§9 は提案、§10 はオーナー判断待ち                                                                                                                                                                                                                                                                                                                                                                                              | 本書                              |
| 2026-09-30 | オーナーレビュー（PR #475 1 回目）の方向: クロスインスタンスの同一性、Action の真正な fencing、設定不一致時の write fail-closed を先に固める。→ 6.3 の `alarm-authority`、5.1 の役割 2 種と A / D 限定、4.3 の fail-closed、6.3 の origin seq + HLC、6.2 の区間ごとの正ソースに反映                                                                                                                                                              | PR #475 レビュー（オーナー）      |
| 2026-09-30 | オーナーレビュー（PR #475 6 回目）の方向: pending timeout と commit の競合を調停者上で解消する、Alarm quorum recovery が非 origin の replica からもイベントを回収できるようにする、quorum membership の変更規則を固定する。→ 4.3 / 5.3 の fenced abort と req / aborted のワード、6.3 の replica API と origin ごとの watermark、固定 membership と計画停止の手順、5.3 の double buffer、HLC の受信時 merge、heartbeat の 2 ワード表記に反映     | PR #475 レビュー（オーナー）      |
| 2026-09-30 | オーナーレビュー（PR #475 5 回目）の方向: 書き込みゲートに調停者の committed generation を直接入れる、PLC 調停者が generation 全体を比較できる表現にする。→ 4.3 の 3 者一致ゲートと有効化中の閉鎖、5.3 の 96 bit generation token と 32 bit の epoch 類、6.3 の quorum barrier と `ack_replicas` = quorum、4.8 の `clock_timestamp()` と行ロック先行に反映                                                                                       | PR #475 レビュー（オーナー）      |
| 2026-09-30 | オーナーレビュー（PR #475 4 回目）の方向: 設定の generation に共有された単一の commit point を置く（有効化の途中故障と stale primary の昇格を同じ仕組みで解消）、Alarm は連続 watermark と authority 昇格前の catch-up barrier。→ 4.3 の prepare / commit / switch と調停者基準の昇格、5.3 の committed generation ワード、6.3 の連続 watermark・範囲同期・barrier・`ack_replicas` 既定、4.8 の `expires_at >= now()` guard、§7 の解除条件に反映 | PR #475 レビュー（オーナー）      |
| 2026-09-30 | オーナーレビュー（PR #475 3 回目）の方向: 設定の「配布」と「有効化」を分ける、Alarm の `opened → ACK` の因果関係を耐久化する、PLC Action の fencing の保証レベルを正確に定義する。→ 4.3 の generation と昇格条件、6.3 の因果複製と HLC 永続化、5.1 の strict / bounded-delay と 5.3 の command gate、4.8 の group 単位のリース、4.7 の `t` 記述削除に反映                                                                                        | PR #475 レビュー（オーナー）      |
| 2026-09-30 | オーナーレビュー（PR #475 2 回目）の方向: リースの取得が排他でも、その権利が INSERT / PLC write / ACK の耐久性まで切れ目なく伝わっていなければならない。→ 5.1 の期限の規則、4.8 のサイドカー自身の DB リースと同一トランザクション検査、4.3 の鮮度と `config-primary` のリース化、6.3 の同期複製と seq の永続化、4.7 の許容条件に反映                                                                                                            | PR #475 レビュー（オーナー）      |
