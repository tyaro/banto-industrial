# banto-hub / SCADA server 冗長化 設計草案

作成日: 2026-09-30  
最終更新: 2026-09-30（初版。scada-design.md §13.2 の原則を受けて、3 層の冗長化とリースの抽象を草案化）  
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

| 種別                     | 対象                                                                                                     | 方針                                                                                |
| ------------------------ | -------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| 全台で同じでなければ困る | 接続・グループ・タグ定義、計算タグ、Sink グループ、MQTT / gRPC 設定、write-control、コミッショニング状態 | **設定パッケージ**で配布し、`config_fingerprint` の一致を状態で見せる（4.3）        |
| 全台で同じだが機微       | API キー、ユーザー、PLC パスワード、MQTT パスワード                                                      | 設定パッケージには入れない（現状どおり）。**別経路**で配布（4.4）                   |
| 各台が独立に持つ         | 現在値、品質、`revision`、`run_id`、Hub 内 tstore（7 日）、`collect_events`、`hub_write_audit`、監査ログ | 複製しない。読み出し側（運用者・SCADA）が必要なら統合する                           |
| 1 台だけが行う           | MQTT publish、Sink の DB 書き込み、（将来）設定の編集                                                    | **リース**（§5）で役割を 1 台に限定                                                 |
| どちらとも言えない       | 内部タグ（`mem`）の値                                                                                    | 初版は**複製しない**（制約として明記）。後続で保持者からの転送を検討（4.6、§10 #4） |

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

### 4.3 設定の配布と一致確認

- **設定の正は 1 台**（役割 `config-primary`）。初版は**固定**（設定で指定した instance が primary）、
  リース化は後続。primary 以外の管理 UI は設定編集を読み取り専用にする（`/api/v1/status` の
  `config_primary: false` を見て UI が閉じる）
- 配布は**設定パッケージ**（既存の export / import）を使う。ただし現状はクライアント側 TS の機能なので、
  **サーバー側の export / import API** を足す（scada-design.md §5 の Project package と同じ発想。
  SCADA の Hub 連携でも要る）。import は現状どおり非トランザクションで、失敗したら fingerprint 不一致として
  見える
- 各インスタンスは `config_fingerprint`（設定パッケージ相当の内容の正規化ハッシュ。数値 id と機微情報を
  除く）を `/api/v1/status` に出す。**運用者と SCADA は fingerprint の一致で「同じ Hub」と判断する**。
  不一致は警告（値は出し続ける、書き込みは止めない）
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

### 4.8 DB Sink（リース対象）

- Sink サイドカーは**インスタンスごとに 1 つ**置き、設定は各自の Hub から取る（現状どおり）
- 役割 `sink-writer` の保持者に接続しているサイドカーだけが INSERT する。他はキューを溜めず購読だけ
  続ける（切替時に古い値を大量投入しない）。保持者の Hub は `/api/sink/config` に `writer: true|false`
  を含めて渡し、サイドカーは 5 s ごとの状態往復で変化を拾う
- 切替の前後で**重複と欠落の両方が起こりうる**（at-least-once、リース更新間隔の分）。重複の吸収は DBA の
  一意制約に任せる現状を維持するが、**`tag_id` はインスタンスごとの SQLite id なので台間で一致しない**。
  冗長構成の一意制約は **`(ts, external_name)`** を推奨し、external-db-design.md §5.4 に追記する
  （§10 #7）
- 欠落は「切替の間」に限られ、履歴の正は SCADA server / ChronoGazer の recorder（層 3）にある

### 4.9 状態の見せ方

`/api/v1/status` に足す: `service_id`、`instance_id`、`config_fingerprint`、`config_primary`、
`leases: [{role, held, epoch, holder_instance_id, expires_in_ms}]`、`peers: [{instance_id, endpoint,
reachable}]`（peers は設定に書いた静的リスト。発見はしない）。管理 UI のトレイと状態画面はこれを表示する。

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
  execution record、MQTT のペイロードのメタ）は古い epoch を捨てられる。PLC 書き込みは epoch を
  見られないので、リース失効後の書き込みは**時間で**防ぐ（`ttl` の半分で renew、renew 失敗から
  `ttl` 経過までに必ず止める。調停者の時計は使わない、自分の単調時計で数える）
- 取得の優先順位は**静的な優先度**（設定の `lease_priority`、小さいほど優先）で決め、同点は `instance_id` の
  辞書順。優先度の高い台が復帰しても**自動では奪わない**（fail-back しない。運用者が保持者を手放させる）。
  フラッピングを避けるため
- ttl の既定は 10 s、renew 間隔は 3 s、取得の判定は「保持者の更新が ttl を超えて途切れた」
- 役割ごとに独立（`mqtt-publisher` と `sink-writer` が別の台でもよい）。ただし既定では**全役割を同じ台に
  寄せる**（優先度が同じなら同じ台が取る）。分散させると障害時の状態が読みにくい

### 5.2 実装候補

| 候補                                   | 正の置き場所                               | 分断への強さ                                                             | 前提                                 | 位置づけ                                    |
| -------------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------ | ------------------------------------ | ------------------------------------------- |
| **A. PLC 調停（ラダー判定）**          | PLC 内のワード。**PLC が保持者を決める**   | 強い。PLC に届かない台は自動的に失格。PLC が唯一の書き手なので競合が無い | PLC 側にラダー（小さい）を入れられる | **第一候補**                                |
| **B. PLC 調停（ワードのみ）**          | PLC 内のワード。Hub が読んで書く           | 強いが、同時取得の競合窓（1〜2 周期）がある                              | ラダー変更なし。ワードの予約だけ     | A が入れられない現場の既定                  |
| C. 相互ハートビート + 静的優先度       | 各インスタンス（HTTP で相互に見る）        | 弱い。2 台が互いに見えないと**両方が保持者**になりうる                   | 何も要らない                         | PLC も共有ストレージも無い開発 / 検証用のみ |
| D. 外部の単一点（DB 行、共有ファイル） | PostgreSQL の行ロック、共有フォルダの lock | その単一点の可用性に依存                                                 | Sink 用 DB や共有フォルダがある      | Sink の書き手だけに使う選択肢               |
| E. 過半数（Raft 系）                   | インスタンス群                             | 強い（N≥3）                                                              | 3 台以上、実装が重い                 | **採らない**（scada-design.md §13.2）       |

**PLC 調停を第一候補にする理由**: PLC が control authority である原則と整合する。PLC に届かない Hub は
そもそも読み取りも書き込みもできないので、「PLC に届く台の中で 1 台」を選ぶのが現場の意味に合う。
PLC 冗長系ではワードがトラッキングで引き継がれる。

### 5.3 PLC 調停のワード仕様（案）

設定で「調停 PLC」の接続と先頭デバイス（例 `D9000`）を指定する。1 論理サービスに 1 ブロック。

```text
D+0      arbiter_magic      固定値（ブロックが初期化済みか）
D+1      epoch              保持者が変わるたびに +1（A ではラダーが、B では新保持者が書く）
D+2      holder_slot        保持者のスロット番号（0 = 無し）
D+3      holder_ttl_ticks   保持者の更新猶予（PLC の 100 ms tick 単位、既定 100 = 10 s）
D+10..   slot[i] { instance_hash(2W), heartbeat_counter(1W), priority(1W) }   i = 1..8
```

- 各インスタンスは起動時に**スロット**を設定で固定して持つ（動的割当はしない。設定ミスは
  `instance_hash` の不一致で検出）
- ハートビート: 各台が自分の `heartbeat_counter` を renew 間隔で +1 する（PLC 書き込み 1 ワード）
- **A（ラダー判定）**: PLC が各スロットの counter の変化を監視し、`holder_ttl_ticks` の間変化が無ければ
  失格、生きているスロットのうち最小の priority を `holder_slot` に書き `epoch` を +1。Hub は
  `holder_slot` と `epoch` を読むだけ。書き手が PLC 1 つなので競合が無い
- **B（ワードのみ）**: 各台が読み取り周期で全スロットを読み、保持者の counter が ttl を超えて止まって
  いたら「取得を試みる」= `holder_slot` と `epoch+1` を書く → 1 周期待って読み直し、`holder_slot` が
  自分なら取得成功、違えば負け。2 台が同時に書いた窓では**最大 2 周期の間、両方が保持者だと思う**。
  この窓の副作用は at-least-once の範囲で許容する（MQTT は同じ値の二重 publish、Sink は重複行、
  Action は**実行前にもう一度 `observe` して自分でなければ中止**）
- 役割ごとにブロックを分けるか、1 ブロックで全役割を同じ台に寄せるかは 5.1 の「既定は同じ台」に従い、
  初版は **1 ブロック = 全役割**とする

### 5.4 調停 PLC が無い / 届かない場合

- 調停 PLC に届かない台は `renew` が失敗し、副作用を止める（fail-stop）。**全台が届かなければ副作用は
  全停止**。これは「PLC が止まっているのに外へ値を出し続ける」より安全側
- PLC の無い構成（postgres / virtual のみ）は候補 D か C。既定は D（Sink 用 DB があれば行ロック）

---

## 6. 層 3: SCADA server の N 台化

前提: scada-server core（scada-design.md §13.2）は Hub の client であり、値は banto-tagclient で受ける。
論理サービス `service_id` と `instance_id` は §2 と同じ。

### 6.1 仕分け

| 種別             | 対象                                                         | 方針                                                                            |
| ---------------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| 全台で同じ       | Project package（revision 付き）、記録対象の設定、Alarm 定義 | Project package の配布（scada-design.md §5）。`project_revision` の一致を状態に |
| 各台が独立に記録 | 履歴（tstore）、execution record、Alarm イベント             | **読み出し時に統合**（6.2、6.4）                                                |
| 全台に複製       | 操作者の Alarm 状態（ACK / Shelve / Unshelve / コメント）    | **イベントの複製**（6.3）                                                       |
| 1 台だけ         | 常時実行 Action の副作用                                     | リース `action-executor`（§5）                                                  |

### 6.2 recorder（Active/Active、読み出し時に統合）

- 各インスタンスが Hub（自分が接続している Hub インスタンス）から購読し、**自分の `data.dir`** に記録する。
  tstore にはプロセス間の排他が無いので、**`data.dir` をインスタンスで共有しない**（共有フォルダに置く場合も
  インスタンス別のサブフォルダ）
- 記録に `service_id` / `instance_id` / `project_revision` / Hub の `config_fingerprint` をファイルの
  メタ（`tstore_meta`）に添える
- 読み出し（tsquery）は**複数の `data.dir` の和集合**を取る。同じ group・同じ `ptime` の行が複数
  インスタンスにあれば、**リース `action-executor` の保持者（居なければ `lease_priority` 最小）の行を
  優先**する。欠落は他インスタンスの行で埋まる。行の中身が食い違う（各台の受信タイミング差）ことは
  許容する。tsquery の read API に `sources: [dir...]` を足す
- 時計: `ptime` は各台の時計。台間で数百 ms ずれると同じ ptime 行が無く「両方の行が残る」ので、読み出しは
  **decimation（間引き）で吸収**する。NTP 同期は運用要件（§10 #6）
- Replay（scada-design.md §13.3）の前提条件 1〜3 はここに乗る。Replay driver は統合済みの読み出しを使う

### 6.3 Alarm（評価は全台、操作者状態はイベントを複製）

- 評価は各台が自分の受信値で行う。**Alarm の同一性**を台間で揃えるため、Alarm インスタンスの id は
  `(alarm_definition_id, 発生時の Hub 値の t)` から決定的に作る（受信時刻ではなく Hub の `t`）。同じ値で
  発生した Alarm は台が違っても同じ id になる
- 操作者イベント（ACK / Shelve / Unshelve / コメント）は **append-only のイベントログ**として、受けた
  インスタンスが**全ピアへ転送**する（HTTP、best-effort、再送あり、`event_id` = UUID で冪等）。各台は
  受け取ったイベントを自分の Alarm 状態に適用する。競合（同じ Alarm に別の台で別の操作）は
  **イベントの時刻順で最後が勝つ**
- Alarm API はどのインスタンスも同じ内容を返す（イベント適用の遅延分だけずれる）。Alarm Viewer は
  接続中のインスタンスを見る
- 転送が失敗して届かなかったイベントは、ピアが復帰したときに**イベントログの差分同期**（最後に受け取った
  `event_id` 以降を取りに行く）で追いつく

### 6.4 常時実行 Action（リース対象）と execution record

- engine は Action の実行前に `renew` の成否（保持者か）を見る（scada-design.md §21 S10b の
  「自分が実行担当か」の口）。保持者でなければ**評価はするが実行しない**（評価結果は記録する）
- execution record（scada-design.md §16）に `instance_id` と `epoch` を添える。統合は「読み出し時に全台
  から集めて相関 ID で並べる」。同じ相関 ID が複数台にあれば 5.3 の窓で二重実行が起きた証拠として
  警告する

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
- `config_fingerprint` を catalog 取得時に読んで、**前の世代と違えば警告イベント**を出す（値は出す）。
  Hub 側の設定が台間でずれている検出は client でもできるようにする
- 書き込みは今のエンドポイントへ 1 回。切替中は失敗を返す（現状どおり）
- fail-back しない（つながっている限り戻らない）。優先順位は運用者がリストの順で表す
- 状態に `endpoint_index` と `endpoint` を出し、SCADA の状態画面は「どの Hub インスタンスを見ているか」を
  表示する

---

## 8. 障害シナリオ

前提: Hub 2 台（H1 優先、H2）、SCADA server 2 台（S1 優先、S2）、PLC 冗長系（A / B）、調停は PLC
（候補 A）。

| #   | 事象                                        | 期待する振る舞い                                                                                                                                                                                                                         |
| --- | ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | H1 のプロセス停止                           | H1 のハートビートが止まり ttl 後に H2 が全役割を取得。MQTT は H2 が接続し全件 republish、Sink は H2 側が INSERT 開始。client は Transport 失敗で H2 へ切替、名前で再バインド。切替の間（ttl + backoff）の値は欠落、Sink に重複行の可能性 |
| 2   | H1 の NIC 断（PLC には届くが LAN に出ない） | client は H2 へ。H1 は PLC には届くので調停上は保持者のまま → **MQTT / Sink は H1 が続ける**（外部 DB に届くかは別）。運用者が H1 の役割を手放させる。この事象は**調停が PLC 側にある弱点**として §10 #8 に残す                          |
| 3   | H1 と PLC の間の断（LAN は生きている）      | H1 の renew が失敗し副作用停止。H2 が取得。H1 の値は Bad、client は値が Bad でも接続は維持（H1 が PLC に届かないだけで Hub は生きている）→ **client 側の切替条件に「全接続が Bad」を足すか**は §10 #9                                    |
| 4   | PLC の系切替（A → B）                       | 各 Hub の接続層が endpoint を切替。切替中は Bad。調停ワードはトラッキングで引き継がれるので保持者は変わらない                                                                                                                            |
| 5   | H1 と H2 の相互断、両方 PLC には届く        | 調停は PLC なので保持者は 1 台のまま。client は自分が届く方へ。**分断しても副作用は 1 台**                                                                                                                                               |
| 6   | 全 Hub が PLC に届かない                    | 全役割が失効、副作用は全停止。client は値 Bad。安全側                                                                                                                                                                                    |
| 7   | S1 停止                                     | S2 が `action-executor` を取得。recorder は S2 が続けている（欠落なし）。Alarm の操作者状態は複製済み。画面は S2 へ切替                                                                                                                  |
| 8   | 設定変更（primary H1 で編集、H2 に未配布）  | fingerprint 不一致を両方の状態と client の警告で検出。値は出続ける。運用者が配布して解消                                                                                                                                                 |
| 9   | 全停止からの再起動                          | 起動順に依存しない。各台が起動 → 調停ワードを読む → 保持者が居なければ優先度順に取得。MQTT の `$state` は保持者が `online` にする                                                                                                        |
| 10  | 時計のずれ                                  | リースは各台の単調時計で数えるので影響なし。履歴の統合（6.2）と Alarm の時刻順（6.3）に影響 → NTP を運用要件に                                                                                                                           |

---

## 9. 段階導入

scada-design.md §22 #19 の「単一構成が動いてから」に従い、**単一構成の段階で入れておくもの（R0）**と
冗長化本体を分ける。

- **R0（単一構成に先に入れる。冗長化の有無に関わらず価値がある）**
  - `service_id` / `instance_id` / `config_fingerprint` を Hub の `/api/v1/status` に出す
  - 設定パッケージのサーバー側 export / import API（SCADA の Hub 連携でも使う）
  - SLMP の `io_id` / `network_id` / `pc_id` を registry から指定できるようにする（既知の制約の解消）
  - `plc_connections` の endpoint リスト化（長さ 1 で既存と同じ）
  - banto-tagclient の endpoint リスト化（長さ 1 で既存と同じ）と `config_fingerprint` の観測
  - MQTT `client_id` の既定を `banto-hub-{instance_id}` に
  - scada-server core の engine に「自分が実行担当か」の口（scada-design.md §21 S10b）
- **R1 リース**: `Lease` trait、候補 B（PLC ワードのみ）と候補 C（開発用）の実装、MQTT / Sink を
  リース配下に。候補 A のラダーは実機で試作
- **R2 Hub 2 台**: primary 固定の設定配布、API キーは (b)、Sink の `writer` フラグ、状態画面。実機で
  §8 の #1 / #3 / #5 / #9 を確認
- **R3 SCADA server 2 台**: recorder の複数 `data.dir` 統合、Alarm 操作者イベントの複製、execution
  record の統合、画面の切替
- **R4 N≥3 と PLC 冗長系**: スロット 3 以上の実機確認、MELSEC 冗長系の `io_id` = 0x03D0 の確認、
  読み取り中継モード（接続数 1 の機器）
- ChronoGazer 単体（記録計商品）は R3 の recorder 統合を crate 単位で共有するが、単体商品としての
  冗長化は対象外

---

## 10. 未決事項（オーナー判断待ち）

1. **MELSEC 冗長系の宛先指定**（3.2）: `io_id` = 0x03D0 を待機系ポートに投げたときの挙動と、応答エコーの
   扱い。実機確認が要る。手元の R08ENCPU（非冗長）では確認できない
2. **PLC 接続数と Hub 台数**（4.2）: インスタンス別ポートを第一候補としてよいか。接続数 1 の機器向けの
   読み取り中継モードを初版に含めるか（推奨: 含めない、R4）
3. **調停の候補 A（ラダー）を標準にするか**（5.2）: PLC 側に小さなラダーを入れる運用を現場に求められるか。
   求められないなら候補 B（ワードのみ、競合窓あり）が既定になる
4. **内部タグの複製**（4.6）: 初版は複製しない案でよいか
5. **API キーの配布**（4.4）: (b) インスタンスごとのキーを初版としてよいか
6. **時計同期を運用要件にする**（6.2、§8 #10）: NTP 必須と文書化してよいか
7. **Sink の一意制約**（4.8）: 冗長構成では `(ts, external_name)` を推奨に変える（external-db-design.md
   §5.4 の追記）
8. **PLC には届くが LAN に出ない Hub**（§8 #2）: 調停に「client から到達できること」を混ぜるか。混ぜると
   調停が PLC だけで閉じなくなる。推奨: 混ぜず、運用者の手動切替と監視で対応
9. **client の切替条件に「値が全部 Bad」を含めるか**（§8 #3）: 含めると PLC 停止時に client が Hub 間を
   往復する。推奨: 含めず、Bad は Bad として表示する
10. **config-primary をリース化する時期**: 初版は固定。運用で困ってから

---

## 11. 決定記録

| 日付       | 決定                                                                                                                   | 出所                              |
| ---------- | ---------------------------------------------------------------------------------------------------------------------- | --------------------------------- |
| 2026-09-30 | 3 層独立、読み取り・評価は全台、副作用は 1 台（リース）、PLC 調停第一候補、名前束縛が前提、SCADA server だけ先行しない | scada-design.md §13.2（オーナー） |
| 2026-09-30 | 本草案を作成。§3〜§9 は提案、§10 はオーナー判断待ち                                                                    | 本書                              |
