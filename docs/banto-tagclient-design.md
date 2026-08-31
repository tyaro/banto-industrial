# banto-tagclient 設計

作成日: 2026-08-29
状態: **S3b完了（S3b-1/S3b-2）**。S1bのREST catalog/values transport、S2aの
Hub WS wire純粋解析・bounded pending map・latest-wins publish gate・非LIVE current抑止に加え、
S2b-1の認証付きWebSocket handshake、S2b-2aのon_change subscribe送信と1フレーム受信、
S2b-2bのcrate-private単一世代worker・tokio watchによるlatest snapshot配信・atomic publishを
実装した。S3aでは公開Handle、worker所有権、明示shutdown、Drop時の非同期処理なしabortを
実装した。S3b-1ではcatalog起点の逐次再接続、指数backoff、停止割り込みを実装し、S3b-2ではconfig_changed再bind、revision/runtime metadataの再解決、coalesceを実装した（2026-08-31）。
設計時参照baseline: `b9552627a86015b354b3c5651184fb108ba89e44`
実API確認日: 2026-08-30（`apps/banto-hub/core/src/rest.rs` / `stream.rs`）

---

## 0. 目的と非スコープ

`banto-tagclient` は、アプリケーションが banto-hub のcatalogから安定IDでタグを
解決し、現在値を取得・購読するための Rust crate である。PLC、Modbus、SLMPへは
直接接続しない。タグ定義と品質判定の一次ソースは常に banto-hub とする。

初版の対象は次に限る。

- RESTによるcatalogと明示タグの初期snapshot取得
- WebSocketによる`on_change`購読と`config_changed`検知
- stable IDを用いたbindingの解決・再解決
- 最新値優先のアプリ内状態配信と停止可能な接続ライフサイクル

次は初版の非スコープである。

- `WriteValue`、PLC直接接続、タグ設定の変更
- MQTT、gRPC、履歴照会、認可方式の変更
- OS keyring、Tauri command、画面、案件固有のタグ名・ID・接続設定
- 実機値が失われた場合のdemo値への自動フォールバック

## 1. 位置づけと境界

```mermaid
flowchart LR
	A[private Tauri adapter\nkeyring / UI] -->|endpoint + API key| B[banto-tagclient]
	B -->|REST catalog / snapshot| C[banto-hub]
	B -->|WebSocket on_change / config_changed| C
	C --> D[PLC / field devices]
```

- `banto-tagclient` はTauri、Svelte、OS keyring、案件設定を知らない。
- private側adapterがkeyringからAPI keyを取得し、SDKへ渡す。SDKはkeyringを所有しない。
- API keyは `Authorization: Bearer <token>` ヘッダだけで送る。URL queryへ入れない。
- S1b RESTは自動system/environment proxyを無効化し、設定endpointへ直接接続する。redirectも追従しない。
- endpointは初版では`http`/`ws`のみとする（`https`/`wss`は別設計）。URLは
  userinfo、query、fragmentを必ず拒否し、HTTP redirectも追従しない。hostは空でない
  DNS名またはIP、portは省略または1..=65535、pathはorigin-formの絶対path（必要なら
  `/api/v1`等の固定prefix）だけを許可し、pathに資格情報を置かない。SDKは接続先の
  scheme/host/port/pathをこの境界内で固定してREST/WSを組み立てる。
- SDKから返す接続状態、値、未解決状態は、UIが現在値と誤認しないよう区別する。

## 2. transport選定

初版はREST + WebSocketを採用する。

| 用途                          | transport                  | 理由                                                                                                             |
| ----------------------------- | -------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| catalog取得、明示タグの初期値 | REST                       | 再接続時も含め、要求と応答を明確に対応できる。                                                                   |
| 値の継続受信                  | WebSocket `on_change`      | 最新値を低遅延で更新できる。                                                                                     |
| binding再解決の契機           | WebSocket `config_changed` | stable ID bindingのrevision変更をHub変更なしで即時検知できる。                                                   |
| gRPC                          | 初版不採用                 | Hub側`ValueBatch`にrevision / `config_changed`相当がない。catalog pollingかproto拡張の設計が固まるまで保留する。 |

RESTだけの定期pollingは、rename・rebind・削除を検知するまでの遅延を作る。WebSocketの
`config_changed`を利用し、構成変更時だけcatalogを取り直すことで、この遅延を持ち込まない。

## 3. データ契約

### 3.1 binding

アプリは案件固有の外部名ではなく、次のstable IDとアプリ内のbinding keyを保存する。

```rust
pub struct StableTagId {
	pub connection_id: i64,
	pub group_id: i64,
	pub tag_id: i64,
}

pub struct BindingRequest {
	pub binding_key: String,
	pub stable_id: StableTagId,
}
```

`external_name`はcatalogから都度解決する表示用・購読用情報であり、bindingの同一性には
用いない。public文書・fixtureに案件固有のtag名、tag ID、接続情報を入れない。

要求`BindingRequest`の`binding_key`重複、stable ID三つ組の重複、catalog内のstable ID
重複は全てfail-closedとする。部分的にbindingを作らず、安定分類
`duplicate_binding_key` / `duplicate_requested_stable_id` / `duplicate_catalog_stable_id`
（実装では`invalid_bindings`または`invalid_catalog`の下位理由としてもよい）で返す。

### 3.2 値と未解決

```rust
pub enum ValueQuality {
	Good,
	Stale,
	Bad,
}

pub struct TagValueSnapshot {
	pub binding_key: String,
	pub numeric_value: Option<f64>,
	pub quality: ValueQuality,
	pub source_timestamp_ms: Option<i64>,
	pub received_at_ms: i64,
	pub catalog_revision: u64,
	pub run_id: Option<u64>,
	pub collection_mode: CollectionMode,
	pub value_source: ValueSource,
	pub unresolved: Option<UnresolvedReason>,
}
```

`numeric_value: None`、品質、時刻、未解決理由を混同しない。`received_at_ms`はSDKが受信した
時刻、`source_timestamp_ms`はHubから来た値の時刻である。再bindingに失敗した既存値は
保持してよいが、`unresolved`を設定してcurrent値として扱わない。

REST catalog DTOはHubの`revision`, `run_id`, `collection_mode`と各tagの`value_source`を
保持する。REST values/snapshot DTOも応答の`revision`, `run_id`, `collection_mode`と
各値の`value_source`を保持する（Hub実装のwire名はsnake_case）。値側entryの
`value_source`をauthoritativeな値源情報として保持し、catalog側の表示情報で上書きしない。

```rust
pub enum CollectionMode { Configured, AllSimulation, Unknown(String) }
pub enum ValueSource {
	Real,
	Simulation,
	DerivedSimulation,
	Internal,
	Unknown(String),
}
```

`value_source`の既知値は`real` / `simulation` / `derived_simulation` / `internal`。
未知値はraw値を保持した`Unknown(String)`として扱い、`Real`や実機のcurrentへ昇格させない。
`collection_mode`も未知値を`Unknown(String)`として保持するが、Unknownまたはcatalogと
valuesで異なる場合はcurrentを公開しない。catalogとvaluesは`revision`だけでなく
`run_id`（Optionの有無と値）と`collection_mode`も同一generationで一致しなければならない。
WS `data`にはrevision/source/run metadataがないため、最後に確定したREST snapshotの
metadataと、値側entryのauthoritativeな`value_source`をその接続世代へ固定する。
切断・再bindingでは必ずこのsnapshotを再取得し、旧世代のWS値を再利用しない。

## 4. 公開APIとエラー契約

S1bで確定した公開APIは次のとおりである。REST clientは自動system/environment proxyを無効化し、
設定endpointへ直接接続する（redirectも追従しない）。

```rust
pub struct RestClient { /* Endpoint + opaque SecretApiKey + reqwest Client */ }

impl RestClient {
	pub fn new(endpoint: Endpoint, secret: SecretApiKey) -> Result<Self>;
	pub async fn fetch_catalog(&self) -> Result<CatalogSnapshot>;
	pub async fn fetch_values(&self, tags: &[&str]) -> Result<ValuesSnapshot>;
	pub fn start(self, requests: Vec<BindingRequest>) -> Result<TagClientHandle>;
}

pub struct TagClientHandle { /* private stop/state/task ownership */ }

pub struct TagClientState { /* state, optional current, optional last_error */ }

impl TagClientHandle {
	pub fn state(&self) -> TagClientState;
	pub fn state_watch(&self) -> tokio::sync::watch::Receiver<TagClientState>;
	pub async fn shutdown(self) -> Result<()>;
}
```

`fetch_values`は外部タグ名を単一の`tags=name1,name2` queryへエンコードし、空リストでも
`tags=`を送る。タグ名にカンマが含まれる場合は通信前に`invalid_tag_selection`でfail-closed
する。S3aの`start`は要求をspawn・通信前に検証し、現在のTokio runtime上で単一世代workerを所有する
`TagClientHandle`を返す。handleはclone不可で、`state`/`state_watch`だけを公開し、明示的な
`shutdown().await`で停止通知・WebSocket close・worker joinを行う。DropはblockせずStoppedを
通知してbest-effort abortする。worker失敗時は`TagClientState::last_error()`へ安定分類を残し、
shutdownはそのエラーを返す。公開Handleによるcatalog起点の再接続・retry、rebinding、retry拡張はS3bで扱う。

`SecretApiKey`は明示的なopaque wrapperとする。crate-private APIは
`SecretApiKey::new(String) -> Result<SecretApiKey, SecretError>`と、SDK内部だけが使う
`apply_authorization(request)`（Authorization bearer headerを設定する）であり、raw値を
返す`as_str`/`to_string`は提供しない。`Clone`, `Debug`, `Display`, `Serialize`,
`Deserialize`は実装せず、`TagClientConfig`やエラー、ログ、URLにも値を出さない。再利用が
必要なworker所有権は内部で限定的に管理し、呼出側へ複製可能なsecretを返さない。

エラーは文字列だけで判定しない。少なくとも次の安定分類を持たせる。

| 分類                            | 意味                                                             | 再試行                                                       |
| ------------------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------ |
| `unauthorized`                  | 401/403または資格情報の拒否                                      | 無限高速再試行しない。呼出側が資格情報を更新して再開始する。 |
| `transport`                     | 接続、timeout、切断                                              | bounded exponential backoffで再接続する。                    |
| `protocol_error`                | 不正JSON、期待外メッセージ、契約違反                             | 接続を閉じ、backoff後にcatalogから再開する。                 |
| `catalog_unavailable`           | catalog取得不能                                                  | backoff後にcatalogから再開する。                             |
| `binding_unresolved`            | stable IDがcatalogで解決不能                                     | 接続は継続可能。対象値をcurrent扱いしない。                  |
| `revision_mismatch`             | catalogとvalues/snapshotのrevision不一致                         | bounded retry。整合するまでcurrentを公開しない。             |
| `runtime_metadata_mismatch`     | revision/run_id/collection_mode不一致、または未知collection_mode | bounded retry。整合するまでcurrentを公開しない。             |
| `duplicate_binding_key`         | 要求`binding_key`重複                                            | fail-closed。部分bindingを公開しない。                       |
| `duplicate_requested_stable_id` | 要求stable ID重複                                                | fail-closed。部分bindingを公開しない。                       |
| `duplicate_catalog_stable_id`   | catalog内stable ID重複                                           | fail-closed。部分bindingを公開しない。                       |
| `invalid_endpoint`              | 禁止scheme/URL部品、redirect応答                                 | 再試行せず設定を修正する。                                   |
| `invalid_tag_selection`         | カンマを含む外部タグ名（Hubの単一queryで曖昧になるため拒否）     | 入力を修正する。                                             |
| `stopped`                       | 呼出側の停止または正常shutdown                                   | 再接続しない。                                               |

`Debug`/`Display`、エラー、ログにtokenを含めない。endpointについてもhost以外のpathや
資格情報を露出しない。secret wrapperは値を常にredactする。

## 5. 状態機械と再接続

```mermaid
stateDiagram-v2
	[*] --> stopped
	stopped --> connecting: start
	connecting --> live: catalog → resolve → WS subscribe → matching snapshot → publish gate
	connecting --> unauthorized: 401 / 403
	connecting --> reconnecting: transport / protocol error
	live --> reconnecting: disconnect
	live --> rebinding: config_changed
	rebinding --> rebinding: coalesce further config_changed
	rebinding --> live: catalog → resolve → WS subscribe → matching snapshot → publish gate
	rebinding --> reconnecting: bounded retry exhausted / transport error
	reconnecting --> connecting: backoff elapsed
	unauthorized --> stopped: shutdown / caller action
	connecting --> stopped: shutdown
	live --> stopped: shutdown
	rebinding --> stopped: shutdown
	reconnecting --> stopped: shutdown
```

接続開始・再接続・rebinding後の新世代確立は、同じrace-free publish gateを通る。catalogを
取得してstable IDをresolveした後、先にWS接続とsubscribeを成立させる。WSの初期dataと
以後のdataは、無制限queueではなくbinding/tagごとに最新1件だけを保持するbounded pending
mapへcoalesceし、この時点ではcurrentへ公開しない。その後REST snapshot
を取得し、catalogとvaluesの`revision`、`run_id`、`collection_mode`が全て一致し、
collection_modeが既知で、要求したgeneration中に`config_changed`も`unknown_tag`も受信
していないことを確認して初めてsnapshotとpending WS dataを一括公開する。ただし公開時は
各値のsource timestamp `t`を比較し、同値時は受信順sequenceを使う安定規則でsnapshotより
古いpending値を破棄する。snapshotより新しいpendingだけをsnapshotへ重ね、単一snapshot
としてatomicに公開する。gate確認中に通知を
受信した場合、または不一致ならWS世代を破棄し、catalog取得からbounded retryする。これに
より「snapshot取得後からWS成立まで」に通知を取り逃がして旧bindingを公開する経路を持たない。

```mermaid
sequenceDiagram
	participant A as App
	participant C as banto-tagclient
	participant H as banto-hub
	A->>C: start(bindings)
	C->>H: GET catalog
	H-->>C: catalog + revision + run_id + collection_mode + value_source
	C->>C: stable ID resolve
	C->>H: WS connect / subscribe(on_change)
	H-->>C: initial data (buffer; do not publish)
	C->>H: GET explicit tag snapshot
	H-->>C: values + revision + run_id + collection_mode + value_source
	C->>C: require matching revision + run metadata + known mode
	C->>C: require no config_changed/unknown_tag during generation
	C->>C: compare (t, receive_seq); discard old pending; overlay newer pending
	C->>C: atomic publish one snapshot (snapshot + newer pending only)
	H-->>C: config_changed
	C->>C: invalidate current; enter rebinding and coalesce
	C->>H: GET catalog (bounded retry, from catalog each attempt)
	C->>C: re-resolve / WS subscribe (data buffered)
	C->>H: GET snapshot
	C->>C: require matching metadata / publish gate
```

`config_changed`受信時は直ちに`rebinding`へ遷移し、旧値をcurrent扱いしない。rebinding中に
同じまたは別revisionの通知が連打されても、通知をキューへ無制限に積まず「再解決が必要」
という一つのpending状態へcoalesceする。1回の試行は必ずcatalog取得から始め、stable IDを
再解決し、値snapshotのrevision、run_id、collection_modeがcatalogと一致した場合だけ新しい
購読・currentを公開する。publish gate中のWS値はbinding/tagごとの最新1件だけを保持し、
各値の`(source timestamp t, receive sequence)`を比較する。REST snapshotより古い、または
同値で安定規則上古いpendingは破棄し、新しいpendingだけをsnapshotへ重ねて一つのsnapshot
としてatomicに公開する。これによりWS初期dataがREST snapshotより古い場合に値を巻き戻さない。
`value_source`はvalues側entryをauthoritativeとして採用する。
不一致ならbounded retry（回数・待ち時間上限あり）し、上限到達時は`revision_mismatch`または
`runtime_metadata_mismatch`
または`reconnecting`としてcurrentを公開しない。対象が消えた場合は
`binding_unresolved`として値を保持してもcurrentにはしない。切断後もWebSocketだけを
張り直さず、必ずcatalogとsnapshotからやり直す。

## 6. バックプレッシャと所有権

- 値は最新値優先とする。内部状態はwatch相当のbounded latest snapshotで配る。
- publish gate中のWS dataは無制限queueにせず、binding/tagをキーとするbounded pending mapに
  最新1件だけを保持する。各entryはsource timestamp `t`と単調な受信sequenceを持ち、REST
  snapshotより古いpendingは公開前に破棄する。
- 遅いconsumerのための無制限queueは作らない。古い中間値は破棄可能である。
- `TagClientHandle`がworker task、socket、channelの所有者である。
- `shutdown().await`は停止通知、socket close、task joinを行う。明示shutdownを推奨し、
  Dropは同期的なStopped通知、停止通知、best-effort abortだけを行いblockしない。
- `shutdown`後およびDrop後に再接続taskやsocketが残らないことをテストする。S3bで再接続とrebindingを扱う。

## 7. 依存と実装ゲート

S1aでレビュー済みの依存は、workspaceの`serde`/`serde_json`、`reqwest 0.13.4`
（`default-features = false`、URL型のみのため`json` featureなし）、
`zeroize 1.9.0`（derive featureなし）である。Cargo.lockにはこれらの既存package系列を
再利用し、S1a追加による新規package系列は増えていない。REST clientは手書きHTTP parserを避け、
S1bでreqwestを使う。手書きparserは
HTTP framing、redirect、timeout、header処理、認証の誤実装リスクを増やすため採用しない。

crate追加前に、次をレビューゲートとする。

1. `Cargo.lock`増分と依存グラフ
2. 各依存のlicenseと保守状況
3. Windows配布バイナリの増分
4. 既存workspace featureとの整合
5. Hubのrelease tagまたは互換commitへの固定方法

S2b-1では`tokio-tungstenite 0.30`（MIT）と`tungstenite 0.30`（MIT OR Apache-2.0）を
承認済みworkspace依存として利用する。TLS featureは有効化せず、Cargo.lockは既存系列のみを
再利用する。WebSocket transportのbinary size実測はS4で行う。Issue #199の既存version/comment
不整合はS4統合ゲートで追跡する。設計時点でtarget Hubのrelease tagは未定である。参照baselineは
本書冒頭のcommitとし、
リリース互換commit/tagはSDK実装完了時に決める。

S2b-2aでは`futures-util 0.3`をcrateの直接workspace依存として追加した。`SinkExt`/`StreamExt`
によるboundedな1フレーム送受信とPing時flushに使用し、licenseはMIT OR Apache-2.0、既存の
workspace/lock系列を再利用してpackage/version blockは追加していない。追加featureは`sink`のみである。

S2b-2bでは既存workspaceの`tokio`に`sync`と`macros` featureを有効化し、`watch`によるboundedな
latest state配信と`select!`によるWS優先処理に使用した。S3aでは同じ既存workspace依存の`rt`
featureを有効化し、公開Handleだけがworkerをspawnする。新規package/version blockは追加していない。

## 8. テスト計画

| テスト                       | 確認する契約                                                                                      |
| ---------------------------- | ------------------------------------------------------------------------------------------------- |
| catalog resolve              | stable IDから外部名・購読対象を解決できる。                                                       |
| unknown stable ID            | `binding_unresolved`になり、値をcurrent扱いしない。                                               |
| initial snapshot             | REST snapshotとWS初期dataでlatestが更新される。                                                   |
| race-free publish gate       | WSを先に成立させ、snapshot後の通知取り逃しを防ぎ、gate確認前のdataを公開しない。                  |
| handshake buffer latest-wins | pending mapはbinding/tagごとに最新1件だけを保持し、snapshotより古いWS frameは上書きせず破棄する。 |
| config_changed rename/rebind | rebinding中は旧値を無効化し、通知連打をcoalesceして一度だけ再解決する。                           |
| revision一致ゲート           | catalogとvalues/snapshotのrevision不一致を公開せず、catalogからbounded retryする。                |
| runtime metadata一致         | revision/run_id/collection_modeを同一generationとして検証し、未知mode・不一致をfail-safeにする。  |
| value_source未知値           | `Unknown(raw)`で保持し、実機値/`Real`と誤認しない。                                               |
| duplicate拒否                | binding_key、要求stable ID、catalog stable IDの重複をfail-closedにする。                          |
| endpoint boundary            | userinfo/query/fragment、禁止scheme、redirectを拒否する。                                         |
| disconnect/reconnect         | bounded backoff後、catalog/snapshotから接続を再構築する。                                         |
| 401/403                      | `unauthorized`になり、高速無限再試行しない。                                                      |
| malformed JSON               | `protocol_error`に分類し、secretを出さず回復経路へ入る。                                          |
| invalid tag selection        | カンマを含む外部タグ名を通信前に`invalid_tag_selection`として拒否する。                           |
| backpressure/latest-wins     | 遅いconsumerでもqueueが無制限に増えず、最新値を観測できる。                                       |
| shutdown                     | task、socket、channelが残留しない。                                                               |
| secret redaction             | Debug/Display/エラー/ログへtokenやendpoint pathを出さない。                                       |

## 9. 実装sliceと完了条件

| slice  | 内容                                                           | 完了条件                                                                                                                                                                    |
| ------ | -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| S1a    | crate骨格、共通DTO、SecretApiKey、Endpoint、stable ID resolver | URL境界・redaction・metadata保持・unknown値保持・重複fail-closed・catalog resolveテストが通る。REST送信、Authorization、redirect処理は含めない。                            |
| S1b    | REST catalog/values transport、Authorization、redirect拒否     | reqwestによる読み取り専用GET、認証ヘッダ、redirectを追従しない設定、HTTPエラー分類のテストが通る。                                                                          |
| S2a    | WS wire解析、publish gate、latest snapshot、状態DTO            | malformed/unknown/id・tag拒否、bounded pending、handshake latest-wins、RESTとのtimestamp/sequence統合、metadata一致、非LIVE current抑止の純粋coreテストが通る。             |
| S2b-1  | 認証付きWebSocket handshake transport                          | prefix保持URL、Authorization、redirect非追従、HTTP/timeout分類、1MiB制限、秘密redactionのテストが通る。                                                                     |
| S2b-2a | on_change subscribe送信、1フレーム受信                         | 厳密なsubscribe JSON、共通tag validation、native Ping/Pong、Text受信、Binary/Close/EOF/容量分類のテストが通る。                                                             |
| S2b-2b | 単一世代worker、watch latest snapshot配信、atomic publish      | catalog→WS subscribe→初回data→REST gateの順序、完全snapshotのatomic publish、live更新のlatest-wins、失敗時current消去のテストが通る。                                       |
| S2     | WS購読、latest snapshot、状態機械                              | S2a/S2b-1/S2b-2a/S2b-2bの完了条件を満たし、WS先行接続から単一世代のatomic publishまでの全テストが通る。公開Handle、再接続、rebinding、shutdownはS3a/S3bで扱う。             |
| S3a    | 公開Handle、worker所有権、明示shutdown、Drop abort             | runtime外startのfail-closed、state/state_watch、Live後のgraceful close/join、in-flight stop、失敗error保持、Drop後のcurrent消去テストが通る。                               |
| S3b-1  | catalog起点の再接続、backoff、停止割り込み                     | Transport/ProtocolError/CatalogUnavailableを逐次retryし、Unauthorized等をterminal扱いにする。Live後のbackoff reset、Reconnecting中のcurrent消去、停止割り込みテストが通る。 |
| S3b-2  | config_changed、rebinding/coalesce、再解決、retry拡張          | revision/runtime metadata不一致の再解決、coalesced rebind、旧値無効化、停止可能なrebind retryを実装し、テストが通る。                                                       |
| S4     | workspace統合と互換性固定                                      | 公開restart/credential更新、S4互換tag固定、実Hub/LAN統合検証、依存レビューと最終互換性確認を完了する。                                                                      |

初版のDefinition of Doneは、全テスト表を自動化し、書き込み・PLC直結が存在せず、
demoへの自動fallbackがなく、認証情報が全観測可能面からredactされ、停止後にworkerが
残らないこととする。Hub本体・既存app/crate実装は変更せず、manifest/lockfileは承認済み追加のみとし、
新規依存は別レビューで承認されるまで追加しない。
