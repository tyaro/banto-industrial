//! 収集サービス（#383 段階2b / R1-C）: `banto-collect` の
//! [`Collector`] をこのアプリの設定 DB・データディレクトリに配線する
//! サービス層。
//!
//! chronogazer はこれまで収集エンジンを一切起動していなかった（`banto-collect`
//! は R1-A で依存に足しただけで使用箇所 0）。ここはその**最初の配線**にあたる。
//!
//! # ここまでの範囲（C-1 → C-2 → C-3a）
//!
//! C-1（#406）はサービス層の骨格（`start`/`stop`/`restart`/`state`/読み出し）
//! までで、**誰も [`CollectorService::start`] を呼んでいなかった**。
//!
//! C-2（#407）で足したのは**操作の口とその前提**だけ:
//!
//! * 起動時の自動開始（[`CollectorService::autostart`]）、
//! * ライフサイクル操作の**待ち時間の上限**（[`COLLECT_OPERATION_TIMEOUT`]）と、
//!   打ち切りを表す [`CollectOutcome::pending`]、
//! * そのために要る [`CollectorState::Starting`]。
//!
//! C-3a（この変更）で足したのは**読み出しの口**だけ:
//!
//! * 現在値（[`CollectorService::values`]）、接続状態
//!   （[`CollectorService::connections`]）、収集イベント一覧
//!   （[`CollectorService::events`]）の 3 つ、
//! * その 3 つが共有する**結末の型** [`Readout`] -「走っていない」「読めなかった」
//!   「読めて 0 件」を**別の値**にするためのもの、
//! * 現在値を**キューから外し**（[`CurrentValuesHandle`] を葉に公開）、
//!   キューを通る読み出しに**短い上限**（[`COLLECT_READ_TIMEOUT`]）を掛けた。
//!
//! 残りは変わらず別枠:
//!
//! * 画面（収集の状態表示・現在値表示・イベント一覧ページ）は **C-3b**、
//! * シミュレータハーネスと E2E は **C-4**（`tests/collect_roundtrip.rs` と
//!   `e2e/tests/user-simulator-roundtrip.spec.ts`）、
//! * Hub 経由で受けている値の保存・合流は **段階3**（`crate::hub` は触らない）。
//!
//! # 扱う対象（段階2b）
//!
//! **SLMP / Modbus TCP の直結のみ**。Hub 接続は設定 KV（`hub.record`）のままの
//! 別建てで、`plc_connections` には何も足していない（マイグレーション無し）。
//!
//! # 状態の表現 — 「収集対象 0 件」は異常ではない
//!
//! [`banto_collect::Collector::start`] は接続 0 件の構成を
//! [`CollectError::Config`] で拒否する。エンジンにとっては正しい（開く
//! スキーマが無い）が、**利用者にとってはエラーではなく「まだ何も登録して
//! いない」**。そこでこのサービスは `Collector::start` を呼ぶ**前に**
//! [`banto_collect::CollectorConfig::tag_count`] を見て、0 件なら
//! [`CollectorState::NoTargets`] という**専用の状態**を返す - #332 / #385 で
//! 確立した「**空とエラーを別の状態にする**」規律
//! （docs/implementation-checklist.md §5 の 1 行目）をここでも守る。
//!
//! **数えるのはタグ**であって、グループでも接続でもない。利用者にとっての
//! 「収集対象」は**タグ 1 本 1 本**で、グループと接続はその入れ物にすぎない
//! （入れ物だけ作って中身がまだ無い、は設定作業の途中として普通に起こる）。
//! [`banto_collect::build_config`] は**タグが 1 本も無い有効グループも計画に
//! 残す**ので、`group_count()` で数えると「有効な接続 1 件・有効なグループ
//! 1 件・**有効タグ 0 件**」という構成が素通りし、読む物が何も無いのに PLC へ
//! 繋ぎに行って tstore まで開いてしまう（#406 レビュー P2）。
//! `tag_count() == 0` は**グループ 0 件・接続 0 件の場合も必ず含む**（タグは
//! グループの中にしか居ない）ので、判定はこの 1 本で足りる - 「起こす条件」を
//! 2 本に割らない（docs/implementation-checklist.md §5）。
//!
//! 状態は 5 つだけ（[`CollectorState`]）: 停止中 / **起動中** / 動作中 /
//! 収集対象なし / 起動失敗（理由付き）。起動失敗の `reason` は
//! [`CollectError`] の文言をそのまま載せる（分類して捨てない）。
//!
//! # 内部の状態と、状態取得で公開する形を分ける
//!
//! [`CollectorState::StartFailed`] の `reason` は [`CollectError`] の文言
//! そのままなので、**接続先のホストや `data.dir` の絶対パスを含みうる**。
//! 一方で状態の**読み取り**は `viewer` にも開いている
//! （[`COLLECT_READ_ROLE`]）。その判断の根拠は「[`CollectorState`] には機微な
//! 情報が入っていない」ことだったのに、C-2 の実装は**内部の型をそのまま
//! ワイヤへ載せていた**（#407 レビュー P2-2）。
//!
//! そこで型を 2 つに分ける:
//!
//! * [`CollectorState`] は**内部・診断用**。`reason` を**持ったまま**変えない
//!   （分類して捨てない、という上の規律はそのまま）。
//! * [`CollectorStateView`] が**公開用**。`startFailed` という**状態そのものは
//!   返すが、理由は返さない**。
//!
//! **変換は [`CollectorStateView::from`] の 1 箇所だけ**で、REST
//! （`GET /api/collect`）も Tauri（`collect_status`）も
//! [`CollectorService::state_view`] を通る - [`COLLECT_AUDIT_RESOURCE`] や
//! 床の定数と同じ考え方で、**経路による食い違いを申し合わせではなく構造で
//! 防ぐ**。
//!
//! ## 理由はどこで利用者に伝わるか（公開から落とした分の埋め合わせ）
//!
//! **起動を実行した本人には、操作の戻り値が理由を伝える**。
//! [`CollectorService::start`]（`collect_start` / `POST /api/collect/start`）は
//! 起動に失敗したとき `Err` を返し、その中身は
//! [`CollectorState::StartFailed`] に焼き付けたのと**同じ文言**
//! （[`Lifecycle::fail_start`]）。押した人はその場でエラーとして理由を読める。
//!
//! **残る制約（黙って落とさないために明記する）**: 画面を再読み込みすると、
//! 状態取得は `startFailed` としか答えないので**理由は分からなくなる**。
//! 知りたければ「収集を開始」をもう一度押す（同じ構成ならまた同じ理由が
//! エラーとして返る）。**診断用の取得口（`admin` 限定で理由まで返す口）は
//! C-2 では作らない** - 作るなら床・監査・画面の置き場まで含めて別途決める。
//!
//! なお [`CollectOutcome::status`] は**内部の [`CollectorState`] のまま**で、
//! ここは意図的にそろえていない: [`CollectOutcome`] を受け取れるのは
//! `editor` 以上（[`COLLECT_OPERATION_ROLE`]）＝**操作を実行した本人**だけで、
//! その人には上のとおり理由が伝わってよい。公開用の型に落としているのは
//! **`viewer` にも開いている状態取得の経路**である。
//!
//! # 下位の失敗で上位の状態を書き換えない
//!
//! [`CollectorState`] が変わるのは **[`CollectorService`] 自身の
//! start/stop/restart のときだけ**。走り出した後の PLC 断・読み取り失敗・
//! append 失敗は `banto-collect` 側が `Bad` 品質フラグと `collect_events` の
//! イベントに変換して**回り続ける**設計なので（`banto-collect` の
//! `error.rs` / `task.rs` のモジュール doc）、それでこのサービスの状態を
//! 「停止」や「起動失敗」に落とすことはしない（#385 の規律 =
//! docs/implementation-checklist.md §5「状態を別軸に保つ」）。
//!
//! # ライフサイクルは専用タスクが所有する（ロックで守らない）
//!
//! [`Collector`] の**所有権そのもの**と、それに対応する状態遷移は
//! [`CollectorService`] ではなく**1 本の tokio タスク**（[`Lifecycle`]）が
//! 持つ。`start`/`stop`/`restart` と読み出しは、**コマンド（mpsc）+ 応答
//! （oneshot）**でそのタスクに依頼するだけで、呼び出し側は
//! [`Collector`] に指一本触れない。
//!
//! **なぜロックではなくタスクなのか（キャンセル安全性。#406 レビュー P2）**:
//! 以前はサービス側が `AsyncMutex<Option<Collector>>` を `take()` してから
//! `Collector::stop().await` していた。axum のハンドラは接続が切れれば
//! future を drop するので、**`take()` の後・`stop()` の完了前に呼び出し側が
//! 消える**ことが現実に起こる。そうなると `collector` は `None`・状態は
//! `Running` という食い違いが**そのまま残り**、しかも次の `stop()` は
//! 「走っていない」分岐に落ちて状態を直せない。さらに `Collector::stop()` は
//! **接続タスクの join と writer の最終 flush** なので、状態表示だけ直しても
//! 実体の停止は保証できない。ライフサイクル処理をタスク側に置けば、
//! **oneshot の受信側が落ちても送信が失敗するだけで、開始済みの処理は
//! 最後まで進む**（#400 で潰した「飛行中の操作」と同じ層の、キャンセル側）。
//!
//! この形から**ただで**出てくる保証:
//!
//! * タスクはコマンドを**逐次**処理するので、**新しい `start()` は前の
//!   `stop()` が完了するまで進まない**。二重起動の防止も「呼ばれた順に
//!   効く」も、ロックではなく**所有権と 1 本のキュー**が担保する。
//! * 接続状態の読み出し（[`CollectorService::connection_status`]）も**同じ
//!   タスク**へ依頼する（**キューは別**。下の「読み取りと操作でキューを
//!   分ける」）ので、**ライフサイクル操作の途中の [`Collector`] を覗くことが
//!   ない** - 返ってくるのは必ず「どれかの操作と操作の間」の姿で、状態と
//!   食い違わない。代償として、**起動処理の最中は読み出しがその完了まで
//!   待つ**ので、画面へ出す口（[`CollectorService::connections`]）には
//!   **短い上限** [`COLLECT_READ_TIMEOUT`] を掛けてある（下の「読み出しの
//!   3 つの口」）。
//! * コマンドを**送る前**に呼び出し側が消えた場合は、そもそも何も起きない
//!   （キューに積まれていないので、タスクは知らないまま）。
//!
//! **現在値だけはこのキューを通らない**（C-3a）。理由と代償は下の
//! 「現在値はキューから外して葉に公開する」。
//!
//! 残るロックは**公開ロック（葉）** [`CollectorContext::published`]
//! （`std::sync::Mutex`）**1 本だけ**で、**書くのはライフサイクルタスクだけ**。
//! `.await` をまたいで保持しないし、ここから他のロックを取らない。
//! ポーリング経路（[`CollectorService::state`] / [`CollectorService::values`]）は
//! **キューを通らない**ので、起動中（tstore を開いている最中）でも待たされない。
//!
//! **状態と現在値ハンドルを 1 本のロックに同居させている**のは、ロック順序の
//! 問題を**作らない**ため（docs/implementation-checklist.md §5「ロック順序を
//! 一方向に固定」）: 2 本に分けると「状態 → 現在値」「現在値 → 状態」の順序を
//! 決めて守らせる話になるが、1 本なら**そもそも 2 つ取る場面が無い**。
//! おまけに、書き込み口を [`CollectorContext::publish`] 1 本に絞れるので
//! **「状態は `Running` なのに現在値が取り下げられている」という食い違いが
//! 構造上作れない**（下の「現在値はキューから外して葉に公開する」）。
//!
//! # 起動時の自動開始と、「収集を再起動」だけが反映の口であること
//!
//! アプリ（`src-tauri` の `setup()`）と LAN サーバー単体（`banto-serve` の
//! `main()`）は、どちらも起動時に [`CollectorService::autostart`] を spawn
//! する（docs/r1-plan.md の R1-C「起動時に build_config → start」）。
//! **失敗しても起動は止めない** - `crate::hub` の `resume()` と同じ流儀で、
//! 理由は [`CollectorState::StartFailed`] に残るので画面から見える。
//! **収集対象 0 件は失敗ではない**（[`CollectorState::NoTargets`]）。
//!
//! **レジストリ（接続・グループ・タグ）の変更では自動再起動しない。** 反映は
//! **明示的な「収集を再起動」操作**（[`CollectorService::restart`]、
//! `collect_restart`）だけで行う。docs/r1-plan.md の R1-C がそう定めている
//! のに加え、記録計としてはこちらが正しい: レジストリ CRUD のたびに
//! `restart()` を掛けると、**設定を 1 行直すたびに収集が途切れる**。タグの
//! 名前を 1 つ直す、しきい値を 1 つ足す、といった編集の最中に何度も
//! 収集が止まって立ち上がり直し、そのたびに tstore がローテーションして
//! ファイルが刻まれる。編集が一段落したところで利用者が 1 回押す方が、
//! 欠測も断片化も少ない。
//!
//! # 読み出しの 3 つの口 -「走っていない」「読めなかった」「0 件」を別にする
//!
//! C-3a の口は 3 つ: 現在値（[`CollectorService::values`]）、接続状態
//! （[`CollectorService::connections`]）、収集イベント一覧
//! （[`CollectorService::events`]）。**3 つとも [`Readout`] を返す**。
//!
//! [`Readout`] は「読めました」と「読めませんでした」を**同じ入れ物の別の値**に
//! する型で、これが C-3a の一番の勘所（docs/implementation-checklist.md §5
//! 「エラーを空に潰さない」/ #332 の 6 状態 / #400 の言い分け）:
//!
//! | 結末 | 意味 | 画面が言うべきこと |
//! | --- | --- | --- |
//! | [`Readout::NotRunning`] | 収集が走っていない（停止中 / `NoTargets` / 起動失敗） | 「収集は動いていません」 |
//! | [`Readout::Unavailable`] | 走ってはいるが**この 1 回が読めなかった**（[`COLLECT_READ_TIMEOUT`] で打ち切り / DB が読めない） | 「今は読めませんでした」（失敗とも 0 件とも言わない） |
//! | [`Readout::Ready`] | **読めた**。中身が空でも「0 件でした」という**事実** | 「0 件」 |
//!
//! 空の `HashMap` を返して「走っていない」を「0 件」に潰さない、というのは
//! C-1 から [`Option`] でやってきたこと（[`CollectorService::current_values`]）
//! の延長で、そこに**「読めなかった」という 3 つ目**を足したのが [`Readout`]。
//!
//! ## それぞれの口がどの結末を返しうるか（書いた分岐が到達すること）
//!
//! docs/implementation-checklist.md §5「書いた分岐が実際に到達するか確かめる」
//! に従って、**どの経路でどれが返るか**を明示しておく:
//!
//! * [`CollectorService::values`][] は `NotRunning`（葉に現在値ハンドルが
//!   無い）と `Ready`（0 件を含む）。**`Unavailable` は返らない** - 葉から
//!   読むので待つ相手がおらず、打ち切りようがない（それがキューから外した
//!   目的）。
//! * [`CollectorService::connections`][] は 3 つとも返る。`Unavailable` は
//!   「ライフサイクルタスクが遅い `start`/`stop` を処理している最中に
//!   [`COLLECT_READ_TIMEOUT`] が過ぎた」場合。
//! * [`CollectorService::events`][] は `Unavailable`（DB が読めない / 打ち
//!   切り）と `Ready`（0 件を含む）。**`NotRunning` は返らない** - 次項。
//!
//! ## イベント一覧だけ「走っていない」を返さない（意図的）
//!
//! `collect_events` は**過去の記録**であって、走っている収集エンジンの
//! 覗き窓ではない。収集が止まっていても「なぜ止まったか」を調べるために
//! 読むものなので、**停止中でも読めなければ意味が無い**
//! （docs/r1-plan.md の R1-C が「`collect_events` のイベント一覧ページ
//! （banto 監査ログページの流儀）」と書いているとおり、`crate::audit` の
//! 一覧と同じ性質の口）。ここで「走っていないので返しません」と答えるのは、
//! **読めた過去を隠す**ことになる。
//!
//! したがって [`CollectorService::events`] は収集の状態を一切見ない。
//! 走っているかどうかを知りたい画面は [`CollectorService::state_view`] を
//! 併せて読むこと（状態はそちらが唯一の口、という C-2 からの線引きのまま）。
//!
//! ## 上限を [`COLLECT_OPERATION_TIMEOUT`] と別に持つ理由
//!
//! 読み出しは**ポーリングで引かれる口**なので、ライフサイクル操作の 30 秒は
//! 長すぎる（30 秒返らない口を 1 秒ごとに叩けば、飛行中の要求が積み上がる
//! だけ）。値と根拠は [`COLLECT_READ_TIMEOUT`] の doc。
//!
//! # 現在値はキューから外して葉に公開する（C-3a）
//!
//! 現在値は**この後ポーリングで引かれる**（R1-D の監視画面が 1 秒前後で
//! 回す）。C-2 までのようにライフサイクルタスクのコマンドキューを通すと、
//! **遅い `start()` の後ろで待たされる** - #400 で潰した「画面が固まる」型が
//! そのまま再発する。
//!
//! そこで [`CurrentValuesHandle`] は**葉に公開する**:
//!
//! * [`Collector::current_values`] は `Arc<RwLock<..>>` を包んだハンドルを
//!   `clone` して返すだけなので、**タスクの外に持ち出して保持してよい**
//!   （`banto_collect::current` のモジュール doc - 収集タスクが書き、表示層が
//!   読む前提の共有ハンドル）。
//! * ライフサイクルタスクは **start が成功したときに公開し、stop が完了した
//!   ときに取り下げる**。公開・取り下げは**状態と同じ 1 本のロック**
//!   （[`CollectorContext::publish`]）で行うので、
//!   **「`Running` なのにハンドルが無い」「`Stopped` なのにハンドルが残って
//!   いる」が構造上作れない**。
//! * 読む側（[`CollectorService::current_values`] /
//!   [`CollectorService::values`]）は**同期**で、キューもディスクも触らない。
//!
//! **接続状態は外に出せない**ので、こちらはキュー経由のまま:
//! [`Collector::status`] は `&Collector` を要求し、その中の `StatusMap` は
//! `banto-collect` 内で `pub(crate)`。`crates/` は変更しない方針なので、
//! ハンドルだけ葉へ持ち出すことができない。代わりに
//! [`COLLECT_READ_TIMEOUT`] を掛けて、**待たされ続ける代わりに「読めません
//! でした」と答える**（[`Readout::Unavailable`]）。
//!
//! # 読み取りと操作でキューを分ける（#408 レビュー P2-1）
//!
//! 上の [`COLLECT_READ_TIMEOUT`] は、**呼び出し側が待つのをやめる**だけの
//! ものだった。**投入済みの要求はキューに残り続ける**ので、C-3a の実装
//! （接続状態の読み取りを操作と同じキューへ送る）には次の経路があった:
//!
//! 1. 応答しない共有への `open` などで `start()` が長時間止まる、
//! 2. その間、画面が接続状態を 1 秒ごとにポーリングする。呼び出し側は
//!    2 秒で [`Readout::Unavailable`] を受け取って次の周回へ進むが、
//!    **要求はキューに積み上がる**、
//! 3. [`COMMAND_QUEUE_DEPTH`] 件が**既に呼び出し元へ答えを返した読み取り**で
//!    埋まる、
//! 4. その後の `stop()` / `restart()` が [`COLLECT_ENQUEUE_TIMEOUT`] に
//!    掛かり、「**混雑により未受付**」になる。
//!
//! つまり **`viewer` でもできる閲覧のポーリングが、`editor` の停止要求を
//! 受け付けさせなくする**。読み取りの**待ち時間**に上限を掛けても
//! **キュー内の要求数は減らない**ので、上限では直らない。
//!
//! そこで**チャネルを 2 本に分ける**:
//!
//! * 操作（[`Command`]）: [`COMMAND_QUEUE_DEPTH`] 件・`send_timeout`
//!   （[`COLLECT_ENQUEUE_TIMEOUT`]）・入らなければ未受付のエラー。**C-2 の
//!   ままで、何も変えていない。**
//! * 読み取り（[`ReadCommand`]）: [`READ_QUEUE_DEPTH`] 件と**小さく固定**し、
//!   **`try_send`**。枠が無ければ**待たずに即** [`Readout::Unavailable`]
//!   （「読めなかった」は [`Readout`] に既にある正しい表現。**新しい状態は
//!   増やさない**）。
//!
//! **枠が空くのは、ライフサイクルタスクがその要求を取り出したときだけ。**
//! 呼び出し側が [`COLLECT_READ_TIMEOUT`] で待つのをやめても枠は返さない -
//! 返す実装（打ち切りで permit を解放する等）にすると、**タスクが止まって
//! いる間に未処理の要求がいくらでも積み上がる**ので、分けた意味が無くなる
//! （オーナー指摘）。タスクが止まっている間に何件溜めても答えは返らないの
//! だから、**溜めないことが正しい**。
//!
//! ライフサイクルタスクは `tokio::select!` で 2 本を受け、`biased` で
//! **操作を優先**する。読み取りが後回しになっても「今は読めませんでした」で
//! 正しく畳めるが、操作にはその逃げ道が無い（停止は実行されなければ
//! ならない）。
//!
//! # タスクが居ないことを「走っていない」と答えない（#408 レビュー）
//!
//! [`Readout::NotRunning`] を返してよいのは「**状態を見て、走っていないと
//! 分かった**」ときだけ - [`Lifecycle`] タスクが実際に `collector` を見て
//! 「持っていない」と答えた場合である。**タスクが居なくなっていた**
//! （チャネルが閉じている / 応答の送信側が drop された = タスクが panic した）
//! ときは、C-3a では `None` に潰して `NotRunning` と答えていたが、これは
//! **二重に嘘になりうる**（オーナー判断）:
//!
//! 1. **[`CollectorService::state`] と食い違う。** 状態は**葉**
//!    （[`CollectorContext::published`]）から読むので、タスクが panic しても
//!    **最後に公開された状態（たとえば `running`）を返し続ける**。同じ画面に
//!    「状態: 動作中」と「接続状態: 走っていません」が並ぶことになる。
//! 2. **実際に収集が続いている公算が大きい。** `banto-collect` の
//!    [`Collector`] には **`Drop` 実装が無い**（`crates/banto-collect` 唯一の
//!    `impl Drop` はテスト用の `TempDir`）。タスクが panic して [`Collector`] が
//!    drop されても、`Collector::start` が spawn した**接続タスクは止まらない** -
//!    保持しているのは `JoinHandle`（drop は detach であって abort ではない）
//!    と停止合図の `watch::Sender<bool>` で、後者が drop されても値は `false`
//!    のままなので、受信側の停止条件（`*stop_rx.borrow()` が `true`）は成立
//!    しない。つまり「誰も収集していない」は事実とは限らない。
//!
//! **[`Readout::Unavailable`]（走ってはいるが、この 1 回が読めなかった）が
//! この状況を正しく言い表している。** 判定材料が無いときに「止まっている」側へ
//! 倒さない、というのは #387 の `KeyOutcome::Undetermined` で採った規律と同じ
//! （docs/implementation-checklist.md §5）。
//!
//! **読み出しの 3 つの口のうち、この分岐があるのは接続状態だけ**である:
//! 現在値（[`CollectorService::values`]）は葉を読むので判定が
//! [`CollectorContext::published`] そのもの（タスクの生死に左右されない）、
//! イベント一覧（[`CollectorService::events`]）はそもそも `NotRunning` を
//! 返さず、読めなければ既に [`Readout::Unavailable`]。
//!
//! `crates/` は**変更しない** - `Drop` が無いことは**事実として参照している
//! だけ**で、接続タスクを道連れにする設計にすべきかは `banto-collect` 側の
//! 別の判断（このアプリが先取りしない）。
//!
//! # 公開用の型 - 何を載せ、何を落としたか
//!
//! [`CollectorStateView`] と同じ考え方（#407 レビュー P2-2）で、**`banto-collect`
//! の内部型をそのままワイヤに載せない**。3 つの口の読み取りは
//! [`COLLECT_READ_ROLE`]（`viewer` 以上）に開いているので、**内部パス・接続先
//! ホスト・資格情報にあたるものを含めない**。
//!
//! | 公開型 | 載せるもの | 落としたもの |
//! | --- | --- | --- |
//! | [`CurrentSampleView`] | 値・`ptimeMs`・品質（`good`/`bad`/`stale`） | 無し（`banto_collect::CurrentSample` の全部。機微なものが無い） |
//! | [`ConnectionStatusView`] | `connected` / `reconnecting`（`attempt`）/ `stopped` | 無し（接続先ホスト・ポートはそもそもこの型に無い） |
//! | [`CollectEventRow`] | `id`・`tsMs`・`kind`・`connectionKey`・`tagKey`・`level`・`value` | **`detail`（自由文）** |
//!
//! 地図の鍵（`conn:<id>` / `tag:<id>`）は**そのまま載せる** - これは
//! レジストリの行 id であって接続先の情報ではなく、画面がタグ名・接続名と
//! 突き合わせるのに要る。
//!
//! **`collect_events.detail` を落とした理由**: あの列は自由文で、中身は
//! `banto-collect` が**外から受け取った文言をそのまま**入れている -
//! `plc_disconnected` は `banto_plc::PlcError` の文言（DNS 解決の失敗文など
//! **接続先が出うる**）、`append_failure_entered` は `banto_tstore` の
//! 書き込みエラー（**ファイルパスが出うる**）。C-2 の P2 で
//! [`CollectorState::StartFailed`] の `reason` を viewer に見せてしまったのと
//! **同じ類の漏れ**なので、ここは種類ごとに選り分けるのではなく
//! **列ごと SELECT しない**（[`CollectorService::events`] の SQL に `detail` が
//! 無い）。種類ごとの白名簿にすると、次に `detail` を足した誰かの分が黙って
//! 漏れる - 型と SQL で落としておけば、載せようとした時点で手が止まる。
//!
//! **残る制約（黙って落とさないために明記する）**: 画面は切断の理由や
//! 書き込み失敗の詳細を**見られない**。必要になったら、`banto-collect` 側で
//! 「利用者に見せてよい理由」を分類した項目を持たせる（自由文をフィルタする
//! のではなく、発生源で分ける）のが筋で、それは C-3a ではやらない。
//!
//! # 非有限の浮動小数点は公開する形で正規化する（#408 レビュー P2-2）
//!
//! `f64` は NaN・`+∞`・`-∞` を取りうるのに、**`serde_json` はそのどれも
//! `null` にする**（JSON に非有限の数が無いため）。[`CurrentSampleView`] が
//! [`banto_collect::CurrentSample`] の `value` と `quality` をそのまま写して
//! いた C-3a の実装では、`Some(f64::NAN)` + [`Quality::Good`] が
//!
//! ```json
//! {"value":null,"ptimeMs":42,"quality":"good"}
//! ```
//!
//! になる。**数値として使えないのに品質は good** なので、
//! recorder-requirements.md §3.2 が要求している「画面が Bad / Stale を
//! 出し分ける」が成立しない - 画面は品質を見て異常を判別できず、値が
//! 消えている理由も分からない。
//!
//! **実際の入力経路から到達する**: Modbus の浮動小数点デコードは非有限値を
//! 除外しておらず、`banto-collect` の `record_group()` も `Some(value)` /
//! `Quality::Good` のまま現在値キャッシュへ入れる。
//!
//! そこで**公開型への変換時に `is_finite()` を確かめ、非有限なら
//! `value: None` / `quality: bad` に正規化する**（[`CurrentSampleView`] の
//! [`From`]）。`number | null` と `good`/`bad`/`stale` という**現行の契約は
//! そのまま**で、語彙は増やさない。
//!
//! **なぜ変換の場所で直すのか**:
//!
//! * `crates/` は変更禁止（この PR の方針）なので、Modbus のデコード側や
//!   `banto-collect` のキャッシュ側では直せない。
//! * そもそも**内部のキャッシュには元の値が残ってよい** - 直したいのは
//!   「外に出す形が自己矛盾していること」であって、収集エンジンが何を
//!   受け取ったかではない。**公開する形だけを揃える**のは
//!   [`CollectorStateView`]（`reason` を落とす）や [`CollectEventRow`]
//!   （`detail` を落とす）と同じ層の仕事で、変換が 1 箇所
//!   （[`CurrentSampleView::from`]）しかないので**経路によって食い違わない**。
//! * デコード側の扱い（そもそも非有限値を読み取り失敗にするか）は別の判断で、
//!   直すならこの層ではなく発生源。ここで正規化しても**その判断を先取り
//!   しない**（公開する形が自己矛盾しないことだけを保証する）。
//!
//! # 無応答への上限 - 「打ち切り」は「失敗」ではない
//!
//! ライフサイクル操作は [`COLLECT_OPERATION_TIMEOUT`] で**待つのをやめる**。
//! 上限そのものの根拠はその定数の doc を参照。ここで押さえておくのは**言い
//! 分け**（#400 で確立したもの）:
//!
//! * 打ち切ったのは**待ち時間**であって**操作ではない**。ライフサイクルは
//!   専用タスクが所有しているので、**待つのをやめてもタスク側の処理は最後まで
//!   進む**。
//! * したがって [`CollectOutcome::pending`] が `true` のとき、呼び出し側は
//!   「失敗しました」と言ってはいけない。言うべきは「**まだ終わっていない**」。
//! * 「今どうなっているか」は [`CollectorService::state`] が**キューを通らずに**
//!   答えるので、打ち切った側もその後の画面も、そこを見れば追いつける。
//!
//! そのために [`CollectorState::Starting`] がある。上限で打ち切ったあと
//! `state()` が `Stopped` を返すと**嘘になる**（起動処理はタスク側で続いて
//! いる）ので、タスクは**tstore を開く前に**`Starting` を立て、成否が
//! 決まってから `Running` / `NoTargets` / `StartFailed` に遷移する。
//!
//! # 「受け付けた」とも言い切らない - 投入と完了待ちを分ける
//!
//! 上の言い分けには**裏返し**がある（#407 レビュー P2-1）。「失敗したと言い
//! 切らない」のと同じ厳密さで、「**受け付けたと言い切らない**」こと。
//!
//! [`CollectOutcome::pending`] は「**後から必ず実行される**」という約束で
//! あって、「混雑で投げられなかった」の言い換えではない。C-2 の初版は上限
//! 1 本を**キューへの投入待ちごと**包んでいたので、
//!
//! 1. 収集対象がある状態で起動処理が待機し、後続の操作でキュー（深さ
//!    [`COMMAND_QUEUE_DEPTH`]）が埋まる、
//! 2. `stop()` が満杯のキューに入れず `send().await` で待つ、
//! 3. 上限で打ち切り、呼び出し側には `pending: true` が返る、
//! 4. しかし**停止コマンドはどこにも送られていない**ので、起動処理が復帰しても
//!    停止は実行されず、**収集は動き続ける**、
//!
//! という経路で「受付済み」と嘘をついた。しかも REST / Tauri はこの戻り値を
//! `result: "ok"` / `pending: true` で監査に残すので、**応答も監査も受付済み
//! 扱い**になっていた。
//!
//! そこで上限を**2 本に分ける**（[`CollectorService::lifecycle`]）:
//!
//! * **投入**: [`COLLECT_ENQUEUE_TIMEOUT`] 付きの
//!   [`tokio::sync::mpsc::Sender::send_timeout`]。入らなかったら「**混雑により
//!   未受付**」という**エラー**を返す（`pending` ではない）。**実際に何も
//!   起きていない**のだから、ここは言い切ってよい。エラー経路なので
//!   REST / Tauri の記録も走らず、`result: "ok"` では残らない。
//! * **完了待ち**: 投入に**成功した後**だけ [`COLLECT_OPERATION_TIMEOUT`] を
//!   掛ける。ここで打ち切ったときだけ `pending: true`（＝タスクが必ず実行する）。
//!
//! **無期限待機には戻さない**（それは #400 で潰した「画面が永久に固まる」
//! そのものなので）。
//!
//! # 同じ `data.dir` を 2 つのプロセスで開かないこと（未防止の制約）
//!
//! デスクトップアプリと `banto-serve` は**どちらも**起動時に自動開始するので、
//! **両方を同時に起動して同じ `data.dir` を指していると、2 つの収集エンジンが
//! 同じ時系列ファイル群へ書き込む**（同じ設定 DB を共有していれば同じ
//! `data.dir` を解決するので、これは十分に起こりうる）。`banto-tstore` は
//! プロセス間の排他を持たないため、結果は二重書き込み - 同じ時刻の行が
//! 2 回入る、ローテーションが互いのファイルを踏む、といった壊れ方をする。
//!
//! **防止機構は C-2 時点で実装していない**（ロックファイル等は別途）。
//! 運用上の約束として、**同じ `data.dir` に対して収集するプロセスは 1 つだけ**
//! にすること。`banto-serve` はあくまで開発・E2E 用の単体サーバーなので、
//! デスクトップアプリと併走させない。
//!
//! # 終了フックからの停止
//!
//! `src-tauri` の `shutdown_app_state` も**同じ口**（[`CollectorService::stop`]）
//! を通る。あちらは後始末全体に 5 秒の予算があり、超えたら待つのをやめるが、
//! **やめるのは待つ側だけで、タスク側の停止処理（join と最終 flush）は
//! そのまま続く**。プロセスがその直後に終わるので実害は無い - 途中で
//! 切り上げても、失われうるのは最後の未 flush 分だけ（固まる方が悪い）。
//!
//! # 保持期間（`retention.days`）について
//!
//! `crate::settings::StoreSettings` が `data.dir` / `retention.days` を持つが、
//! **このモジュールのコードはファイルを一切削除しない**。期限超過ファイルの
//! 自動削除は docs/recorder-requirements.md §3.4 にある機能だが、**別途
//! 実装する**（誤って削除を先取りしない）。ここは設定値を持つだけ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use banto_collect::{
    build_config, ClientFactory, CollectError, CollectEvent, Collector, CollectorOptions,
    ConnectionStatus, CurrentSample, CurrentValuesHandle, EventSink, Quality,
};
use banto_core::BantoError;
use banto_tstore::{Clock, SystemClock};
// ロールの下限（`COLLECT_OPERATION_ROLE` / `COLLECT_READ_ROLE`）だけ、この
// service 層が名前を出す - 両経路の床を 1 か所で決めるため（transport の
// 知識ではなく「この資源を触ってよいのは誰か」という資源側の性質）。
use crate::users::Role;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 収集サービスの状態。**必要最小限の 5 つ**だけ（語彙を増やさない）。
///
/// * [`Self::Stopped`] - 止まっている（まだ一度も起動していない、または
///   停止した）。
/// * [`Self::Starting`] - **起動処理の最中**（構成の組み立てと tstore を
///   開く往復）。C-2 で足した - [`COLLECT_OPERATION_TIMEOUT`] で待つのを
///   やめたあと `Stopped` を返すと嘘になるため（このモジュールの doc
///   「無応答への上限」）。ここから必ず `Running` / `NoTargets` /
///   `StartFailed` のどれかへ抜ける。
/// * [`Self::Running`] - 走っている。`groups`/`tags` は**起動時に実際に
///   採用された**収集対象の数。
/// * [`Self::NoTargets`] - **有効なタグが 1 件も無い**ので起動しなかった
///   （有効な接続やグループだけがあってタグが空、も含む）。**エラーでは
///   ない**（このモジュールの doc 参照）。
/// * [`Self::StartFailed`] - 起動を試みて失敗した。`reason` は
///   [`CollectError`] の文言そのまま。
///
/// **これ以上増やさない。** 停止中を表す `Stopping` は足していない: 停止は
/// 短い（接続タスクは `biased` select で停止を最優先に見ており、`connect()`
/// は別タスク）ので、停止の最中に `Running` のままに見えるのは**安全側の
/// 嘘**（「まだ止まっていないかもしれない」と読める）で済む。`Starting` は
/// そうはいかない - `Stopped` のままだと「もう何も動いていない」という
/// **危険側の嘘**になるので、こちらだけ足した。
///
/// `Serialize` を導出しているのは、**操作の戻り値**
/// （[`CollectOutcome::status`]）がこの型のままワイヤに載るため -
/// `crate::hub` が `HubStatus` を判別共用体のまま画面へ渡しているのと同じ
/// 作法（`{"state":"running","groups":2,"tags":10}`）。
///
/// **状態の取得（`viewer` にも開いている経路）はこの型を載せない。**
/// `reason` を落とした [`CollectorStateView`] を通す - 理由はこのモジュールの
/// doc「内部の状態と、状態取得で公開する形を分ける」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CollectorState {
    Stopped,
    Starting,
    Running { groups: usize, tags: usize },
    NoTargets,
    StartFailed { reason: String },
}

/// [`CollectorState`] の**公開用**の形（#407 レビュー P2-2）。
///
/// [`CollectorState`] と**同じ 5 つの状態**を、**同じ綴り**
/// （`#[serde(tag = "state", rename_all = "camelCase")]` なので
/// `stopped`/`starting`/`running`/`noTargets`/`startFailed`。
/// [`CollectorState::as_str`] や監査の `collectState` とも一致する）で表す。
/// 違いは 1 つだけ - **[`Self::StartFailed`] に `reason` が無い**。
///
/// **なぜ型ごと分けるのか**: [`CollectorState::StartFailed`] の `reason` は
/// [`CollectError`] の文言そのままで、接続先のホストや `data.dir` の絶対パスを
/// 含みうる。状態の読み取りは `viewer` にも開いている
/// （[`COLLECT_READ_ROLE`]）ので、**内部の型をそのままワイヤへ載せると、
/// 「状態に機微な情報を含めない」という床を下げた前提が実装で満たされない**。
/// `#[serde(skip)]` を `reason` に付けて 1 つの型で済ます手もあるが、それだと
/// **診断（理由を見たい）と公開（見せたくない）が同じ型の同じフィールドに
/// 同居し続ける**ので、次に誰かが `reason` を使う口を足したときに黙って漏れる。
/// 型が違えば、公開経路に内部の型を載せようとした時点でコンパイルが止まる。
///
/// **変換は [`From`] の 1 実装だけ**。REST も Tauri も
/// [`CollectorService::state_view`] を通るので、経路によって公開する情報が
/// 割れようがない（[`COLLECT_AUDIT_RESOURCE`] と同じ作法）。
///
/// 起動に失敗した理由が**どこで利用者に伝わるか**（と、画面を再読み込み
/// すると分からなくなるという残る制約）は、このモジュールの doc
/// 「理由はどこで利用者に伝わるか」を参照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CollectorStateView {
    Stopped,
    Starting,
    Running {
        groups: usize,
        tags: usize,
    },
    NoTargets,
    /// 起動を試みて失敗した。**理由は載せない**（上の doc 参照）。
    StartFailed,
}

impl From<&CollectorState> for CollectorStateView {
    fn from(state: &CollectorState) -> Self {
        match state {
            CollectorState::Stopped => Self::Stopped,
            CollectorState::Starting => Self::Starting,
            CollectorState::Running { groups, tags } => Self::Running {
                groups: *groups,
                tags: *tags,
            },
            CollectorState::NoTargets => Self::NoTargets,
            // **ここが全部**: 理由を落とすのはこの 1 行だけ。
            CollectorState::StartFailed { .. } => Self::StartFailed,
        }
    }
}

impl CollectorState {
    /// 走っているか。`state()` の呼び出し側が毎回 `matches!` を書かずに
    /// 済むだけの薄い述語。
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// 監査ログ用の短い識別子。**`serde` が付けるタグと同じ綴り**（上の
    /// `#[serde(tag = "state", rename_all = "camelCase")]`）なので、監査で
    /// 見た値と画面で見た値が一致する。`crate::hub` の `HubStatus::as_str`
    /// と同じ役割。
    ///
    /// **理由（`StartFailed` の `reason`）も件数も載せない** - 監査の detail に
    /// 入れてよいのは「操作者」と「結果の状態」までで、接続先・ファイルパス
    /// といった内部の詳細を持ち込まないため（`reason` は [`CollectError`] の
    /// 文言そのままで、ホストやパスを含みうる）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running { .. } => "running",
            Self::NoTargets => "noTargets",
            Self::StartFailed { .. } => "startFailed",
        }
    }
}

/// 監査ログ（`crate::audit`）で収集の操作に付ける `resource`。
///
/// **拒否も成功も、REST も Tauri も、必ずこの 1 つの値**。定数にして両方の
/// 経路から参照させているのは、#397 の P2-D で踏んだ穴を繰り返さないため:
/// あのときは Hub の拒否が REST 側 `"hub"` / Tauri 側 `"settings"` に割れて
/// いて、「hub への拒否だけ抽出する」フィルタが Tauri 分を黙って取りこぼして
/// いた。**同じ操作が経路によって別の `resource` にならない**ことを、
/// 申し合わせではなく 1 つの定数で担保する。
///
/// `action` の側は操作ごとに違う（`start` / `stop` / `restart`、拒否は
/// ガードが書く `denied`）ので、既存の語彙（`create`/`delete`/… と同じ
/// 「小文字の動詞」）に合わせて各呼び出し側が書く。
pub const COLLECT_AUDIT_RESOURCE: &str = "collect";

/// 収集の**変更操作**（開始・停止・再起動）に要る最低ロール。
///
/// **`editor` 以上**（`admin` ではない）: docs/recorder-requirements.md §3.6
/// 「収集の開始/停止は editor 以上」と、そこに追記された 2026-09-18 の
/// オーナー決定 -「banto-hub は同種の制御を admin 限定にしているが、
/// chronogazer は別プロダクトであり R0 本文の決定を継承する。**banto-hub の
/// 先例に引きずられて admin 限定へ変更しない**」。
///
/// [`COLLECT_AUDIT_RESOURCE`] と同じく**両経路がこの 1 つの定数を参照する** -
/// REST（`crate::rest` の `collect_router` が変更ルートに掛ける
/// `RoleGuard`）と Tauri（`src-tauri` の `require_collect_editor`）で床が
/// 割れようがないようにするため。
pub const COLLECT_OPERATION_ROLE: Role = Role::Editor;

/// 収集の**状態の読み取り**に要る最低ロール。
///
/// **`viewer` 以上**（2026-09-20 オーナー決定、#407 レビュー）。R0 §3.6 の
/// viewer は「**閲覧のみ**」であって「何も見えない」ではない。収集が動いて
/// いるかどうかは**監視画面（C-3 / R1-D）の基本情報**であり、この床で公開
/// するのは [`CollectorStateView`] - 「止まっている / 起動中 / 走っている
/// （件数）/ 対象なし / 起動失敗」しか語らず、接続先もキーもファイルパスも
/// 出ない。viewer に見せて困るものが無い。
///
/// **その前提は型で担保する**（#407 レビュー P2-2）。内部の
/// [`CollectorState::StartFailed`] は `reason` を持っており、そこには
/// [`CollectError`] の文言（ホストや絶対パスを含みうる）がそのまま載る。
/// 床をここまで下げてよい理由が「状態に機微な情報が無いこと」である以上、
/// **公開経路には内部の型を載せない** - 必ず
/// [`CollectorService::state_view`] を通す。
///
/// `crate::hub` の `hub_status` が `admin` 限定なのは **Hub の接続先と
/// キーの情報**を扱うからで、**収集の稼働状態はそれとは性質が違う** -
/// 先例に引きずられない、という 2026-09-18 の決定と同じ考え方をここでも採る。
///
/// **床は分けても [`COLLECT_AUDIT_RESOURCE`] は分けない**: 読み取りの拒否も
/// 変更の拒否も、同じ `"collect"` で記録する。
pub const COLLECT_READ_ROLE: Role = Role::Viewer;

/// ライフサイクル操作（`start`/`stop`/`restart`）1 回の結末。
///
/// **`pending` は「失敗」ではない。** `true` は「[`COLLECT_OPERATION_TIMEOUT`]
/// まで待ったが、まだ終わっていない」という意味で、打ち切ったのは**待ち時間**
/// だけ - ライフサイクルタスク側の処理はそのまま最後まで進む（このモジュールの
/// doc「無応答への上限」/ #400 で確立した言い分け）。呼び出し側は
/// 「失敗しました」ではなく「**まだ終わっていません**」と言い、続きは
/// [`CollectorService::state`] で追うこと。
///
/// **裏返しも同じだけ厳密に**（#407 レビュー P2-1）: `pending` が返るのは
/// **依頼がライフサイクルタスクのキューに確かに入った後**だけ。混雑で入れられ
/// なかったときは「受け付けた」と言わず、**エラー**を返す（このモジュールの
/// doc「「受け付けた」とも言い切らない」/ [`COLLECT_ENQUEUE_TIMEOUT`]）。
/// `pending: true` は「**後から必ず実行される**」という約束である。
///
/// `status` は**打ち切った時点**の状態なので、`pending == true` のときはほぼ
/// [`CollectorState::Starting`]（あるいは、停止の最中なら直前の状態）になる。
///
/// `status` が**公開用の [`CollectorStateView`] ではなく内部の
/// [`CollectorState`]** なのは意図的 - これを受け取れるのは
/// [`COLLECT_OPERATION_ROLE`]（`editor` 以上）＝**操作を実行した本人**だけで、
/// その人には起動失敗の理由が伝わってよいため（モジュール doc
/// 「理由はどこで利用者に伝わるか」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectOutcome {
    pub status: CollectorState,
    pub pending: bool,
}

impl CollectOutcome {
    /// 操作が終わった。
    fn settled(status: CollectorState) -> Self {
        Self {
            status,
            pending: false,
        }
    }

    /// 上限まで待ったが、まだ終わっていない。
    fn still_working(status: CollectorState) -> Self {
        Self {
            status,
            pending: true,
        }
    }
}

// --- C-3a: 読み出しの結末と、その公開用の形 ---------------------------------

/// 読み出し 1 回の結末。**3 つの口（現在値・接続状態・イベント一覧）が
/// 共有する**（このモジュールの doc「読み出しの 3 つの口」）。
///
/// 「**走っていない**」「**読めなかった**」「**読めて 0 件**」を
/// **別の値**にするためだけの型で、これが C-3a の一番の勘所
/// （docs/implementation-checklist.md §5「エラーを空に潰さない」）。空の
/// コレクションを返して 3 つを 1 つに潰すと、画面は「0 件です」としか言えず、
/// 利用者は**収集が止まっているのか・読めなかったのか・本当に何も無いのか**を
/// 区別できない。
///
/// ワイヤ形は判別共用体（`{"state":"ready","data":…}` /
/// `{"state":"notRunning"}` / `{"state":"unavailable"}`）で、
/// [`CollectorStateView`] と同じ作法（`crate::hub` の `HubStatus` 以来の
/// このアプリの流儀）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Readout<T> {
    /// 収集が走っていない（停止中 / [`CollectorState::NoTargets`] /
    /// [`CollectorState::StartFailed`]）。**0 件ではない。**
    ///
    /// **これを返してよいのは「状態を見て、走っていないと分かった」とき
    /// だけ**（#408 レビュー）。読み取りの仕組みが壊れていて**判定できな
    /// かった**ときは [`Self::Unavailable`] - 「分からない」を「止まって
    /// いる」側へ倒さない（このモジュールの doc「タスクが居ないことを
    /// 「走っていない」と答えない」）。
    NotRunning,
    /// 走ってはいるが、**この 1 回が読めなかった** -
    /// [`COLLECT_READ_TIMEOUT`] で打ち切った、または DB が読めなかった。
    ///
    /// **「失敗しました」とも「0 件です」とも言わないこと**（#400 で確立した
    /// 言い分けと同じ）。次のポーリングで読めるかもしれない。
    Unavailable,
    /// **読めた**。`data` が空でも、それは「0 件でした」という**事実**。
    Ready { data: T },
}

impl<T> Readout<T> {
    /// 監査・ログ・テスト用の短い識別子。**`serde` が付けるタグと同じ綴り**
    /// （[`CollectorState::as_str`] と同じ役割）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotRunning => "notRunning",
            Self::Unavailable => "unavailable",
            Self::Ready { .. } => "ready",
        }
    }

    /// 読めたときだけ中身を借りる。**`None` が「0 件」を意味しない**ことに
    /// 注意（0 件は `Some` の中の空）。
    pub fn data(&self) -> Option<&T> {
        match self {
            Self::Ready { data } => Some(data),
            _ => None,
        }
    }
}

/// [`banto_collect::Quality`] の公開用の形（`good` / `bad` / `stale`）。
///
/// 内部型が `Serialize` を導出していないので、**ワイヤの綴りをこちらで
/// 決める**（画面が Stale / Bad を出し分けられること自体が
/// recorder-requirements.md §3.2 の要求）。`Stale` は保存されず
/// **読み取り時に導出される**品質で、
/// [`banto_collect::CurrentValuesHandle::snapshot`] が既に導出済みの値を
/// くれる - ここでは詰め替えるだけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QualityView {
    Good,
    Bad,
    Stale,
}

impl From<Quality> for QualityView {
    fn from(quality: Quality) -> Self {
        match quality {
            Quality::Good => Self::Good,
            Quality::Bad => Self::Bad,
            Quality::Stale => Self::Stale,
        }
    }
}

/// [`banto_collect::CurrentSample`] の公開用の形。
///
/// **品質と時刻を必ず載せる**: 値だけ返すと、画面は「通信エラーで古い値を
/// 表示し続けている」のか「今読めた値」なのかを言えない
/// （recorder-requirements.md §3.2 の Stale / Bad 表示）。落としたものは
/// **無い** - `CurrentSample` は値・時刻・品質しか持たず、機微なものが無い
/// （このモジュールの doc「公開用の型」）。
///
/// **非有限の値（NaN・±∞）はここで正規化する** - このモジュールの doc
/// 「非有限の浮動小数点は公開する形で正規化する」（#408 レビュー P2-2）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentSampleView {
    /// スケーリング済みの値。**読めなかったサンプルは `None`**（`quality` が
    /// `bad`）- ここを 0 に潰さない。
    ///
    /// **常に有限**（`is_finite()`）。非有限の値は [`From`] で `None` +
    /// `bad` に正規化されるので、`null` と `good` が同居することはない。
    pub value: Option<f64>,
    /// このサンプルの時刻（UTC epoch ミリ秒、収集 PC の時計）。
    pub ptime_ms: i64,
    pub quality: QualityView,
}

impl From<&CurrentSample> for CurrentSampleView {
    /// **非有限の値をここで畳む**（#408 レビュー P2-2。理由はこのモジュールの
    /// doc「非有限の浮動小数点は公開する形で正規化する」）。
    ///
    /// `ptime_ms` は**そのまま**残す - 「いつのサンプルか」は値が使えなくても
    /// 正しい情報で、`Quality::Bad` の既存のサンプル（`value: None`）でも
    /// 同じように載せている。
    fn from(sample: &CurrentSample) -> Self {
        match sample.value {
            // NaN・+∞・-∞: `serde_json` はこれを `null` にするので、
            // そのまま写すと「**値が無いのに品質は good**」になり、画面が
            // 品質を見て異常を判別できなくなる。**数値として使えない**の
            // だから、`value` を落とすのと同時に品質も `bad` にする。
            Some(value) if !value.is_finite() => Self {
                value: None,
                ptime_ms: sample.ptime_ms,
                quality: QualityView::Bad,
            },
            _ => Self {
                value: sample.value,
                ptime_ms: sample.ptime_ms,
                quality: sample.quality.into(),
            },
        }
    }
}

/// [`banto_collect::ConnectionStatus`] の公開用の形
/// （`{"status":"connected"}` / `{"status":"reconnecting","attempt":2}` /
/// `{"status":"stopped"}`）。
///
/// 落としたものは**無い**: この型は接続先のホストもポートも資格情報も
/// 持っていない（持っているのは状態と再接続の試行回数だけ）。`attempt` は
/// 残す - 「切れてから何回目か」はヘルス表示の実用情報で、機微ではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ConnectionStatusView {
    Connected,
    Reconnecting { attempt: u32 },
    Stopped,
}

impl From<&ConnectionStatus> for ConnectionStatusView {
    fn from(status: &ConnectionStatus) -> Self {
        match status {
            ConnectionStatus::Connected => Self::Connected,
            ConnectionStatus::Reconnecting { attempt } => Self::Reconnecting { attempt: *attempt },
            ConnectionStatus::Stopped => Self::Stopped,
        }
    }
}

/// `collect_events` 1 行の公開用の形（#383 段階2b / R1-C の C-3a）。
///
/// 列は `crates/banto-collect/migrations/0001_collect_events.sql` のとおり
/// （`id` / `ts` / `kind` / `connection_key` / `tag_key` / `level` / `value` /
/// `detail`）で、**`detail` だけを落としている** - 理由はこのモジュールの doc
/// 「公開用の型 - 何を載せ、何を落としたか」（自由文で、接続先やファイルパスを
/// 含みうる）。落とすのは型だけでなく
/// [`CollectorService::events`] の **SQL でも**（`SELECT` に `detail` が無い）。
///
/// `kind` / `level` を文字列のままにしているのは、**この表の語彙を決めている
/// のは `banto-collect` であって、このアプリではない**から
/// （`EventKind::as_str` が権威。DDL にも `CHECK` は無く、H4 で 4 種類が
/// スキーマ変更なしに足された）。ここで列挙型に写すと、`banto-collect` が
/// 種類を足すたびにこのアプリが**知らない種類を落とす**か失敗するようになる。
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CollectEventRow {
    pub id: i64,
    /// UTC epoch ミリ秒（`collect_events.ts`）。
    pub ts_ms: i64,
    /// `collection_started` / `plc_disconnected` / `threshold_entered` など
    /// （`banto_collect::EventKind::as_str` の綴り）。
    pub kind: String,
    /// `conn:<id>`。収集エンジン全体のイベント（開始・停止）では `None`。
    pub connection_key: Option<String>,
    /// `tag:<id>`。`threshold_*` だけ `Some`。
    pub tag_key: Option<String>,
    /// `H` / `HH` / `L` / `LL`（`threshold_*` だけ）。
    pub level: Option<String>,
    /// しきい値を跨いだ値（`threshold_*` だけ）。
    pub value: Option<f64>,
}

/// イベント一覧の 1 ページ分の要求。
///
/// **監査ログ一覧（`crate::audit`）の流儀に合わせる**（docs/r1-plan.md の
/// R1-C「`collect_events` のイベント一覧ページ（banto 監査ログページの
/// 流儀）」）:
///
/// * 総件数は [`CollectEventList::total_count`] で返す（`crate::audit` の
///   `ListResult` と同じ綴り `totalCount`）、
/// * 既定の取得件数は **50**（[`COLLECT_EVENTS_DEFAULT_LIMIT`]。
///   `banto_core::Pagination` の既定と同じ）、
/// * 並び順は**新しい順**で固定（監査ログ画面の既定 `ts desc` と同じ）。
///
/// 監査ログ一覧と**違う**のは 2 点で、どちらも `GET` にしたことの帰結:
///
/// * 並べ替え・絞り込みは受け取らない（`POST .../list` + `ListParams` では
///   ないので、列名を渡す口が無い）。要るようになったら
///   `crate::audit` と同じ `ColumnMap` + `ListParams` の形に寄せること。
/// * **取得件数に上限を掛ける**（[`COLLECT_EVENTS_MAX_LIMIT`]）。`?limit=` は
///   URL に誰でも書けるので、`limit` を素通しにすると 1 リクエストで表全体を
///   読み出せてしまう。`crate::audit` の `POST` は `admin` 限定だが、この口は
///   `viewer` にも開いている。
///
/// **`as_of_id` = スナップショット境界**（#409 レビュー P2-2）: 指定すると
/// `id <= as_of_id` の行**だけ**を数え、並べる。画面は 1 つの「世代」の
/// 最初の応答で返ってきた境界（[`CollectEventList::as_of_id`]）を固定して、
/// 同じ世代の後続ブロックすべてに渡す - そうしないと、ブロック取得の合間に
/// 先頭へイベントが追加されたとき、`OFFSET` がずれて**境界で行が重複し、
/// 末尾の行が一覧から漏れる**。未指定なら「その時点の最大 `id`」を境界に
/// する（[`CollectorService::events`] の doc）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventPage {
    pub offset: u64,
    pub limit: u64,
    pub as_of_id: Option<i64>,
}

impl EventPage {
    /// 未指定・範囲外を**黙って直す**（拒否しない）。画面のページャが
    /// 素直に使えるようにするための純関数で、単体で表テストしている。
    ///
    /// * `offset` 未指定 = 先頭から。**`i64` に収まるところまで**に丸める -
    ///   SQLite の `OFFSET` は符号付きで、そのまま渡すと巨大な値が負に化けて
    ///   **`OFFSET 0`（＝先頭ページ）として扱われる**。「行き過ぎたページ」は
    ///   0 件で返すべきで、黙って 1 ページ目を返してはいけない。
    /// * `limit` 未指定 = [`COLLECT_EVENTS_DEFAULT_LIMIT`]。
    /// * `limit` は `1..=`[`COLLECT_EVENTS_MAX_LIMIT`] に丸める（`0` を
    ///   そのまま通すと「0 件読めました」という**意味の無い `Ready`** に
    ///   なってしまう - それは 3 状態の言い分けを濁す）。
    pub fn new(offset: Option<u64>, limit: Option<u64>) -> Self {
        Self {
            offset: offset.unwrap_or(0).min(i64::MAX as u64),
            limit: limit
                .unwrap_or(COLLECT_EVENTS_DEFAULT_LIMIT)
                .clamp(1, COLLECT_EVENTS_MAX_LIMIT),
            as_of_id: None,
        }
    }

    /// スナップショット境界を付ける（`None` = 付けない = その時点の最大
    /// `id` を境界にする）。範囲は直さない - 負の値や存在しない大きな値は
    /// 「その境界で数えた集合」をそのまま返すだけで（負なら 0 件）、読み出せる
    /// 範囲は広がらない。
    pub fn as_of(self, as_of_id: Option<i64>) -> Self {
        Self { as_of_id, ..self }
    }
}

impl Default for EventPage {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// イベント一覧の 1 ページ分の応答（[`CollectorService::events`] の
/// `Readout::Ready` の `data`）。
///
/// `rows` / `totalCount` は `banto_core::ListResult` と**同じ綴り**（監査ログ
/// 一覧と同じ形）で、そこに**この応答が使ったスナップショット境界**
/// `asOfId` を 1 つ足したもの（#409 レビュー P2-2）。`ListResult` は
/// `banto-core` の型でフィールドを足せないので、同じ綴りの型をここに置く。
/// `Readout` の形は変えていない。
///
/// * `total_count` も `rows` も **`id <= as_of_id` で絞った集合**から取る。
/// * **表が空のときの `as_of_id` は `0`**: `collect_events.id` は
///   `INTEGER PRIMARY KEY AUTOINCREMENT` で 1 から振られるので、`0` は
///   「どの行も含まない境界」を意味する。`null` にしないのは、画面が
///   「境界を受け取った」と「まだ受け取っていない」を取り違えないため
///   （空の世代の後続ブロックも同じ `0` を渡せば、同じ空集合を読む）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectEventList {
    pub rows: Vec<CollectEventRow>,
    pub total_count: u64,
    pub as_of_id: i64,
}

/// ライフサイクルタスクへの**操作**の依頼。応答はそれぞれの `oneshot` に返す。
///
/// **応答の受信側が落ちても（呼び出し側のキャンセル）、タスクは処理を
/// 最後まで続ける** - `send` が `Err` になるだけ。それがこの設計の要点
/// （このモジュールの doc「ライフサイクルは専用タスクが所有する」）。
///
/// **読み取りはこの enum に入れない**（#408 レビュー P2-1）。理由は
/// [`ReadCommand`] と、このモジュールの doc「読み取りと操作でキューを
/// 分ける」。
enum Command {
    Start(oneshot::Sender<Result<CollectorState, BantoError>>),
    Stop(oneshot::Sender<Result<CollectorState, BantoError>>),
    Restart(oneshot::Sender<Result<CollectorState, BantoError>>),
}

/// ライフサイクルタスクへの**読み取り**の依頼。**[`Command`] とは別のチャネル**
/// を通る（#408 レビュー P2-1。このモジュールの doc「読み取りと操作でキューを
/// 分ける」）。
///
/// 中身が 1 つなのは、キューを通る読み取りが接続状態しか無いため -
/// 現在値は葉に公開してあり（[`CurrentValuesHandle`]）、イベント一覧は
/// SQLite を直接読むので、どちらもこのタスクを経由しない。
enum ReadCommand {
    /// 接続状態の読み出し。`Collector::status` が `&Collector` を要求し、
    /// その中の `StatusMap` は `banto-collect` の `pub(crate)` なので、
    /// **ハンドルだけ葉へ持ち出すことができない**（このモジュールの doc
    /// 「現在値はキューから外して葉に公開する」の末尾）。
    ConnectionStatus(oneshot::Sender<Option<HashMap<String, ConnectionStatus>>>),
}

/// **操作用**キュー（[`Command`]）の深さ。ライフサイクル操作は人間の操作
/// 由来で秒に何度も来るものではないので、この深さが埋まるのは
/// 「タスクが 1 件の操作の中で長く止まっている」ときだけ。
///
/// 満杯になったら [`CollectorService::lifecycle`] が
/// [`COLLECT_ENQUEUE_TIMEOUT`] 付きで投入を試み、入らなかったら「**混雑により
/// 未受付**」という**エラー**を返す（`pending` にしない。#407 レビュー P2-1）。
///
/// **読み取りはこのキューに入らない**（#408 レビュー P2-1）- 入れていた頃は、
/// 閲覧のポーリングがここを埋めて**操作の投入を締め出していた**。
/// [`READ_QUEUE_DEPTH`] を参照。
const COMMAND_QUEUE_DEPTH: usize = 32;

/// **読み取り用**キュー（[`ReadCommand`]）の深さ。操作用
/// （[`COMMAND_QUEUE_DEPTH`]）とは**別のチャネル**で、**わざと小さい**。
///
/// **なぜ分けるのか（#408 レビュー P2-1）**: 読み取りは `viewer` にも開いた
/// ポーリングの口で、操作（`editor` 以上の開始・停止・再起動）とは**要求の
/// 性質も頻度も違う**。1 本のキューを共有していた C-3a では、
///
/// 1. 応答しない共有への `open` などで `start()` が長時間止まる、
/// 2. その間に画面が接続状態を 1 秒ごとにポーリングする。呼び出し側は
///    [`COLLECT_READ_TIMEOUT`] で待つのをやめて [`Readout::Unavailable`] を
///    受け取るが、**投入済みの要求はキューに残ったまま**、
/// 3. [`COMMAND_QUEUE_DEPTH`] 件が読み取りで埋まる、
/// 4. その後の `stop()` / `restart()` が [`ENQUEUE_BUSY_MESSAGE`] で
///    **未受付**になる、
///
/// という経路で「**閲覧が操作を締め出す**」ことが起きた。**読み取りの
/// 待ち時間に上限を掛けても、キューの中の要求数は減らない**（上限は
/// 呼び出し側が待つのをやめるだけ）ので、上限では直らない - 資源そのものを
/// 分けるしかない。
///
/// **枠が空くのは、タスクがその要求を取り出したときだけ**。呼び出し側が
/// [`COLLECT_READ_TIMEOUT`] で待つのをやめても枠は返さない（返す実装、
/// たとえば「打ち切ったら permit を解放する」にすると、**未処理の要求が
/// 再び積み上がる**だけで、分けた意味が無くなる）。枠が無い間の読み取りは
/// **待たずに** [`Readout::Unavailable`] で畳む - 「読めなかった」は
/// [`Readout`] に既にある正しい表現なので、**新しい状態は増やさない**。
///
/// **なぜ 4 か**: タスクが健全なら要求はミリ秒で捌けるので、普段はそもそも
/// 積まれない。積まれるのはタスクが止まっているときだけで、その間に何件
/// 溜めても答えは返らない（どれも [`COLLECT_READ_TIMEOUT`] で打ち切られる）
/// から、深さに価値が無い。それでも 1 にしないのは、**複数の画面・複数の
/// 経路（REST と Tauri）が同時にポーリングしうる**ため - 1 だと、正常時でも
/// 2 つ目の画面が毎回「読めませんでした」を見る。4 は「同時に覗く人数」の
/// 現実的な上限として取った。
const READ_QUEUE_DEPTH: usize = 4;

/// **読み出し**（[`CollectorService::connections`] /
/// [`CollectorService::events`]）が待つのをやめるまでの時間。
/// [`COLLECT_OPERATION_TIMEOUT`]（ライフサイクル操作の 30 秒）とは**別の
/// 上限**で、こちらの方がずっと短い。
///
/// **なぜ短いのか**: これは**ポーリングで引かれる口**（R1-D の監視画面が
/// 1 秒前後で現在値・接続状態を回す）。30 秒返らない口を 1 秒ごとに叩けば、
/// 飛行中の要求が積み上がるだけで画面は何も描けない - #400 で潰した
/// 「画面が固まる」の別の顔になる。**読み出しは「待つ」より「今は読めません
/// でしたと答える」方が正しい**（次の周回でまた聞けるので）。
///
/// **なぜ 2 秒か**:
///
/// * 健全なときの往復は**ミリ秒**。接続状態の読み出しはライフサイクルタスクが
///   キューから 1 件取り出して `HashMap` を `clone` するだけ、イベント一覧は
///   同居している SQLite の索引付き 1 ページ読み取り。秒を要する時点で異常。
/// * それでも 1 桁ミリ秒〜数百ミリ秒にしないのは、**正常だが一瞬混む**場面が
///   実際にあるため - 停止処理（接続タスクの join と最終 flush）や起動処理
///   （tstore を開く）の最中は、キューが数百ミリ秒単位で動かないことがある。
///   そこで毎回「読めませんでした」を出すと、画面が**正常時にちらつく**。
/// * 上を取って 5 秒・10 秒にしないのは、1 秒周期のポーリングで**飛行中の
///   要求が 5〜10 本重なる**から。2 秒なら最悪でも 2 本で、しかも利用者から
///   見て「一拍置いて『今は読めませんでした』が出る」応答になる。
///
/// **ライフサイクル操作の 30 秒をここに使い回さないこと。** あちらが長いのは
/// 応答しない共有への `open` を見限る値として選んだからで
/// （[`COLLECT_OPERATION_TIMEOUT`] の doc）、待っている相手も回数も違う。
pub const COLLECT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// イベント一覧の既定の取得件数。`banto_core::Pagination` の既定
/// （`limit: 50`）と同じ値 - 監査ログ一覧と体感を揃えるため
/// （[`EventPage`] の doc）。
pub const COLLECT_EVENTS_DEFAULT_LIMIT: u64 = 50;

/// イベント一覧の取得件数の**上限**。`?limit=` は URL に誰でも書けるので、
/// 素通しにすると 1 リクエストで `collect_events` 全体を読み出せてしまう
/// （この口は `viewer` にも開いている）。500 は「画面 1 ページぶんとしては
/// 十分に多く、表全体を一気に抜くには足りない」ところ。
pub const COLLECT_EVENTS_MAX_LIMIT: u64 = 500;

/// ライフサイクル操作の依頼を**キューに入れる**のを諦めるまでの時間
/// （[`COLLECT_OPERATION_TIMEOUT`] とは**別の上限**。#407 レビュー P2-1）。
///
/// **なぜ完了待ちと分けるのか**: この待ちが解けなかった場合、依頼は
/// **どこにも届いていない**。だから「まだ終わっていない」
/// （[`CollectOutcome::pending`]）ではなく「**受け付けられなかった**」と
/// 言い切るべきで、言い分けが違う以上、上限も別に持つのが素直
/// （このモジュールの doc「「受け付けた」とも言い切らない」）。
///
/// **なぜ 5 秒か（完了待ちの 30 秒より短い）**: ここで待っているのは
/// **I/O ではなく、ライフサイクルタスクがキューから 1 件取り出すこと**だけ。
/// 取り出した瞬間に空きができるので、
///
/// * タスクが手空きなら**即座**に入る、
/// * 読み出しや通常の `start`/`stop` が数件詰まっているだけなら、それらは
///   ミリ秒で捌けるので**やはりすぐ**入る、
/// * [`COMMAND_QUEUE_DEPTH`] 件が滞留したままということは、タスクが 1 件の
///   操作の中で長く止まっている（応答しない共有への open 等）という意味で、
///   **その滞留は最大で完了待ちの上限ぶん続きうる**。
///
/// 3 番目の場合に投入を待ち続けても得るものが無い - 待った末に入っても、
/// そこから完了待ちの 30 秒が**新たに**始まるので、呼び出し側の合計待ち時間が
/// 倍近くになるだけで、それは「固まっている」と区別が付かない。5 秒は通常の
/// キュー回転（ミリ秒）の 3 桁上にあって取りこぼしようがなく、かつ合計の
/// 最悪値を 35 秒に抑えて完了待ちの予算からほとんどはみ出させない。
///
/// **無期限の `send().await` には戻さないこと。** それは #400 で潰した
/// 「画面が永久に固まる」経路そのもので、しかも今度は「受付済み」と嘘を
/// つきながら固まる。
pub const COLLECT_ENQUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 混雑で依頼をキューに入れられなかったときの文言。
///
/// **定数にしているのは、テストが「未受付が明示されている」ことを
/// 文字列の部分一致ではなく等値で固定するため** - 「タスクが居ない」
/// （[`LIFECYCLE_GONE_MESSAGE`]）との読み分けが、テスト側でも実装側でも
/// 曖昧にならないようにする。
const ENQUEUE_BUSY_MESSAGE: &str =
    "収集の操作を受け付けられませんでした（処理が混み合っています）。\
     前の操作が終わるのを待って、もう一度お試しください（この操作は実行されていません）";

/// ライフサイクルタスクが居なくなっていた（= panic した。通常は起こらない）
/// ときの文言。**[`ENQUEUE_BUSY_MESSAGE`] と読み分けられること**が要点で、
/// どちらも「実行されていない」が、後者は再試行で直らない。
const LIFECYCLE_GONE_MESSAGE: &str =
    "収集サービスの内部タスクが停止しています（アプリを再起動してください）";

/// ライフサイクル操作（`start`/`stop`/`restart`）が**キューに入った後**、
/// 完了を待つのをやめるまでの時間。打ち切りの意味は [`CollectOutcome`] の
/// doc を参照（失敗ではない）。
///
/// **キューへの投入はこの上限の外**（[`COLLECT_ENQUEUE_TIMEOUT`]）。1 本で
/// 両方を包むと、**投げられもしなかった操作が `pending: true`（＝後から必ず
/// 実行される）で返る**ため（#407 レビュー P2-1）。
///
/// **なぜ上限が要るのか**: 操作の口を生やした以上、ここは #400 で潰した
/// 「画面が永久に固まる」経路そのものになる。往復の相手はライフサイクル
/// タスクで、そのタスクは `TsWriter::open_with_options`（tstore を開く）を
/// `await` する - `reject` ではなく**無応答**になりうる層なので、上限が
/// 無ければ呼び出し側は永久に返ってこない。
///
/// **なぜ 30 秒か（`crate::hub` の 15/60 秒とは根拠が違う）**: Hub の上限は
/// 「別の機械への HTTP/WS 往復」に対する値だが、こちらが待っているのは
/// **同じプロセスの中のローカル I/O** だけ -
///
/// * `build_config`: 同居している SQLite（レジストリ）の読み取り、
/// * `Collector::start`: `data.dir` の下に tstore の書き手を開く、
/// * `Collector::stop`: 接続タスクの join と最終 flush。
///
/// PLC への接続は**含まれない**（接続は別タスクで、`start` はその完了を
/// 待たない）。健全なローカルディスクならどれもミリ秒で、秒を要する時点で
/// 既に異常である。
///
/// それでも 1 桁秒にしないのは、**`data.dir` が外付け・ネットワークドライブを
/// 指しうる**ため（[`resolve_data_dir`] の doc - 絶対パス指定の主目的が
/// まさにそれ）。応答しない SMB 共有への open は、OS が諦めるまで数十秒
/// 単位で止まることがある。3 秒で切ると「少し遅いだけの共有」を毎回
/// 見限ってしまい、逆に分単位にすると固まっているのと区別が付かない。
/// 30 秒は「遅いが必ず終わるローカル I/O」の上に十分あり、かつ
/// 「利用者がアプリを壊れたと判断する前」に収まる。
///
/// 終了時の後始末（`src-tauri` の `EXIT_CLEANUP_BUDGET` = 5 秒）は**この値
/// より短い**ので、終了経路ではそちらが先に打ち切る - 意図どおり（窓を閉じた
/// 利用者を 30 秒待たせない）。
pub const COLLECT_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 収集エンジンを組み立てるのに要る材料と、外から読める状態。
/// **[`CollectorService`] とライフサイクルタスクの両方が `Arc` で持つ**
/// （[`Collector`] 本体だけはタスクの専有）。
struct CollectorContext {
    /// レジストリ（`plc_connections`/`collection_groups`/`tags`）と
    /// `collect_events` を同居させている、このアプリ唯一の SQLite プール。
    /// [`build_config`] の読み取り元であり、[`EventSink`] の書き込み先でもある。
    pool: SqlitePool,
    /// 時系列ファイル（`banto-tstore`）の置き場。設定 `data.dir` を
    /// [`resolve_data_dir`] で解決したもの。
    data_dir: PathBuf,
    /// ストアのローテーションと現在値の Stale 判定が**同じ「今」**を見るための
    /// 時計。本番は [`SystemClock`]。
    clock: Arc<dyn Clock>,
    /// イベントの二系統出力（live broadcast + `collect_events` テーブル）。
    /// **[`Collector`] ではなくこのサービスが所有する**ので、
    /// [`CollectorService::subscribe_events`] で取った受信ハンドルは
    /// start/stop をまたいで生き続ける（banto-hub の `CollectorManager` が
    /// rebuild をまたいで `EventSink` を使い回しているのと同じ理由）。
    events: EventSink,
    options: CollectorOptions,
    /// 接続タスクが再接続に使うクライアントの生成口。本番は
    /// [`banto_collect::default_client_factory`]（SLMP / Modbus TCP の直結）。
    /// テストだけが差し替える。
    factory: ClientFactory,
    /// **公開ロック（葉）**。**書くのは [`Lifecycle`] タスクだけ**なので、
    /// `Collector` の実体と食い違わない。読み取りはどこからでもよい
    /// （[`CollectorService::state`] / [`CollectorService::current_values`] は
    /// キューを通らない）。
    ///
    /// 状態と現在値ハンドルを**1 本のロックに同居**させている理由は、この
    /// モジュールの doc「ライフサイクルは専用タスクが所有する」の末尾。
    published: Mutex<Published>,
    /// **テスト専用**（製品ビルドにはフィールドごと存在しない）: 起動処理を
    /// 「[`CollectorState::Starting`] を立てた直後・tstore を開く前」で
    /// 止められるゲート。[`COLLECT_OPERATION_TIMEOUT`] の受入条件
    /// （「解決しない起動」で打ち切りが返り、状態が `Starting` のまま残り、
    /// その後タスク側が完了すると追いつく）を、**実時間を待たずに**
    /// 固定するためだけのもの。
    ///
    /// 偽 [`ClientFactory`] では作れない: `Collector::start` が `await` する
    /// のは `TsWriter` を開く往復で、`factory` が呼ばれるのは**その後に
    /// spawn される接続タスクの中**だから（`banto_collect::collector` の
    /// `start_with_client_factory`）。止めたい一点がクライアント生成の手前に
    /// あるので、ここに入れるしかない。
    #[cfg(test)]
    start_gate: Option<Arc<StartGate>>,
}

/// **テスト専用**: [`CollectorContext::start_gate`] の実体。`entered` で
/// 「起動がその一点まで来た」ことを、`release` で「進んでよい」ことを
/// 待ち合わせる。`Notify::notify_one` は待ち手が居なければ permit を
/// 溜めるので、どちらが先でも取りこぼさない（テストモジュールの
/// `StopGate` と同じ作り）。
#[cfg(test)]
#[derive(Default)]
struct StartGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

/// 葉に置いてある「**キューを通らずに読めるもの**」。**書くのは
/// [`Lifecycle`] タスクだけ**で、書き口は [`CollectorContext::publish`] 1 本。
///
/// 2 つを同じ構造体（＝同じロック）に入れているのは、**食い違いを構造で
/// 防ぐ**ため: 別々のロックに分けると「状態は `Running` に上げたが現在値の
/// 公開を忘れた」「停止で状態だけ落として現在値が残った」が書けてしまう。
/// 1 本にして書き口を 1 つに絞れば、**片方だけ書く**コードがそもそも書けない。
struct Published {
    state: CollectorState,
    /// 走っているときだけ `Some`。[`Collector::current_values`] が返す
    /// 共有ハンドル（`Arc<RwLock<..>>` 包み）で、**タスクの外で保持してよい**
    /// （このモジュールの doc「現在値はキューから外して葉に公開する」）。
    current: Option<CurrentValuesHandle>,
}

impl CollectorContext {
    fn state(&self) -> CollectorState {
        self.published
            .lock()
            .expect("collector published lock poisoned")
            .state
            .clone()
    }

    /// 現在値ハンドルのスナップショット。**走っていなければ `None`**。
    fn current(&self) -> Option<CurrentValuesHandle> {
        self.published
            .lock()
            .expect("collector published lock poisoned")
            .current
            .clone()
    }

    /// **状態と現在値を必ず一緒に書く**唯一の口（[`Published`] の doc）。
    /// `current` は「走っているなら `Some`」で、`Running` 以外は必ず `None`。
    fn publish(&self, state: CollectorState, current: Option<CurrentValuesHandle>) {
        let mut published = self
            .published
            .lock()
            .expect("collector published lock poisoned");
        published.state = state;
        published.current = current;
    }
}

/// [`CollectorService`] の実体。`CollectorService` はこれへの `Arc` 1 本だけを
/// 持つので、`Clone` しても**同じ収集エンジン**を指す（デスクトップの
/// コマンドと LAN の REST が別々のエンジンを立てない）。
struct CollectorInner {
    ctx: Arc<CollectorContext>,
    /// ライフサイクルタスクへの 2 本の口。**初回の非同期操作で spawn する**
    /// （[`CollectorService::new`] は `tauri` の `setup` から**ランタイムの
    /// 外**で呼ばれるので、そこで `tokio::spawn` すると panic する）。
    /// `OnceLock` なので二重には立たない。
    channels: OnceLock<Channels>,
}

/// ライフサイクルタスクへの**2 本**の送信口。操作と読み取りでキューを分けて
/// いるので、[`OnceLock`] に入れるのはこの組（片方だけ先に作られる、という
/// 状態を作らないため）。分けた理由は [`READ_QUEUE_DEPTH`]。
struct Channels {
    /// 開始・停止・再起動（[`COMMAND_QUEUE_DEPTH`]・`send_timeout`）。
    commands: mpsc::Sender<Command>,
    /// 接続状態の読み出し（[`READ_QUEUE_DEPTH`]・`try_send`）。
    reads: mpsc::Sender<ReadCommand>,
}

/// 収集エンジンのサービス層。`src-tauri` の `collect_*` コマンドと
/// `crate::rest` の `/api/collect*` ルーターが共有する - 他のサービスと同じ
/// 「service 層は tauri も axum も知らない」規約。
#[derive(Clone)]
pub struct CollectorService {
    inner: Arc<CollectorInner>,
}

impl CollectorService {
    /// 収集サービスを組み立てる。**この時点では何も起動しない**
    /// （`data_dir` も作らない。実際に開くのは [`Self::start`] の中の
    /// `TsWriter`）。
    ///
    /// `clock` を引数に取らないのは、`src-tauri`（このアプリ自身の新規依存を
    /// 持たない、という invariant）が [`SystemClock`] を名指しできないため。
    /// 本番の時計は 1 つしかないので、差し替えはテスト専用の
    /// `new_for_test` に閉じている。
    pub fn new(pool: SqlitePool, data_dir: PathBuf) -> Self {
        Self::build(
            pool.clone(),
            data_dir,
            Arc::new(SystemClock),
            CollectorOptions::default(),
            banto_collect::default_client_factory(),
        )
    }

    fn build(
        pool: SqlitePool,
        data_dir: PathBuf,
        clock: Arc<dyn Clock>,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self::from_context(CollectorContext {
            pool,
            data_dir,
            clock,
            events,
            options,
            factory,
            published: Mutex::new(Published {
                state: CollectorState::Stopped,
                current: None,
            }),
            #[cfg(test)]
            start_gate: None,
        })
    }

    fn from_context(ctx: CollectorContext) -> Self {
        Self {
            inner: Arc::new(CollectorInner {
                ctx: Arc::new(ctx),
                channels: OnceLock::new(),
            }),
        }
    }

    /// 起動時の自動開始（docs/r1-plan.md の R1-C「起動時に build_config →
    /// start」）。`src-tauri` の `setup()` と `banto-serve` の `main()` が
    /// **共通で通る唯一の口**で、どちらも `spawn` して投げっぱなしにする
    /// （`crate::hub` の `resume()` と同じ形 - 起動を待たせない）。
    ///
    /// **失敗しても起動は止めない**。握り潰しているように見えるが理由は
    /// 捨てていない - [`CollectorState::StartFailed`] に残るので
    /// `collect_status` / `GET /api/collect` から必ず見える。**収集対象 0 件は
    /// 失敗ではない**（[`CollectorState::NoTargets`]）ので何も言わない。
    ///
    /// 打ち切り（[`CollectOutcome::pending`]）も失敗ではないので、
    /// 「まだ終わっていない」とだけ言う。
    ///
    /// **レジストリが変わってもここは二度と呼ばれない** - 反映は明示的な
    /// [`Self::restart`] だけ（理由はこのモジュールの doc）。
    pub async fn autostart(&self) {
        match self.start().await {
            Ok(outcome) if outcome.pending => eprintln!(
                "banto: 起動時の収集の開始が{}秒以内に終わりませんでした（開始処理は続いています。状態は収集の状態表示で確認してください）",
                COLLECT_OPERATION_TIMEOUT.as_secs()
            ),
            Ok(_) => {}
            Err(err) => eprintln!("banto: 起動時の収集の開始に失敗しました: {err}"),
        }
    }

    /// 収集を開始する。
    ///
    /// 1. レジストリから構成を作る（[`build_config`]）。
    /// 2. **有効タグが 0 件なら [`CollectorState::NoTargets`]**（`Ok`）。
    ///    `Collector::start` には渡さない - このモジュール doc 参照。
    /// 3. それ以外は [`Collector`] を起動して [`CollectorState::Running`]。
    ///
    /// **既に走っているときは何もしない**（現在の状態をそのまま返す）。
    /// 二重起動を防いでいるのはロックではなく、ライフサイクルタスクが
    /// コマンドを**逐次**処理すること（このモジュール doc 参照）。
    ///
    /// `Err` を返すのは「起動を試みて失敗した」ときだけ。そのとき状態は
    /// [`CollectorState::StartFailed`] になり、**理由が残る**。
    ///
    /// [`COLLECT_OPERATION_TIMEOUT`] で待つのをやめた場合は `Err` ではなく
    /// [`CollectOutcome::pending`] が立つ（**打ち切りは失敗ではない**）。
    /// そのとき状態は [`CollectorState::Starting`] のままで、起動処理は
    /// タスク側で続いている。
    pub async fn start(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Start).await
    }

    /// 収集を停止する。**走っていなければ何もしない**（冪等）- 状態にも
    /// 触らないので、[`CollectorState::NoTargets`] や
    /// [`CollectorState::StartFailed`] の理由が `stop()` で消えることはない。
    ///
    /// `Err` は「最終 flush に失敗した」ときだけ返る。その場合でも
    /// **エンジンは確かに止まっている**ので状態は
    /// [`CollectorState::Stopped`] にする（状態は現実を写す）。
    ///
    /// **この `await` を途中でやめても停止は進む**（依頼はもうタスク側にある）。
    /// 終了フックの 5 秒予算が切れたときに起こるのがまさにこれで、待つのを
    /// やめるだけで join と最終 flush は続く。[`COLLECT_OPERATION_TIMEOUT`]
    /// で打ち切った場合も同じで、[`CollectOutcome::pending`] が立つだけ。
    pub async fn stop(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Stop).await
    }

    /// 停止してから開始する。**間に他の `start`/`stop` を割り込ませない** -
    /// タスク側で「停止 → 開始」を 1 つのコマンドとして処理するので、
    /// `stop().await` → `start().await` と 2 回に分けたときのような隙間が
    /// そもそも無い。
    ///
    /// 停止側の失敗（最終 flush）は**開始を中止する理由にしない** - ログに
    /// 出して開始へ進む。落ちるのは旧ファイルの未 flush 分だけで、それは
    /// 「新しい構成で収集を再開できるか」とは無関係だから（`banto-collect` の
    /// `apply_config` が同じ天秤で同じ側を選んでいる）。
    ///
    /// **レジストリ（接続・グループ・タグ）の変更を収集へ反映する唯一の口**
    /// でもある - CRUD が自動でこれを呼ぶことはしない（理由はこのモジュールの
    /// doc「起動時の自動開始と、「収集を再起動」だけが反映の口であること」）。
    pub async fn restart(&self) -> Result<CollectOutcome, BantoError> {
        self.lifecycle(Command::Restart).await
    }

    /// 現在の状態（**内部・診断用**）。**ネットワークもディスクも DB も
    /// 触らない**し、コマンドキューも通らないので、起動処理の最中でも
    /// 待たされない。
    ///
    /// [`CollectorState::StartFailed`] の `reason` を**持ったまま**返すので、
    /// **これをそのまま外へ出さないこと** - 状態の取得は必ず
    /// [`Self::state_view`] を通す（このモジュールの doc「内部の状態と、
    /// 状態取得で公開する形を分ける」）。
    pub fn state(&self) -> CollectorState {
        self.inner.ctx.state()
    }

    /// 現在の状態の**公開用の形**（[`CollectorStateView`]）。
    ///
    /// **状態を外へ返す経路は、REST（`GET /api/collect`）も Tauri
    /// （`collect_status`）も必ずここを通る** - 変換を 1 箇所に集めて、
    /// 経路によって公開する情報が割れないようにするため（#407 レビュー
    /// P2-2。[`COLLECT_AUDIT_RESOURCE`] や床の定数と同じ作法）。
    ///
    /// [`Self::state`] と同じく、ネットワークもディスクも DB もキューも
    /// 通らないので、画面はこれをポーリングしてよい。
    pub fn state_view(&self) -> CollectorStateView {
        CollectorStateView::from(&self.state())
    }

    /// 接続ごとの状態のスナップショット（**内部型のまま**）。
    ///
    /// * [`Readout::NotRunning`] - **タスクが「走っていない」と答えた**場合
    ///   だけ。空の `HashMap` を返して「接続 0 件」と混同させない
    ///   （docs/implementation-checklist.md §5）。
    /// * [`Readout::Unavailable`] - **読み取り用キューに枠が無かった**
    ///   （待たずに即決）、[`COLLECT_READ_TIMEOUT`] までに答えが返らなかった、
    ///   または**ライフサイクルタスクが居なくなっていた**（チャネルが閉じて
    ///   いる / 応答の送信側が drop された）。最後のものを「走っていない」に
    ///   しないのが #408 レビューの直し - 理由はこのモジュールの doc
    ///   「タスクが居ないことを『走っていない』と答えない」。
    /// * [`Readout::Ready`] - 答えが返った（0 件を含む）。
    ///
    /// ライフサイクルタスクに問い合わせるので、返るのは必ず「どれかの操作と
    /// 操作の間」の姿（操作の途中の [`Collector`] は見えない）。
    ///
    /// **投入は [`READ_QUEUE_DEPTH`] の読み取り専用チャネルへ `try_send`**
    /// （#408 レビュー P2-1）。操作用キューには**一切触らない**ので、
    /// この口をいくらポーリングしても `stop()` / `restart()` の投入を
    /// 締め出せない。枠が無いときに待たないのは、待てば結局
    /// [`COLLECT_READ_TIMEOUT`] で打ち切って同じ答えになるうえ、
    /// **待っている間に投入できてしまうと未処理の要求が積み上がる**から。
    ///
    /// 公開用の型に詰め替えて外へ出すのは [`Self::connections`] の側。
    async fn connection_status(&self) -> Readout<HashMap<String, ConnectionStatus>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        match self
            .reads()
            .try_send(ReadCommand::ConnectionStatus(reply_tx))
        {
            Ok(()) => {}
            // 枠が無い = 直前の読み取りがまだ取り出されていない
            // （タスクが長い操作を抱えている）。**待たない。**
            Err(mpsc::error::TrySendError::Full(_)) => return Readout::Unavailable,
            // タスクが居ない（= panic した。通常は起こらない）。**「走って
            // いない」とは答えない** - 収集が止まった確証が無いどころか、
            // 接続タスクは生き残っている公算が大きい（このモジュールの doc
            // 「タスクが居ないことを「走っていない」と答えない」）。
            Err(mpsc::error::TrySendError::Closed(_)) => return Readout::Unavailable,
        }
        match tokio::time::timeout(COLLECT_READ_TIMEOUT, reply_rx).await {
            // **`NotRunning` を返すのはここだけ** - タスクが実際に
            // `self.collector` を見て「持っていない」と答えた場合。
            Ok(Ok(Some(statuses))) => Readout::Ready { data: statuses },
            Ok(Ok(None)) => Readout::NotRunning,
            // 応答の送信側が落ちた = タスクが居なくなった（上と同じ扱い）。
            Ok(Err(_reply_dropped)) => Readout::Unavailable,
            // 打ち切ったのは**この 1 回の待ち**だけ。依頼はキューに残って
            // いて、タスクが手空きになれば処理される（誰も受け取らないだけ）。
            // **枠が空くのはそのとき**で、ここでは返さない。
            Err(_elapsed) => Readout::Unavailable,
        }
    }

    /// 現在値キャッシュのハンドル。**走っていなければ `None`**（同上 -
    /// 「値がまだ無い」キャッシュを返して「0 件」に潰さない）。
    ///
    /// **同期**（C-3a）: 葉に公開してあるハンドルを `clone` するだけで、
    /// コマンドキューもディスクも触らない - **遅い `start()` の後ろで
    /// 待たされない**（このモジュールの doc「現在値はキューから外して葉に
    /// 公開する」）。ポーリングで引いてよい。
    pub fn current_values(&self) -> Option<CurrentValuesHandle> {
        self.inner.ctx.current()
    }

    // --- C-3a: 外向きの 3 つの読み出し口 ---------------------------------

    /// **現在値**（`GET /api/collect/values` / `collect_values`）。
    ///
    /// * 走っていない → [`Readout::NotRunning`]、
    /// * 走っている → [`Readout::Ready`]（**まだ 1 度も読めていなければ空の
    ///   `HashMap`** = 「0 件」という事実）。
    ///
    /// **[`Readout::Unavailable`] は返らない**（待つ相手が居ないので打ち切る
    /// ものが無い）。キーは `tag:<id>`。
    pub fn values(&self) -> Readout<HashMap<String, CurrentSampleView>> {
        values_readout(self.current_values().map(|handle| handle.snapshot()))
    }

    /// **接続状態**（`GET /api/collect/connections` / `collect_connections`）。
    ///
    /// 3 つの結末すべてを返しうる唯一の口:
    ///
    /// * 走っていない → [`Readout::NotRunning`]、
    /// * [`COLLECT_READ_TIMEOUT`] 以内にライフサイクルタスクが答えなかった →
    ///   [`Readout::Unavailable`]（**「失敗」でも「0 件」でもない**）、
    /// * 答えた → [`Readout::Ready`]。
    ///
    /// キューを通るのは、`banto-collect` が接続状態のハンドルを外に出して
    /// いないから（このモジュールの doc「現在値はキューから外して葉に公開
    /// する」の末尾）。ただし**通るのは読み取り専用のキュー**で、操作用の
    /// キューには触らない（#408 レビュー P2-1。[`READ_QUEUE_DEPTH`]）。
    /// キーは `conn:<id>`。
    pub async fn connections(&self) -> Readout<HashMap<String, ConnectionStatusView>> {
        match self.connection_status().await {
            Readout::NotRunning => Readout::NotRunning,
            Readout::Unavailable => Readout::Unavailable,
            Readout::Ready { data } => connections_readout(Some(data)),
        }
    }

    /// **収集イベント一覧**（`GET /api/collect/events` /
    /// `collect_events_list`）: `collect_events` の 1 ページを**新しい順**で
    /// 返す。総件数は [`CollectEventList::total_count`]（`crate::audit` の一覧と
    /// 同じ綴り）。
    ///
    /// * 読めた → [`Readout::Ready`]（**0 件でも `Ready`**）、
    /// * 読めなかった（DB エラー / [`COLLECT_READ_TIMEOUT`] で打ち切り）→
    ///   [`Readout::Unavailable`]。
    ///
    /// **[`Readout::NotRunning`] は返らない** - イベントは過去の記録なので、
    /// 収集が止まっていても読める（このモジュールの doc「イベント一覧だけ
    /// 「走っていない」を返さない」）。
    ///
    /// **読めなかった理由は返さない**（`Err` にもしない）: この口は `viewer`
    /// にも開いていて、`sqlx` のエラー文言は DB ファイルのパスを含みうる -
    /// C-2 の P2（`StartFailed.reason` の漏れ）と同じ類なので、理由はサーバー
    /// 側のログにだけ出す。
    ///
    /// **スナップショット境界**（#409 レビュー P2-2。[`EventPage`] の
    /// `as_of_id`）: 件数も行も `id <= as_of_id` で絞る。未指定なら
    /// **その時点の最大 `id`** を境界にし、使った境界を
    /// [`CollectEventList::as_of_id`] で返す。この境界が集合のメンバーを
    /// 確定できるのは、`collect_events.id` が **`AUTOINCREMENT`**
    /// （`banto-collect` の `0001_collect_events.sql`）で、単調増加かつ削除
    /// されても再利用されないから - `ts` の並びとは無関係に「境界より後に
    /// 足された行」は必ず境界より大きい `id` を持つ。
    ///
    /// **「件数が増えたら全体を取り直す」方式にしなかった理由**: イベントが
    /// 流れ続けている間（接続が瞬断を繰り返しているなど、まさにこの一覧を
    /// 見たいとき）は毎回取り直しになり、**一覧が永久に読み終わらない** -
    /// 「回復導線が、必要なときだけ死ぬ」型になる。境界を固定すれば、新しい
    /// イベントは利用者が「再読み込み」した（新しい世代を始めた）ときに入る。
    ///
    /// **将来の制約**: 保持期間による削除（R0 §3.4、**まだ未実装**）が入ると、
    /// 古い行（小さい `id`）が消えて、境界を固定していても**末尾側の
    /// `OFFSET` がずれる**（数え直した件数も減る）。削除を実装するときは、
    /// この一覧の取得方法を見直すこと。今は何も消さないので、この問題は
    /// 起きない。
    pub async fn events(&self, page: EventPage) -> Readout<CollectEventList> {
        match tokio::time::timeout(COLLECT_READ_TIMEOUT, self.read_events(page)).await {
            Ok(Ok(result)) => Readout::Ready { data: result },
            Ok(Err(err)) => {
                eprintln!("banto: 収集イベントの読み出しに失敗しました: {err}");
                Readout::Unavailable
            }
            Err(_elapsed) => {
                eprintln!(
                    "banto: 収集イベントの読み出しが{}秒以内に終わりませんでした",
                    COLLECT_READ_TIMEOUT.as_secs()
                );
                Readout::Unavailable
            }
        }
    }

    /// [`Self::events`] の DB 側。**`SELECT` に `detail` が無い**のは意図で、
    /// あの列は接続先やファイルパスを含みうる自由文（このモジュールの doc
    /// 「公開用の型 - 何を載せ、何を落としたか」）。
    ///
    /// 並び順は `ts DESC, id DESC` 固定 - `ts` は同じミリ秒に複数行が並びうる
    /// ので、`id`（`AUTOINCREMENT` = 挿入順）で必ず一意に決める。ここが
    /// 曖昧だとページ境界で行が重複・欠落する。
    ///
    /// **境界の決定・行・件数を 1 つの読み取りトランザクションで行う**
    /// （#409 レビュー P2-2）: 境界を決めてから数えるまでの間に行が足されても、
    /// 3 つの問い合わせは同じデータ集合を見る。加えて行も件数も
    /// `id <= as_of_id` で絞っているので、境界より後の行はどちらにも入らない。
    async fn read_events(&self, page: EventPage) -> Result<CollectEventList, sqlx::Error> {
        let mut tx = self.inner.ctx.pool.begin().await?;
        // 表が空なら MAX(id) は NULL → 境界 0（どの行も含まない。
        // [`CollectEventList`] の doc）。
        let as_of_id = match page.as_of_id {
            Some(id) => id,
            None => sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM collect_events")
                .fetch_one(&mut *tx)
                .await?
                .unwrap_or(0),
        };
        let rows = sqlx::query_as::<_, CollectEventRow>(
            "SELECT id, ts AS ts_ms, kind, connection_key, tag_key, level, value \
             FROM collect_events WHERE id <= ? ORDER BY ts DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(as_of_id)
        .bind(page.limit as i64)
        .bind(page.offset as i64)
        .fetch_all(&mut *tx)
        .await?;
        // 総件数は**ページングの前**の件数（`crate::audit::AuditLogService::list`
        // と同じ形）で、**同じ境界で絞る**。画面のページャがこれを使う。
        let total_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collect_events WHERE id <= ?")
                .bind(as_of_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(CollectEventList {
            rows,
            total_count: total_count as u64,
            as_of_id,
        })
    }

    /// 収集イベントの live 購読。
    ///
    /// ここだけ `Option` ではないのは、[`EventSink`] を**[`Collector`] では
    /// なくこのサービスが所有している**から: 購読者は start/stop をまたいで
    /// 同じ受信ハンドルを使い続けられ、`collection_started` を取りこぼさない。
    /// 「イベントが流れてこない」＝「走っていない」ではないので、走っているか
    /// どうかは [`Self::state`] を見ること。
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.inner.ctx.events.subscribe()
    }

    // --- ライフサイクルタスクへの依頼 ------------------------------------

    /// ライフサイクルタスクへの 2 本の口。**初回の呼び出しで spawn する** -
    /// [`Self::new`] は `tauri` の `setup`（tokio ランタイムの外）から呼ばれる
    /// ので、そこで `tokio::spawn` はできない。ここへ来る経路は全部 `async fn`
    /// の中なので、必ずランタイムの上にいる。
    ///
    /// **2 本まとめて作る**（[`Channels`]）: 操作用と読み取り用を別々に
    /// 遅延生成すると、タスクが片方だけを受け取った状態が作れてしまう。
    fn channels(&self) -> &Channels {
        self.inner.channels.get_or_init(|| {
            let (tx, rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
            let (read_tx, read_rx) = mpsc::channel(READ_QUEUE_DEPTH);
            let lifecycle = Lifecycle {
                ctx: self.inner.ctx.clone(),
                collector: None,
            };
            tokio::spawn(lifecycle.run(rx, read_rx));
            Channels {
                commands: tx,
                reads: read_tx,
            }
        })
    }

    /// **操作用**の送信口（[`COMMAND_QUEUE_DEPTH`]）。
    fn commands(&self) -> &mpsc::Sender<Command> {
        &self.channels().commands
    }

    /// **読み取り用**の送信口（[`READ_QUEUE_DEPTH`]）。閲覧のポーリングが
    /// 操作の投入を締め出さないよう、操作用と資源を共有しない。
    fn reads(&self) -> &mpsc::Sender<ReadCommand> {
        &self.channels().reads
    }

    /// `start`/`stop`/`restart` 共通の往復。3 つとも同じ 1 本を通る
    /// （ここが「操作の口を生やしたのに無応答で固まる」を塞ぐ唯一の場所）。
    ///
    /// **2 段階で、上限も 2 本**（#407 レビュー P2-1。一本化しないこと）:
    ///
    /// 1. **投入** - [`COLLECT_ENQUEUE_TIMEOUT`] 付きの `send_timeout`。
    ///    入らなかったら [`ENQUEUE_BUSY_MESSAGE`] の **`Err`**。ここで返る
    ///    ということは**依頼がどこにも届いていない**（タスクはこの操作を
    ///    知らないまま）ので、「受け付けられませんでした」と**言い切ってよい** -
    ///    `pending`（＝後から必ず実行される）にしてはいけない。
    /// 2. **完了待ち** - 投入に**成功した後**だけ [`COLLECT_OPERATION_TIMEOUT`]。
    ///    ここで打ち切ったときは `Err` にせず [`CollectOutcome::pending`] を
    ///    立て、**その時点の [`Self::state`]**（キューを通らない同期読み取り）を
    ///    載せて返す。依頼はもうタスク側にあるので、処理は最後まで進む。
    ///
    /// 旧実装は 1 本の上限で投入ごと包んでいたため、**キューが満杯で投げられ
    /// なかった操作まで `pending: true` で返し**、REST / Tauri はそれを
    /// `result: "ok"` で監査に残していた（モジュール doc「「受け付けた」とも
    /// 言い切らない」に再現手順）。
    ///
    /// タスクが居なくなっていたら（= panic した。通常は起こらない）
    /// **黙って成功にしない** - 投入時（`Closed`）も応答待ち中（送信側 drop）も
    /// [`LIFECYCLE_GONE_MESSAGE`] の `Err`。ここは C-2 から変えていない。
    async fn lifecycle(
        &self,
        make: fn(oneshot::Sender<Result<CollectorState, BantoError>>) -> Command,
    ) -> Result<CollectOutcome, BantoError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        // --- 1. 投入（ここで返る = 何も起きていない）---
        match self
            .commands()
            .send_timeout(make(reply_tx), COLLECT_ENQUEUE_TIMEOUT)
            .await
        {
            Ok(()) => {}
            Err(mpsc::error::SendTimeoutError::Timeout(_)) => {
                return Err(BantoError::Other(ENQUEUE_BUSY_MESSAGE.to_string()))
            }
            Err(mpsc::error::SendTimeoutError::Closed(_)) => {
                return Err(BantoError::Other(LIFECYCLE_GONE_MESSAGE.to_string()))
            }
        }
        // --- 2. 完了待ち（ここから先、依頼は必ず実行される）---
        match tokio::time::timeout(COLLECT_OPERATION_TIMEOUT, reply_rx).await {
            Ok(Ok(result)) => result.map(CollectOutcome::settled),
            Ok(Err(_reply_dropped)) => Err(BantoError::Other(LIFECYCLE_GONE_MESSAGE.to_string())),
            Err(_elapsed) => Ok(CollectOutcome::still_working(self.state())),
        }
    }

    /// テスト専用の組み立て口: 時計・チューニング・クライアント生成口を
    /// 差し替える。本番の [`Self::new`] を 1 本に保ったまま、テストが
    /// 短いタイムアウトと偽クライアントを使えるようにするためだけのもの。
    #[cfg(test)]
    fn new_for_test(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        Self::build(pool, data_dir, Arc::new(SystemClock), options, factory)
    }

    /// **テスト専用**: [`Lifecycle`] タスクが**居なくなった**状態
    /// （= タスクが panic した後）を作る。2 本のチャネルを自前で作って
    /// **受信側だけ落とし**、送信側を [`CollectorInner::channels`] に
    /// 入れてしまうので、以降の `try_send` / `send_timeout` は必ず
    /// `Closed` になる。
    ///
    /// **タスクを spawn して本当に panic させる形は採らない**: そのためには
    /// panic する分岐を製品コードに用意することになり、`#[cfg(test)]` の
    /// 有無に関わらず「panic させる口」が増える。ここで再現したいのは
    /// 「**送信口は生きているが、受け手が居ない**」という観測可能な状態
    /// そのものなので、チャネルの形で直接作るのが最小。
    ///
    /// 葉（[`CollectorContext::published`]）には**何も書かない** - 呼び出し側の
    /// テストが「タスクが消える直前の状態」を自分で公開する。
    #[cfg(test)]
    fn new_for_test_without_lifecycle_task(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Self {
        let svc = Self::build(pool, data_dir, Arc::new(SystemClock), options, factory);
        let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE_DEPTH);
        let (reads, read_rx) = mpsc::channel(READ_QUEUE_DEPTH);
        // **受け手を落とす** = タスクが消えた、と同じ観測になる。
        drop(command_rx);
        drop(read_rx);
        if svc
            .inner
            .channels
            .set(Channels { commands, reads })
            .is_err()
        {
            panic!("組み立て直後なのにチャネルが既に入っている");
        }
        svc
    }

    /// [`Self::new_for_test`] と同じだが、起動処理を [`StartGate`] で
    /// 止められる（[`CollectorContext::start_gate`] の doc 参照）。
    #[cfg(test)]
    fn new_for_test_gated(
        pool: SqlitePool,
        data_dir: PathBuf,
        options: CollectorOptions,
        factory: ClientFactory,
        gate: Arc<StartGate>,
    ) -> Self {
        let events = EventSink::new(pool.clone());
        Self::from_context(CollectorContext {
            pool,
            data_dir,
            clock: Arc::new(SystemClock),
            events,
            options,
            factory,
            published: Mutex::new(Published {
                state: CollectorState::Stopped,
                current: None,
            }),
            start_gate: Some(gate),
        })
    }
}

/// 走っている [`Collector`] を**専有する**タスクの中身。
///
/// [`CollectorService`] のどのメソッドもここへコマンドを送るだけなので、
/// `Collector` はこの構造体の外に一度も出ない（`Arc` にも `Mutex` にも
/// 入っていない）。状態を書くのもここだけ。
struct Lifecycle {
    ctx: Arc<CollectorContext>,
    /// `None` = 走っていない。[`Collector::stop`] が `self` を消費するので
    /// `Option` + `take()`。
    collector: Option<Collector>,
}

impl Lifecycle {
    /// コマンドを**逐次**処理する。1 つのコマンドを処理している間、次は
    /// 受け取らない - これが「新しい `start()` が前の `stop()` の完了前に
    /// 進まない」の全てで、そのためのロックは 1 つも要らない。
    ///
    /// **口は 2 本**（#408 レビュー P2-1）: 操作（[`Command`]）と読み取り
    /// （[`ReadCommand`]）。逐次処理であることは変わらない - 変わったのは
    /// 「**どちらのキューが埋まっても、もう一方の投入を妨げない**」こと
    /// （[`READ_QUEUE_DEPTH`] の doc に、共有していた頃に起きていたことを
    /// 書いてある）。
    ///
    /// `biased` で**操作を優先**する。読み取りが後回しになっても、呼び出し側は
    /// [`COLLECT_READ_TIMEOUT`] で打ち切って [`Readout::Unavailable`]
    /// （「今は読めませんでした」）と正しく畳めるが、操作の側には
    /// そういう逃げ道が無い（停止は実行されなければならない）。
    ///
    /// [`CollectorService`] が全部 drop されると `Sender` が落ち、この
    /// ループが抜けてタスクが終わる（2 本は [`Channels`] に同居しているので
    /// 一緒に落ちる）。そのとき走っている [`Collector`] は
    /// `stop()` されずに drop される（接続タスクは切り離される）が、これは
    /// サービスごと捨てられる場面 = プロセス終了時だけで、正規の終了経路は
    /// `shutdown_app_state` が明示的に [`CollectorService::stop`] を通る。
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut reads: mpsc::Receiver<ReadCommand>,
    ) {
        loop {
            tokio::select! {
                // 操作を先に見る（上の doc）。
                biased;
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        // `send` の `Err`（呼び出し側が待つのをやめた）は捨てる。
                        // **処理そのものはもう終わっている**ので、誰も受け取らなく
                        // ても状態と実体は一致している。
                        Command::Start(reply) => {
                            let _ = reply.send(self.start().await);
                        }
                        Command::Stop(reply) => {
                            let _ = reply.send(self.stop().await);
                        }
                        Command::Restart(reply) => {
                            let _ = reply.send(self.restart().await);
                        }
                    }
                }
                read = reads.recv() => {
                    let Some(read) = read else { break };
                    match read {
                        // **枠が空くのはここ**（要求を取り出した瞬間）。
                        // 呼び出し側が待つのをやめても枠は返らない
                        // （[`READ_QUEUE_DEPTH`] の doc）。
                        ReadCommand::ConnectionStatus(reply) => {
                            let _ = reply.send(self.collector.as_ref().map(|c| c.status()));
                        }
                    }
                }
            }
        }
    }

    async fn start(&mut self) -> Result<CollectorState, BantoError> {
        // 二重起動の防止。逐次処理なので「見た直後に誰かが起動していた」は
        // 起こらない。
        if self.collector.is_some() {
            return Ok(self.ctx.state());
        }

        // **tstore を開く前に**「起動中」を立てる。呼び出し側が
        // [`COLLECT_OPERATION_TIMEOUT`] で待つのをやめたあと `state()` が
        // `Stopped` を返すと嘘になる（起動処理はここで続いている）ため -
        // このモジュールの doc「無応答への上限」。ここから必ず
        // `Running` / `NoTargets` / `StartFailed` のどれかへ抜ける。
        //
        // 現在値は**まだ公開しない**（`None`）。エンジンはこれから開くので、
        // 「起動中」に現在値が読めたら嘘になる - C-3a の読み出しはこのとき
        // `Readout::NotRunning` と答える。
        self.ctx.publish(CollectorState::Starting, None);

        // **テスト専用**の一時停止点。製品ビルドにはこのブロックごと
        // 存在しない（`CollectorContext::start_gate` の doc）。
        #[cfg(test)]
        if let Some(gate) = self.ctx.start_gate.clone() {
            gate.entered.notify_one();
            gate.release.notified().await;
        }

        let config = match build_config(&self.ctx.pool).await {
            Ok(config) => config,
            Err(err) => return Err(self.fail_start(err)),
        };

        // 「収集対象なし」は **`Collector::start` に渡す前に**分岐する。
        // 渡すと `CollectError::Config` になり、本物の構成エラー（アドレスが
        // 解釈できない等）と同じ入れ物に入ってしまう。**数えるのはタグ** -
        // `build_config` はタグが空の有効グループも計画に残すので、
        // `group_count()` では「有効グループ 1・有効タグ 0」を素通りさせて
        // しまう（#406 レビュー P2。このモジュール doc 参照）。
        if config.tag_count() == 0 {
            self.ctx.publish(CollectorState::NoTargets, None);
            return Ok(CollectorState::NoTargets);
        }

        let groups = config.group_count();
        let tags = config.tag_count();
        let collector = match Collector::start_with_client_factory(
            config,
            &self.ctx.data_dir,
            self.ctx.clock.clone(),
            self.ctx.events.clone(),
            self.ctx.options,
            self.ctx.factory.clone(),
        )
        .await
        {
            Ok(collector) => collector,
            Err(err) => return Err(self.fail_start(err)),
        };

        // **成功を確かめてから**状態を上げる（先に Running にして失敗時に
        // 降ろす、という順序にしない - docs/implementation-checklist.md §6
        // の「失敗経路での状態の落とし方」）。
        //
        // 現在値ハンドルの**公開はここ**（C-3a）。`Running` と同じ
        // `publish` 呼び出しで書くので、「走っているのに現在値が読めない」
        // という食い違いが起こらない（[`Published`] の doc）。
        let current = collector.current_values();
        self.collector = Some(collector);
        let state = CollectorState::Running { groups, tags };
        self.ctx.publish(state.clone(), Some(current));
        Ok(state)
    }

    async fn stop(&mut self) -> Result<CollectorState, BantoError> {
        let Some(collector) = self.collector.take() else {
            // 走っていない: **何もしない**。状態も触らない（`NoTargets` /
            // `StartFailed` の理由を握り潰さないため）。
            return Ok(self.ctx.state());
        };

        // ここから先は誰にも中断されない（呼び出し側が消えてもこのタスクは
        // 生きている）ので、`take()` 済み・状態は `Running` のまま、という
        // 食い違いが残ることはない。
        let result = collector.stop().await;
        // 止まったことは確定なので、flush の成否に関わらず `Stopped` にする。
        // **現在値ハンドルの取り下げもここ**（C-3a）- 停止が実体まで終わって
        // から降ろすので、停止処理の最中はまだ最後の値が読める（止まったのに
        // 読める、という逆の嘘にはならない）。
        self.ctx.publish(CollectorState::Stopped, None);
        match result {
            Ok(()) => Ok(CollectorState::Stopped),
            Err(err) => Err(collect_error(err)),
        }
    }

    async fn restart(&mut self) -> Result<CollectorState, BantoError> {
        if let Err(err) = self.stop().await {
            eprintln!(
                "banto: 収集の再起動中、停止側の後始末に失敗しました（開始は続行します）: {err}"
            );
        }
        self.start().await
    }

    /// 起動の失敗を状態に焼き付けて、同じ理由を `Err` として返す。
    /// **[`CollectError`] の文言をそのまま捨てない**。
    fn fail_start(&self, err: CollectError) -> BantoError {
        let reason = err.to_string();
        self.ctx.publish(
            CollectorState::StartFailed {
                reason: reason.clone(),
            },
            None,
        );
        collect_error_with_reason(err, reason)
    }
}

/// 現在値の「走っていない / 読めて 0 件 / 読めて N 件」の言い分け（**純関数**）。
///
/// [`CollectorService::values`] から切り出してあるのは、**状態の総当たりを
/// 表で固定する**ため（docs/implementation-checklist.md §5「判断は純関数に
/// 出して、状態の総当たりを表でテストする」）。`None` = 走っていない、
/// `Some(空)` = 走っているがまだ 1 件も読めていない（**0 件という事実**）。
fn values_readout(
    snapshot: Option<HashMap<String, CurrentSample>>,
) -> Readout<HashMap<String, CurrentSampleView>> {
    match snapshot {
        None => Readout::NotRunning,
        Some(samples) => Readout::Ready {
            data: samples
                .iter()
                .map(|(key, sample)| (key.clone(), CurrentSampleView::from(sample)))
                .collect(),
        },
    }
}

/// 接続状態の同じ言い分け（**純関数**）。`None` = **タスクが「走っていない」と
/// 答えた**、`Some(空)` = 走っているが接続タスクがまだ 1 件も状態を書いて
/// いない。
///
/// **「読めなかった」はここには来ない** - 打ち切りも**タスクが居ない**場合も
/// [`CollectorService::connection_status`] の側で [`Readout::Unavailable`] に
/// なる（#408 レビュー。このモジュールの doc「タスクが居ないことを「走って
/// いない」と答えない」）。ここは「**答えが返ってきた**」あとの言い分けだけを
/// 担う。
fn connections_readout(
    status: Option<HashMap<String, ConnectionStatus>>,
) -> Readout<HashMap<String, ConnectionStatusView>> {
    match status {
        None => Readout::NotRunning,
        Some(statuses) => Readout::Ready {
            data: statuses
                .iter()
                .map(|(key, status)| (key.clone(), ConnectionStatusView::from(status)))
                .collect(),
        },
    }
}

/// [`CollectError`] をこのアプリの共通エラー型へ。`Registry` は元々
/// [`BantoError`] なのでそのまま戻し（検証エラーの `field_errors` を
/// 文字列に潰さない）、それ以外は文言を保つ。
fn collect_error(err: CollectError) -> BantoError {
    let reason = err.to_string();
    collect_error_with_reason(err, reason)
}

fn collect_error_with_reason(err: CollectError, reason: String) -> BantoError {
    match err {
        CollectError::Registry(inner) => inner,
        _ => BantoError::Other(reason),
    }
}

/// 設定 `data.dir` を実際のディレクトリへ解決する。
///
/// 既定値は banto-hub に倣った相対パス `"./data"` なので、そのまま使うと
/// **プロセスの作業ディレクトリ**（デスクトップアプリでは何であるか分からない
/// 場所）に時系列ファイルを作ってしまう。そこで**相対パスは `base`
/// （アプリのデータディレクトリ）からの相対**として解決する。絶対パスが
/// 設定されていればそれをそのまま使う（運用で外付けドライブを指したい、が
/// この設定の主目的）。
///
/// 空文字・空白だけは「未設定」とみなし、既定の `"./data"` と同じ扱いにする。
pub fn resolve_data_dir(base: &Path, configured: &str) -> PathBuf {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return base.join("data");
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use crate::test_support::TempDir;
    use banto_collect::BackoffConfig;
    use banto_plc::{BoxFuture, PlcClient, PlcError, ReadRequest, ReadResult, TagValue};
    use banto_tags::{
        CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
        TagInput, TagService,
    };
    use banto_tstore::WriterOptions;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    /// 速い・決定的なチューニング。`banto-collect` の `tests/integration.rs`
    /// の `fast_options` と同じ意図（**テストの中で実時間を待たない**ための
    /// もので、待ち合わせのための sleep はどのテストにも書いていない）。
    fn fast_options() -> CollectorOptions {
        CollectorOptions {
            backoff: BackoffConfig {
                base: Duration::from_millis(20),
                max: Duration::from_millis(100),
            },
            connect_timeout: Duration::from_millis(100),
            response_timeout: Duration::from_millis(100),
            writer_options: WriterOptions::default(),
        }
    }

    /// 接続しに行かない偽クライアント。**実 PLC を模さない**（一巡の確認は
    /// C-4（`tests/collect_roundtrip.rs` と
    /// `e2e/tests/user-simulator-roundtrip.spec.ts`）のハーネスの仕事）が、
    /// 「収集対象があるときに `Collector` が確かに
    /// 立ち上がる」を実ネットワーク無しで押さえるために使う。
    struct OfflineClient;

    impl PlcClient for OfflineClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(1.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    fn offline_factory() -> ClientFactory {
        Arc::new(|_spec| Box::new(OfflineClient) as Box<dyn PlcClient>)
    }

    /// 停止の途中で**確実に**止まってくれるゲート。接続タスクの graceful
    /// exit は `client.disconnect().await` を通るので、そこを塞ぐと
    /// [`Collector::stop`]（接続タスクの join → 最終 flush）が中で止まる。
    ///
    /// **1 回だけ**効く（`armed`）: 同じテストの中で立て直した 2 本目の
    /// エンジンの後始末まで塞ぐと、テストが終われなくなるため。
    ///
    /// 実時間は一切待たない - 「入った」も「開けた」も [`Notify`] で
    /// 待ち合わせる（`notify_one` は待ち手が居なければ permit を溜めるので、
    /// どちらが先でも取りこぼさない）。
    struct StopGate {
        entered: Notify,
        release: Notify,
        armed: AtomicBool,
    }

    impl StopGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: Notify::new(),
                release: Notify::new(),
                armed: AtomicBool::new(true),
            })
        }

        /// 偽クライアントの `disconnect` から呼ばれる。
        async fn pass(&self) {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
        }

        /// 停止処理が「実体の停止」の途中まで進んだことを待つ。
        async fn wait_entered(&self) {
            self.entered.notified().await;
        }

        fn release(&self) {
            self.release.notify_one();
        }
    }

    /// [`OfflineClient`] と同じだが、切断だけ [`StopGate`] を通る。
    struct GatedClient {
        gate: Arc<StopGate>,
    }

    impl PlcClient for GatedClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(1.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            let gate = self.gate.clone();
            Box::pin(async move { gate.pass().await })
        }
    }

    fn gated_factory(gate: &Arc<StopGate>) -> ClientFactory {
        let gate = gate.clone();
        Arc::new(move |_spec| Box::new(GatedClient { gate: gate.clone() }) as Box<dyn PlcClient>)
    }

    /// 「[`COLLECT_OPERATION_TIMEOUT`] に掛からずに終わった」ことを確かめて、
    /// 結果の状態だけ取り出す。打ち切りを扱わないテスト（この下のほとんど）が
    /// **`pending` を見落とさない**ようにするための一手間 - 素通しにすると、
    /// 上限が誤って効いている回帰を「状態が違う」という遠い形でしか
    /// 検出できない。
    fn settled(outcome: CollectOutcome) -> CollectorState {
        assert!(
            !outcome.pending,
            "上限に掛からない前提のテストで打ち切られた: {outcome:?}"
        );
        outcome.status
    }

    /// 接続状態の読み出しから「**読めた結果**」だけを取り出す。
    /// `None` = 走っていない、`Some` = 走っている（0 件を含む）。
    ///
    /// **打ち切り（[`Readout::Unavailable`]）は panic させる** - このヘルパを
    /// 使うテストはどれも「タスクが手空きのはず」の場面で呼んでおり、そこで
    /// 打ち切られるのは足場の前提が崩れている。素通しして `None` に潰すと、
    /// **「走っていない」と見分けが付かなくなる**（読み取りが詰まる回帰を
    /// 「エンジンが立っていない」と読み違える）。
    async fn statuses(svc: &CollectorService) -> Option<HashMap<String, ConnectionStatus>> {
        match svc.connection_status().await {
            Readout::NotRunning => None,
            Readout::Ready { data } => Some(data),
            Readout::Unavailable => {
                panic!("接続状態の読み出しが打ち切られた（手空きのはずの場面）")
            }
        }
    }

    /// いま溜まっているイベントの種類を全部取り出す（待たない）。
    fn drain(rx: &mut broadcast::Receiver<CollectEvent>) -> Vec<banto_collect::EventKind> {
        let mut kinds = Vec::new();
        while let Ok(event) = rx.try_recv() {
            kinds.push(event.kind);
        }
        kinds
    }

    fn kind_count(kinds: &[banto_collect::EventKind], kind: banto_collect::EventKind) -> usize {
        kinds.iter().filter(|k| **k == kind).count()
    }

    fn count_kind(
        rx: &mut broadcast::Receiver<CollectEvent>,
        kind: banto_collect::EventKind,
    ) -> usize {
        kind_count(&drain(rx), kind)
    }

    async fn service(dir: &TempDir) -> (SqlitePool, CollectorService) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
        );
        (pool, svc)
    }

    /// [`service`] と同じだが、切断を [`StopGate`] で塞げるサービス。
    async fn gated_service(dir: &TempDir) -> (SqlitePool, CollectorService, Arc<StopGate>) {
        let pool = init_db_memory().await.expect("init_db_memory");
        let gate = StopGate::new();
        let svc = CollectorService::new_for_test(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            gated_factory(&gate),
        );
        (pool, svc, gate)
    }

    /// 指定した種類のイベントが来るまで待つ。**実時間を待たない**
    /// （broadcast の受信で待ち合わせるだけ）。
    async fn wait_for(rx: &mut broadcast::Receiver<CollectEvent>, kind: banto_collect::EventKind) {
        loop {
            let event = rx.recv().await.expect("イベント購読が切れた");
            if event.kind == kind {
                return;
            }
        }
    }

    /// 有効な接続 1 と、その下の**有効グループ 1**をレジストリに入れる
    /// （**タグは作らない**）。グループ id を返すので、テストが自分で
    /// 「タグ 0 件」「無効タグだけ」といった構成を組める。
    async fn seed_enabled_group(pool: &SqlitePool) -> i64 {
        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                // 127.0.0.1:1 は何も listen していない予約ポート。
                // `offline_factory` を使うのでそもそも誰も dial しない。
                host: "127.0.0.1".to_string(),
                port: 1,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .expect("create plc connection");
        let group = CollectionGroupService::new(pool.clone())
            .create(CollectionGroupInput {
                name: "G1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
                default_writable: true,
                query_sql: None,
            })
            .await
            .expect("create collection group");
        group.id
    }

    /// [`seed_enabled_group`] のグループにタグを 1 本足す。
    /// `address` を呼び出し側が決められるので、「解釈できないアドレス」で
    /// 構成組み立ての失敗も作れる。`enabled` で無効タグも作れる。
    async fn seed_tag(pool: &SqlitePool, group_id: i64, address: &str, enabled: bool) {
        TagService::new(pool.clone())
            .create(TagInput {
                name: "T1".to_string(),
                collection_group_id: group_id,
                address: address.to_string(),
                data_type: "i16".to_string(),
                string_length: None,
                string_encoding: "utf8".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                threshold_h: None,
                threshold_hh: None,
                threshold_l: None,
                threshold_ll: None,
                enabled,
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("create tag");
    }

    /// 有効な接続 1 / グループ 1 / **有効タグ 1** - 「走る」構成。
    async fn seed_one_tag(pool: &SqlitePool, address: &str) {
        let group_id = seed_enabled_group(pool).await;
        seed_tag(pool, group_id, address, true).await;
    }

    /// **C-1 の一番大事な受入条件**: 有効な収集対象が 1 件も無いときは
    /// 「収集対象なし」であって、**エラーではない**。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `tag_count() == 0` 分岐を
    /// 消して `Collector::start` の `CollectError::Config` をそのまま返す実装に
    /// 戻すと、`start()` が `Err` を返すのでこのテストは `expect` で落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_enabled_targets_is_a_state_of_its_own_not_an_error() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        let state = settled(
            svc.start()
                .await
                .expect("収集対象 0 件は「エラー」ではない（Ok が返る）"),
        );
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // 「走っていない」ことが読み出し側からも分かる（空に潰さない）。
        assert!(statuses(&svc).await.is_none());
        assert!(svc.current_values().is_none());
    }

    /// #406 レビュー P2: **接続とグループは有効なのに、タグが 1 本も
    /// 登録されていない**構成。`build_config` はタグが空の有効グループも計画に
    /// 残す（`GroupPlan` の `tags`/`requests` が空になるだけ）ので
    /// `group_count() == 1` / `tag_count() == 0` になり、`group_count` で
    /// 判定していた頃は **`Running { groups: 1, tags: 0 }` になって PLC へ
    /// 繋ぎに行き、tstore まで開いていた**。収集する物は 1 つも無い。
    ///
    /// 反証（回帰の検出）: 判定を `config.group_count() == 0` に戻すと
    /// `NoTargets` ではなく `Running { groups: 1, tags: 0 }` が返り、
    /// [`statuses`] も `Some` になるのでこのテストは落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_with_no_tag_at_all_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_enabled_group(&pool).await;

        let state = settled(svc.start().await.expect("有効タグ 0 件はエラーではない"));
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        // **収集エンジンを起動していない** = PLC へ繋ぎにも行っていない。
        assert!(
            statuses(&svc).await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
        assert!(svc.current_values().is_none());
    }

    /// 同上の、**タグは登録されているが全部無効**な場合。`build_config` は
    /// 無効タグを落とすので、レジストリに行はあっても計画のタグは 0 件になる。
    ///
    /// 反証（回帰の検出）: 上と同じ - `group_count()` 判定に戻すと
    /// `Running { groups: 1, tags: 0 }` になって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_enabled_group_whose_tags_are_all_disabled_is_no_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        let group_id = seed_enabled_group(&pool).await;
        seed_tag(&pool, group_id, "40001", false).await;

        let state = settled(svc.start().await.expect("有効タグ 0 件はエラーではない"));
        assert_eq!(state, CollectorState::NoTargets);
        assert_eq!(svc.state(), CollectorState::NoTargets);
        assert!(
            statuses(&svc).await.is_none(),
            "収集対象が無いのにエンジンが立っている"
        );
        assert!(svc.current_values().is_none());
    }

    /// 停止中に `stop()` を呼んでも壊れない（冪等）。あわせて、**`stop()` が
    /// 直前の理由（ここでは `NoTargets`）を握り潰さない**ことも固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_is_idempotent_and_keeps_the_previous_reason() {
        let dir = TempDir::new();
        let (_pool, svc) = service(&dir).await;

        assert_eq!(
            settled(svc.stop().await.expect("停止中の stop")),
            CollectorState::Stopped
        );
        assert_eq!(svc.state(), CollectorState::Stopped);

        svc.start().await.expect("start");
        assert_eq!(svc.state(), CollectorState::NoTargets);

        // 走っていないので「何もしない」= NoTargets のまま。
        assert_eq!(
            settled(svc.stop().await.expect("走っていない stop")),
            CollectorState::NoTargets
        );
        assert_eq!(
            settled(svc.stop().await.expect("2 回目の stop")),
            CollectorState::NoTargets
        );
    }

    /// `start()` に失敗したら**理由が状態に残る**。解釈できないアドレスの
    /// タグを 1 本入れて `build_config` を失敗させる（`banto-tags` は
    /// アドレスの書式を検証しない - 書式は I2/I3b の担当）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_start_keeps_its_reason_in_the_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "これはアドレスではない").await;

        let err = svc.start().await.expect_err("構成が組み立てられない");
        let message = err.to_string();

        match svc.state() {
            CollectorState::StartFailed { reason } => {
                assert!(
                    !reason.trim().is_empty(),
                    "理由が空になっている: {reason:?}"
                );
                assert!(
                    message.contains(&reason) || reason.contains("収集設定エラー"),
                    "状態の理由と返ったエラーが食い違っている: state={reason:?} err={message:?}"
                );
            }
            other => panic!("StartFailed を期待したが {other:?}"),
        }
        assert!(statuses(&svc).await.is_none());
    }

    /// 収集対象があるときは本当に起動し、読み出しが「走っている」形になる。
    /// 二重 `start()` が 2 つ目のエンジンを立てないことも同時に固定する。
    ///
    /// **数え方**: 接続ごとの `ConnectionStatus` は接続タスクが**自分の
    /// タイミングで**書き込むので、起動直後の件数を数えるのは時間に依存する
    /// （それを待つのは「実時間待ちのテスト」になる）。代わりに
    /// `collection_started` イベントの本数を数える - これは
    /// `Collector::start` が**戻る前に**必ず 1 回出すので、2 本目のエンジンが
    /// 立っていれば必ず 2 件になる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_twice_runs_exactly_one_engine() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        let first = settled(svc.start().await.expect("1 回目の start"));
        assert_eq!(first, CollectorState::Running { groups: 1, tags: 1 });
        assert!(svc.state().is_running());
        assert!(
            statuses(&svc).await.is_some(),
            "走っているので Some（`None` = 走っていない、と読み分けられる）"
        );
        assert!(svc.current_values().is_some());

        let second = settled(svc.start().await.expect("2 回目の start"));
        assert_eq!(second, first, "二重起動はせず、同じ状態を返す");
        assert_eq!(
            count_kind(&mut rx, banto_collect::EventKind::CollectionStarted),
            1,
            "2 回目の start がもう 1 つエンジンを立てていない"
        );

        svc.stop().await.expect("stop");
        assert_eq!(svc.state(), CollectorState::Stopped);
        assert!(statuses(&svc).await.is_none());
    }

    /// `start` と `stop` を**同時に**投げても、ライフサイクルタスクが逐次
    /// 処理するので「半分だけ起動した」状態は残らない - 終わったあとは必ず
    /// 「走っている（`Running`）」か「止まっている（`Stopped`）」の
    /// どちらかで、しかも状態と読み出しが一致する。
    ///
    /// 実時間を待たない（`join` するだけ。`sleep` も `tokio::time` の進行も
    /// 使っていない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_and_stop_racing_leave_a_consistent_state() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        starter.await.expect("start task").expect("start");
        stopper.await.expect("stop task").expect("stop");

        let state = svc.state();
        let running = statuses(&svc).await.is_some();
        assert_eq!(
            state.is_running(),
            running,
            "状態（{state:?}）と読み出し（走っている={running}）が食い違っている"
        );
        assert!(
            matches!(
                state,
                CollectorState::Running { .. } | CollectorState::Stopped
            ),
            "中途半端な状態が残っている: {state:?}"
        );

        // 後始末（走っていれば止める。止まっていれば何もしない）。
        svc.stop().await.expect("後始末の stop");
    }

    /// `restart()` は止めてから起動する。走っていない状態から呼んでも
    /// 単なる起動として成立し、走っている状態から呼ぶと**止めてから**
    /// 立て直す（= 前のエンジンが残らない）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_stops_the_old_engine_before_starting_a_new_one() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        // 走っていない状態からの restart = ただの起動（停止は何もしない）。
        let state = settled(svc.restart().await.expect("restart"));
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let again = settled(svc.restart().await.expect("2 回目の restart"));
        assert_eq!(again, CollectorState::Running { groups: 1, tags: 1 });

        let events = drain(&mut rx);
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStarted),
            2,
            "restart 2 回で起動は 2 回: {events:?}"
        );
        assert_eq!(
            kind_count(&events, banto_collect::EventKind::CollectionStopped),
            1,
            "2 回目の restart は古いエンジンを確かに止めてから立て直した: {events:?}"
        );

        svc.stop().await.expect("stop");
    }

    // --- 停止の途中キャンセル（#406 レビュー P2） ------------------------

    /// **この修正の一番大事な受入条件**: `stop()` の呼び出し側が
    /// 停止処理の途中で消えても、**停止は実体まで完了し、状態と食い違わない**。
    ///
    /// 旧実装（サービス側で `AsyncMutex<Option<Collector>>` を `take()` して
    /// から `collector.stop().await`）では、この瞬間にキャンセルされると
    /// `collector = None` / 状態 = `Running` のまま**永久に**固定され、
    /// もう一度 `stop()` を呼んでも「走っていない」分岐に落ちて直せなかった。
    /// さらに接続タスクの join も最終 flush も行われないままだった。
    ///
    /// 実時間は待たない（ゲートは [`Notify`]、完了待ちは読み出しのキュー）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_stop_still_finishes_the_stop() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        // 接続タスクが `Connected` になるまで待つ。そこまで行かないと
        // graceful exit が `disconnect` を通らず、ゲートに入らない。
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        // 「停止が実体の停止の途中まで進んだ」ことを確かめてから切る
        // （切るのが早すぎると、そもそも何も始まっていない）。
        gate.wait_entered().await;
        stopper.abort();
        assert!(
            stopper.await.unwrap_err().is_cancelled(),
            "呼び出し側は確かにキャンセルされた"
        );

        gate.release();

        // 読み出しはライフサイクルタスクのキューを通るので、**飛行中の停止が
        // 終わってから**返る - sleep で待つ必要がない。
        assert!(
            statuses(&svc).await.is_none(),
            "キャンセル後も停止は実体まで完了している"
        );
        assert!(svc.current_values().is_none());
        assert_eq!(
            svc.state(),
            CollectorState::Stopped,
            "状態と実体が一致している"
        );

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "最終 flush まで進んだ（`collection_stopped` は stop の最後に出る）: {kinds:?}"
        );

        // キャンセルの後にもう一度呼んでも壊れない（冪等のまま）。
        assert_eq!(
            settled(svc.stop().await.expect("キャンセル後の stop")),
            CollectorState::Stopped
        );
    }

    /// キャンセルされた `stop()` の**後に投げた `start()` は、その停止が
    /// 完了するまで進まない**（オーナー指定）。
    ///
    /// 証拠はイベントの順序: `collection_stopped` は旧エンジンの最終 flush の
    /// **後**に出るので、2 本目の `collection_started` がその後ろに来ていれば、
    /// 新しい起動は確かに前の停止の完了を待っている。
    ///
    /// `start` の依頼を**キューに積んでからゲートを開ける**ために、ここだけ
    /// 内部の [`Command`] を直接送っている（`svc.start()` を別タスクで
    /// 走らせる書き方だと、依頼が積まれる前にゲートを開けてしまう競争になり、
    /// テストが「たまたま順番どおり」でも通ってしまう）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_queued_after_a_cancelled_stop_waits_for_it() {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        gate.wait_entered().await;
        stopper.abort();
        let _ = stopper.await;

        // 停止はまだゲートの中。ここで start をキューへ積む。
        let (reply_tx, reply_rx) = oneshot::channel();
        svc.commands()
            .send(Command::Start(reply_tx))
            .await
            .map_err(|_| ())
            .expect("start をキューに積む");
        gate.release();
        let state = reply_rx
            .await
            .expect("start の応答")
            .expect("キャンセル後の start");
        assert_eq!(state, CollectorState::Running { groups: 1, tags: 1 });

        let kinds = drain(&mut rx);
        let stopped = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStopped)
            .unwrap_or_else(|| panic!("前の停止が完了していない: {kinds:?}"));
        let started = kinds
            .iter()
            .position(|k| *k == banto_collect::EventKind::CollectionStarted)
            .unwrap_or_else(|| panic!("新しい起動が見当たらない: {kinds:?}"));
        assert!(
            stopped < started,
            "新しい start が、前の stop の完了を待たずに進んだ: {kinds:?}"
        );

        svc.stop().await.expect("後始末の stop");
    }

    /// `subscribe_events()` は start/stop をまたいで生き続ける
    /// （`EventSink` を `Collector` ではなくサービスが持っているため）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_subscription_survives_start_and_stop() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        let mut rx = svc.subscribe_events();
        svc.start().await.expect("start");
        svc.stop().await.expect("stop");

        let kinds = drain(&mut rx);
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStarted),
            "購読は start 前に取ったので collection_started が届く: {kinds:?}"
        );
        assert!(
            kinds.contains(&banto_collect::EventKind::CollectionStopped),
            "同じ受信ハンドルで stop も見える: {kinds:?}"
        );
    }

    // --- 無応答への上限と `Starting`（C-2） -------------------------------

    /// **C-2 の一番大事な受入条件**: 起動が解決しないとき、呼び出し側は
    /// [`COLLECT_OPERATION_TIMEOUT`] で**待つのをやめる**が、
    ///
    /// * それは**失敗ではない**（`Err` にならず `pending` が立つ）、
    /// * 状態は [`CollectorState::Starting`] のままで **`Stopped` と嘘を
    ///   つかない**、
    /// * **その後タスク側が完了すると状態が追いつく**。
    ///
    /// 実時間は 1 ミリ秒も待たない: 起動は [`StartGate`] で止め、上限は
    /// [`tokio::time::pause`] の仮想時計が自動で進めて消費する。
    ///
    /// **時計を止めるのは足場を組み終えてから**（`start_paused = true` では
    /// ない）。sqlx の接続確立は別スレッドで進むので、仮想時計のままだと
    /// ランタイムがアイドルになった瞬間に時間が飛び、プールの取得上限
    /// （30 秒）が**接続が返る前に**切れて `init_db_memory` 自体が失敗する。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `set_state(Starting)` を
    /// 消すと、打ち切り時点の状態が `Stopped` のままになって 2 番目の
    /// `assert_eq!` が落ちる。`CollectorService::lifecycle` の
    /// `tokio::time::timeout` を外すと、`starter` が永久に返らずテストが
    /// 終わらない（= 上限が効いていないことがそのまま現れる）。
    ///
    /// 上限を消費し終えたら**時計を実時間へ戻す**。戻さずに仮想時計のまま
    /// 進めると、アイドルのたびに時間が飛んで sqlx のプール保守が
    /// 「10 分アイドル」の接続を刈り、`sqlite::memory:` の DB ごと消えてしまう。
    #[tokio::test]
    async fn an_unresponsive_start_is_abandoned_not_failed_and_the_state_catches_up() {
        let dir = TempDir::new();
        let pool = init_db_memory().await.expect("init_db_memory");
        let gate = Arc::new(StartGate::default());
        let svc = CollectorService::new_for_test_gated(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
            gate.clone(),
        );

        tokio::time::pause();

        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        // 起動が「`Starting` を立てた直後」まで来たことを確かめてから待つ
        // （来る前に上限が過ぎると、テストは通っても何も確かめていない）。
        gate.entered.notified().await;
        assert_eq!(
            svc.state(),
            CollectorState::Starting,
            "tstore を開く前に「起動中」が立っていない"
        );

        let outcome = starter
            .await
            .expect("start task")
            .expect("打ち切りは `Err` にしない");
        assert!(
            outcome.pending,
            "上限で待つのをやめたことが呼び出し側に伝わっていない: {outcome:?}"
        );
        assert_eq!(
            outcome.status,
            CollectorState::Starting,
            "打ち切った時点の状態を載せていない"
        );
        assert_eq!(
            svc.state(),
            CollectorState::Starting,
            "打ち切っただけなのに「止まっている」と嘘をついている"
        );

        tokio::time::resume();

        // タスク側の起動処理はまだ続いている。開けてやると、状態が追いつく。
        gate.release.notify_one();
        assert!(
            statuses(&svc).await.is_none(),
            "収集対象が無い構成なのでエンジンは立たない"
        );
        assert_eq!(
            svc.state(),
            CollectorState::NoTargets,
            "タスク側が決着した後も `Starting` のまま取り残されている"
        );
    }

    // --- 未受付を「受付済み」と言わない（#407 レビュー P2-1） --------------

    /// **#407 レビュー P2-1 の受入条件**: **キューに入れられなかった操作は
    /// 「受付済み（`pending: true`）」で返らない**。
    ///
    /// オーナー指定の再現手順をそのまま組む:
    ///
    /// 1. 収集対象がある状態で起動を [`StartGate`] で止める（ライフサイクル
    ///    タスクはこの 1 件を処理中のまま動かない）、
    /// 2. キューを [`COMMAND_QUEUE_DEPTH`] 件で満杯にする、
    /// 3. その状態で `stop()` を呼び、投入を打ち切らせる。
    ///
    /// 旧実装（上限 1 本で投入ごと包む）は、ここで `Ok(pending: true)` を
    /// 返していた。**停止コマンドはどこにも送られていないのに**「後から必ず
    /// 実行される」と言い、REST / Tauri はそれを `result: "ok"` で監査に
    /// 残していた。手順 4 がその実害（**収集が動き続ける**）を固定する。
    ///
    /// 実時間は 1 ミリ秒も待たない: ゲートは [`Notify`]、投入の上限は
    /// [`tokio::time::pause`] の仮想時計が自動で進めて消費する。**時計を
    /// 止めるのは足場を組み終えてから**（理由は 1 つ上のテストの doc と同じ -
    /// sqlx のプールが巻き添えになる）。
    ///
    /// 反証（回帰の検出）: [`CollectorService::lifecycle`] の投入を
    /// `send_timeout` から無期限の `send().await` に戻し、上限 1 本
    /// （`timeout(COLLECT_OPERATION_TIMEOUT, ..)`）で投入ごと包むと、
    /// `stop()` が `Err` ではなく `Ok(pending: true)` を返すので
    /// `expect_err` が落ちる。
    #[tokio::test]
    async fn an_operation_that_never_reached_the_queue_is_refused_not_called_pending() {
        let dir = TempDir::new();
        let pool = init_db_memory().await.expect("init_db_memory");
        seed_one_tag(&pool, "40001").await;
        let gate = Arc::new(StartGate::default());
        let svc = CollectorService::new_for_test_gated(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
            gate.clone(),
        );

        // 1. 起動を止める。タスクはこの 1 件を抱えたまま先へ進まない。
        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        gate.entered.notified().await;

        // 2. **操作用**キューを満杯にする。#408 で読み取りを別のチャネルへ
        //    移したので、ここは操作コマンドで埋めるしかない（そしてそれが
        //    正しい - 操作用キューを埋められるのは操作だけ、が #408 の直しの
        //    中身そのもの）。**`Start` を使う**: ゲートが開いた後に順に
        //    処理されるが、`start` は既に走っていれば何もしない（冪等）ので、
        //    手順 4 の「収集が動き続けている」を汚さない。
        let mut fillers = Vec::new();
        loop {
            let (reply_tx, reply_rx) = oneshot::channel();
            match svc.commands().try_send(Command::Start(reply_tx)) {
                Ok(()) => fillers.push(reply_rx),
                Err(mpsc::error::TrySendError::Full(_)) => break,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    panic!("前提が崩れている: ライフサイクルタスクが居ない")
                }
            }
        }
        assert_eq!(
            fillers.len(),
            COMMAND_QUEUE_DEPTH,
            "キューが満杯になっていない（この後の stop が普通に入ってしまう）"
        );

        // 3. この状態の `stop()`。**未受付が明示される**こと。
        tokio::time::pause();
        let err = svc
            .stop()
            .await
            .expect_err("キューに入れられなかった操作を `Ok` で返している");
        tokio::time::resume();
        assert_eq!(
            err.to_string(),
            ENQUEUE_BUSY_MESSAGE,
            "「混雑で未受付」が「内部タスクが停止」と読み分けられない: {err}"
        );

        // 4. ゲートを開けると起動は完了する。**停止はどこにも届いていない**
        //    ので収集は走り続けている - 「受付済み」と嘘をついていたときに
        //    起きていた実害そのもの。
        gate.release.notify_one();
        let started = starter.await.expect("start task").expect("start");
        assert_eq!(
            started.status,
            CollectorState::Running { groups: 1, tags: 1 }
        );
        assert!(
            svc.state().is_running(),
            "受け付けていない `stop()` が実行されたことになっている: {:?}",
            svc.state()
        );
        assert!(
            statuses(&svc).await.is_some(),
            "収集は動き続けているはず（未受付の stop は実行されない）"
        );

        svc.stop().await.expect("後始末の stop");
    }

    // --- 閲覧のポーリングが操作の受付を塞がない（#408 レビュー P2-1） ------

    /// **#408 レビュー P2-1 の受入条件**: **接続状態のポーリングを何回
    /// 打ち切らせても、その後の `stop()` が「滞留を理由に未受付」に
    /// ならない**。
    ///
    /// オーナー指定の再現手順:
    ///
    /// 1. 収集対象がある状態で起動を [`StartGate`] で止める（ライフサイクル
    ///    タスクはこの 1 件を抱えたまま動かない = キュー経由の読み取りは
    ///    1 件も捌けない）、
    /// 2. その状態で [`CollectorService::connections`] を
    ///    [`COMMAND_QUEUE_DEPTH`] 回（= 操作用キューを埋めるのに十分な回数）
    ///    呼び、全部打ち切らせる、
    /// 3. **その後の `stop()` が受け付けられる**こと。
    ///
    /// **確かめているのは「停止処理が即座に完了すること」ではなく、
    /// 「停止要求を受け付けられること」**（起動はゲートの中なので、停止が
    /// 実際に走るのはその後）。だから `Ok` であれば十分で、
    /// [`CollectOutcome::pending`] が立っているのは正しい - 依頼はタスク側に
    /// 確かに届いており、[`ENQUEUE_BUSY_MESSAGE`] の `Err`（＝**どこにも
    /// 届いていない**）とは意味が違う。
    ///
    /// 実時間は 1 ミリ秒も待たない: ゲートは [`Notify`]、2 つの上限は
    /// [`tokio::time::pause`] の仮想時計が自動で進めて消費する。**時計を
    /// 止めるのは足場を組み終えてから**（理由は他の 2 本と同じ - sqlx の
    /// プールが巻き添えになる）。
    ///
    /// 反証（回帰の検出）: 読み取りを操作用キューへ戻す
    /// （[`CollectorService::connection_status`] を
    /// `self.commands().send(..)` に書き換える）と、手順 2 で操作用キューが
    /// [`COMMAND_QUEUE_DEPTH`] 件の**既に答えを返した読み取り**で埋まり、
    /// 手順 3 の `stop()` が [`ENQUEUE_BUSY_MESSAGE`] の `Err` になるので
    /// `expect` が落ちる。
    #[tokio::test]
    async fn polling_connections_never_keeps_a_stop_from_being_accepted() {
        let dir = TempDir::new();
        let pool = init_db_memory().await.expect("init_db_memory");
        seed_one_tag(&pool, "40001").await;
        let gate = Arc::new(StartGate::default());
        let svc = CollectorService::new_for_test_gated(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
            gate.clone(),
        );
        let mut rx = svc.subscribe_events();

        // 1. 起動を止める。タスクはこの 1 件を抱えたまま先へ進まない。
        let starter = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.start().await })
        };
        gate.entered.notified().await;

        tokio::time::pause();

        // 2. 閲覧（viewer でもできる）のポーリング。**全部「読めなかった」**
        //    で畳まれる - 走っていないのでも 0 件なのでもない。
        for round in 1..=COMMAND_QUEUE_DEPTH {
            let connections = svc.connections().await;
            assert_eq!(
                connections,
                Readout::Unavailable,
                "{round} 回目のポーリングが「読めなかった」以外になっている: {connections:?}"
            );
        }

        // 3. **その後の停止要求**。混雑を理由に突き返されないこと。
        let outcome = svc
            .stop()
            .await
            .expect("閲覧のポーリングが操作の受付を塞いでいる（未受付で返った）");
        assert!(
            outcome.pending,
            "起動がゲートの中なので、停止はまだ終わっていないはず: {outcome:?}"
        );

        tokio::time::resume();

        // 受け付けた停止は、起動の完了後に**実行される**（`pending` の約束）。
        // 実時間は待たず、収集エンジンが出す停止イベントで待ち合わせる。
        gate.release.notify_one();
        starter.await.expect("start task").expect("start");
        wait_for(&mut rx, banto_collect::EventKind::CollectionStopped).await;
        assert_eq!(
            svc.state(),
            CollectorState::Stopped,
            "受け付けた停止が実行されていない"
        );
    }

    /// **#408 レビュー（オーナー判断）の受入条件**: ライフサイクルタスクが
    /// 居ない（チャネルが閉じた）状態の読み取りは **「読めなかった」**
    /// であって、**「走っていない」ではない**。
    ///
    /// 足場は「タスクが panic した直後」の観測を作る
    /// （[`CollectorService::new_for_test_without_lifecycle_task`]）。葉には
    /// **`Running` を公開したまま**にしておく - タスクが消えても葉は最後に
    /// 公開された状態を返し続けるので、これが実際に起こる姿。
    ///
    /// ここで `NotRunning` と答えると**二重に嘘になる**（このモジュールの doc
    /// 「タスクが居ないことを「走っていない」と答えない」）:
    ///
    /// 1. 同じ画面に「状態: 動作中」（[`CollectorService::state_view`]）と
    ///    「接続状態: 走っていません」が並ぶ、
    /// 2. [`Collector`] に `Drop` が無く、接続タスクは止まらないので、
    ///    **収集は実際には続いている公算が大きい**。
    ///
    /// 反証（回帰の検出）: [`CollectorService::connection_status`] の
    /// `TrySendError::Closed` 分岐を `Readout::NotRunning` に戻すと、
    /// 2 つ目の `assert_eq!` が落ちる。
    #[tokio::test]
    async fn a_read_with_no_lifecycle_task_says_unreadable_not_not_running() {
        let dir = TempDir::new();
        let pool = init_db_memory().await.expect("init_db_memory");
        let svc = CollectorService::new_for_test_without_lifecycle_task(
            pool.clone(),
            dir.path().join("data"),
            fast_options(),
            offline_factory(),
        );
        // タスクが消える直前に公開されていた状態。葉はこれを返し続ける。
        svc.inner
            .ctx
            .publish(CollectorState::Running { groups: 1, tags: 1 }, None);

        assert_eq!(
            svc.state_view(),
            CollectorStateView::Running { groups: 1, tags: 1 },
            "葉は最後に公開された状態を返し続けるはず（前提）"
        );

        let connections = svc.connections().await;
        assert_eq!(
            connections,
            Readout::Unavailable,
            "タスクが居ないことを「走っていません」と言い切っている（状態表示と食い違い、\
             かつ接続タスクは止まっていない）: {connections:?}"
        );
        assert_ne!(
            connections.as_str(),
            "notRunning",
            "「判定できなかった」を「止まっている」側へ倒している"
        );
    }

    /// 普段の（混んでいない）経路では、投入用の上限を足しても**何も
    /// 変わらない** - 3 つの操作とも `pending` にならずに決着する。
    /// 上の 1 本だけだと「常にエラーを返す実装」でも通ってしまうので、
    /// その反対側を押さえる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_uncongested_operation_is_still_accepted_and_settles() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        assert_eq!(
            settled(svc.start().await.expect("start")),
            CollectorState::Running { groups: 1, tags: 1 }
        );
        assert_eq!(
            settled(svc.restart().await.expect("restart")),
            CollectorState::Running { groups: 1, tags: 1 }
        );
        assert_eq!(
            settled(svc.stop().await.expect("stop")),
            CollectorState::Stopped
        );
    }

    // --- 公開用の状態（#407 レビュー P2-2） -------------------------------

    /// 公開用の型は**5 つの状態を同じ綴りで保ったまま、理由だけ落とす**。
    ///
    /// 綴りは [`CollectorState::as_str`]（＝監査の `collectState`）と
    /// 一致していること - 画面で見た値と監査で見た値が食い違わないための
    /// 前提で、公開用の型を足したことでそこが崩れていないかをここで固定する。
    ///
    /// 反証（回帰の検出）: [`CollectorStateView::StartFailed`] に
    /// `reason` を持たせて [`From`] で詰め直すと、`reason` の
    /// `assert!` が落ちる。
    #[test]
    fn the_public_state_view_keeps_every_state_but_drops_the_reason() {
        let marker = "CHRONOGAZER-LEAK-CANARY";
        let cases = [
            CollectorState::Stopped,
            CollectorState::Starting,
            CollectorState::Running {
                groups: 2,
                tags: 10,
            },
            CollectorState::NoTargets,
            CollectorState::StartFailed {
                reason: format!("収集設定エラー: {marker} が開けません"),
            },
        ];

        for state in cases {
            let json = serde_json::to_value(CollectorStateView::from(&state)).expect("serialize");
            assert_eq!(
                json["state"],
                state.as_str(),
                "公開用の綴りが内部の状態（＝監査の綴り）と食い違っている: {state:?}"
            );
            assert!(
                json.get("reason").is_none(),
                "公開用の形に `reason` が残っている: {json}"
            );
            assert!(
                !json.to_string().contains(marker),
                "理由の中身が公開用の形に漏れている: {json}"
            );
        }

        // 件数は落とさない（監視画面の基本情報）。
        let running = serde_json::to_value(CollectorStateView::from(&CollectorState::Running {
            groups: 2,
            tags: 10,
        }))
        .expect("serialize");
        assert_eq!(running["groups"], 2);
        assert_eq!(running["tags"], 10);
    }

    // --- 起動時の自動開始（C-2） ------------------------------------------

    /// 自動開始は「収集対象があれば本当に走り出す」。`src-tauri` の
    /// `setup()` と `banto-serve` の `main()` が呼ぶのはこの 1 本だけなので、
    /// ここを押さえれば両経路の中身が同じであることも同時に決まる
    /// （呼び出し側の配線そのものは、`crate::hub` の `resume()` と同様に
    /// 目視確認の範囲 - どちらのエントリポイントも自動テストの外にある）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autostart_starts_collecting_when_there_are_targets() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_one_tag(&pool, "40001").await;

        svc.autostart().await;

        assert_eq!(svc.state(), CollectorState::Running { groups: 1, tags: 1 });
        assert!(statuses(&svc).await.is_some());

        svc.stop().await.expect("後始末の stop");
    }

    /// 自動開始は**起動を止めない**。収集対象 0 件（失敗ですらない）でも、
    /// 構成が壊れていて起動できなくても、`autostart()` は panic せずに戻り、
    /// **理由は状態に残る**。
    ///
    /// 反証（回帰の検出）: `autostart()` が `start()` の `Err` を
    /// `expect`/`?` で外へ出す実装に変えると、2 つ目のケースで panic して
    /// 落ちる（= アプリの起動が収集の失敗で止まることを意味する）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autostart_never_stops_the_app_and_keeps_the_reason() {
        let no_targets_dir = TempDir::new();
        let (_pool, svc) = service(&no_targets_dir).await;
        svc.autostart().await;
        assert_eq!(
            svc.state(),
            CollectorState::NoTargets,
            "収集対象 0 件は失敗ではない"
        );

        let failing_dir = TempDir::new();
        let (pool, failing) = service(&failing_dir).await;
        seed_one_tag(&pool, "これはアドレスではない").await;
        failing.autostart().await;
        match failing.state() {
            CollectorState::StartFailed { reason } => {
                assert!(!reason.trim().is_empty(), "理由が空になっている");
            }
            other => panic!("StartFailed を期待したが {other:?}"),
        }
    }

    // --- C-3a: 読み出しの 3 つの口 ----------------------------------------

    /// **C-3a の一番大事な受入条件**: 現在値の読み出しは**葉から**なので、
    /// ライフサイクルタスクが遅い操作を抱えていても**待たされない**。
    /// そして**同じ瞬間**、キューを通る接続状態の読み出しは
    /// [`COLLECT_READ_TIMEOUT`] で打ち切られて「**読めなかった**」になる -
    /// 「走っていない」でも「0 件」でもなく。
    ///
    /// この 1 本の中に**対照**が入っている: 同じ時点・同じサービスで、
    /// 葉の口は即答し、キューの口は打ち切られる。現在値をキュー経由に戻すと
    /// 前者も後者と同じ結末になる（= #400 で潰した「画面が固まる」型の再発）。
    ///
    /// 実時間は 1 ミリ秒も待たない: 停止は [`StopGate`]（`Notify`）で止め、
    /// 上限は [`tokio::time::pause`] の仮想時計が自動で進めて消費する。
    /// **時計を止めるのは足場を組み終えてから**（`start_paused = true` では
    /// ない）- 理由は `an_unresponsive_start_is_abandoned_...` の doc と同じ
    /// （sqlx のプールが巻き添えになる）。current-thread ランタイム
    /// （`#[tokio::test]` の既定）なのは、仮想時計の auto-advance が
    /// そちらでしか効かないため。
    ///
    /// 反証（回帰の検出）: [`CollectorService::values`] を
    /// 「`Command::CurrentValues` をキューへ送って待つ」旧実装に戻すと、
    /// 現在値の読み出しも停止の完了まで返らなくなり、仮想時計が進んで
    /// `tokio::time::timeout` が `Elapsed` になるので最初の `expect` が落ちる。
    #[tokio::test]
    async fn reading_current_values_does_not_wait_behind_a_slow_stop_but_connections_say_unavailable(
    ) {
        let dir = TempDir::new();
        let (pool, svc, gate) = gated_service(&dir).await;
        seed_one_tag(&pool, "40001").await;
        let mut rx = svc.subscribe_events();

        svc.start().await.expect("start");
        // 接続タスクが `Connected` になるまで待つ（そこまで行かないと
        // graceful exit が `disconnect` を通らず、ゲートに入らない）。
        wait_for(&mut rx, banto_collect::EventKind::PlcConnected).await;

        // 停止を「実体の停止の途中」で止める。ライフサイクルタスクはこの
        // 1 件を抱えたまま動かない = キュー経由の読み出しは返らない。
        let stopper = {
            let svc = svc.clone();
            tokio::spawn(async move { svc.stop().await })
        };
        gate.wait_entered().await;

        tokio::time::pause();

        // 葉からの読み出し: **待たされない**。
        let values = tokio::time::timeout(COLLECT_READ_TIMEOUT * 10, async { svc.values() })
            .await
            .expect("現在値の読み出しが（キューを通って）待たされた");
        assert_eq!(
            values.as_str(),
            "ready",
            "停止が完了するまでは現在値が読めるはず（取り下げは停止の完了後）: {values:?}"
        );

        // **同じ瞬間**のキュー経由の読み出し: 打ち切られて「読めなかった」。
        let connections = svc.connections().await;
        assert_eq!(
            connections,
            Readout::Unavailable,
            "打ち切りを「走っていない」や「0 件」に潰している: {connections:?}"
        );

        tokio::time::resume();

        gate.release();
        stopper.await.expect("stop task").expect("stop");

        // 停止が完了したら、両方とも「走っていない」。**0 件ではない。**
        assert_eq!(
            svc.values(),
            Readout::NotRunning,
            "停止後も現在値ハンドルが葉に残っている（取り下げ漏れ）"
        );
        assert_eq!(svc.connections().await, Readout::NotRunning);
    }

    /// 現在値の言い分けを**表で**固定する（純関数 [`values_readout`]）。
    /// 「走っていない」「読めて 0 件」「読めて N 件」が別の値になること、
    /// そして**品質と時刻が落ちない**こと。
    ///
    /// 反証（回帰の検出）: `values_readout` の `None` 分岐を
    /// `Readout::Ready { data: HashMap::new() }` に変えると 2 番目の
    /// `assert_ne!` が落ちる（「走っていない」が「0 件」に潰れる）。
    /// [`CurrentSampleView`] から `quality` を外すと最後の `assert_eq!` が落ちる。
    #[test]
    fn the_values_readout_keeps_not_running_zero_and_real_samples_apart() {
        let not_running = values_readout(None);
        assert_eq!(not_running.as_str(), "notRunning");
        assert!(
            not_running.data().is_none(),
            "走っていないのに中身がある: {not_running:?}"
        );

        let zero = values_readout(Some(HashMap::new()));
        assert_eq!(zero.as_str(), "ready", "0 件は「読めた」: {zero:?}");
        assert!(zero.data().expect("ready").is_empty());
        assert_ne!(
            zero.as_str(),
            not_running.as_str(),
            "「読めて 0 件」と「走っていない」が同じ値になっている"
        );

        let samples: HashMap<String, CurrentSample> = [
            (
                "tag:1".to_string(),
                CurrentSample {
                    value: Some(1.5),
                    ptime_ms: 42,
                    quality: Quality::Good,
                },
            ),
            (
                "tag:2".to_string(),
                CurrentSample {
                    value: None,
                    ptime_ms: 43,
                    quality: Quality::Bad,
                },
            ),
            (
                "tag:3".to_string(),
                CurrentSample {
                    value: Some(2.0),
                    ptime_ms: 44,
                    quality: Quality::Stale,
                },
            ),
        ]
        .into_iter()
        .collect();
        let json = serde_json::to_value(values_readout(Some(samples))).expect("serialize");
        assert_eq!(json["state"], "ready");
        assert_eq!(
            json["data"]["tag:1"],
            serde_json::json!({ "value": 1.5, "ptimeMs": 42, "quality": "good" })
        );
        assert_eq!(
            json["data"]["tag:2"],
            serde_json::json!({ "value": null, "ptimeMs": 43, "quality": "bad" }),
            "読めなかったサンプルの値を 0 に潰していないか"
        );
        assert_eq!(
            json["data"]["tag:3"],
            serde_json::json!({ "value": 2.0, "ptimeMs": 44, "quality": "stale" })
        );
    }

    /// **#408 レビュー P2-2 の受入条件**: **非有限の値（NaN・`+∞`・`-∞`）が
    /// 「`value: null` なのに `quality: good`」で公開されない**。
    ///
    /// `serde_json` は非有限の `f64` を `null` にするので、内部の値をそのまま
    /// 写すと**数値として使えないのに品質は good** という自己矛盾した形が
    /// ワイヤに出る（このモジュールの doc「非有限の浮動小数点は公開する形で
    /// 正規化する」）。画面は品質を見て異常を判別できなくなる。
    ///
    /// 3 つの非有限値を**品質 good のまま**渡し、公開型 → JSON まで通して
    /// 確かめる。あわせて**有限値の Good / Bad / Stale と `ptimeMs` の既存の
    /// 扱いが変わっていない**ことも同じ表で押さえる（正規化が正常な値を
    /// 巻き込んでいないこと）。
    ///
    /// 反証（回帰の検出）: [`CurrentSampleView`] の [`From`] から
    /// `is_finite()` の分岐を外して `value`/`quality` をそのまま写す実装に
    /// 戻すと、非有限の 3 件が `{"value":null,...,"quality":"good"}` になり、
    /// `quality` の `assert_eq!` が落ちる。
    #[test]
    fn a_non_finite_sample_is_never_published_as_null_with_good_quality() {
        let non_finite = [
            ("tag:nan", f64::NAN),
            ("tag:inf", f64::INFINITY),
            ("tag:neg-inf", f64::NEG_INFINITY),
        ];
        let samples: HashMap<String, CurrentSample> = non_finite
            .iter()
            .enumerate()
            .map(|(index, (key, value))| {
                (
                    (*key).to_string(),
                    CurrentSample {
                        // **品質は good のまま**渡す（Modbus のデコードが
                        // 非有限値を除外していないので、実際にこう来る）。
                        value: Some(*value),
                        ptime_ms: 100 + index as i64,
                        quality: Quality::Good,
                    },
                )
            })
            .collect();

        let json = serde_json::to_value(values_readout(Some(samples))).expect("serialize");
        assert_eq!(json["state"], "ready");
        for (index, (key, value)) in non_finite.iter().enumerate() {
            let row = &json["data"][*key];
            assert_eq!(
                *row,
                serde_json::json!({
                    "value": serde_json::Value::Null,
                    "ptimeMs": 100 + index as i64,
                    "quality": "bad",
                }),
                "非有限値（{value}）が「値は null なのに品質は good」で公開されている: {row}"
            );
        }

        // 有限値の扱いは**変わっていない** - 3 つの品質がそのまま載り、
        // `Bad` の `value: None` も 0 に潰れない。
        let finite: HashMap<String, CurrentSample> = [
            (
                "tag:good".to_string(),
                CurrentSample {
                    value: Some(1.5),
                    ptime_ms: 42,
                    quality: Quality::Good,
                },
            ),
            (
                "tag:bad".to_string(),
                CurrentSample {
                    value: None,
                    ptime_ms: 43,
                    quality: Quality::Bad,
                },
            ),
            (
                "tag:stale".to_string(),
                CurrentSample {
                    value: Some(-0.0),
                    ptime_ms: 44,
                    quality: Quality::Stale,
                },
            ),
        ]
        .into_iter()
        .collect();
        let json = serde_json::to_value(values_readout(Some(finite))).expect("serialize");
        assert_eq!(
            json["data"]["tag:good"],
            serde_json::json!({ "value": 1.5, "ptimeMs": 42, "quality": "good" })
        );
        assert_eq!(
            json["data"]["tag:bad"],
            serde_json::json!({ "value": null, "ptimeMs": 43, "quality": "bad" }),
            "読めなかったサンプルの品質が正規化で書き換わっている"
        );
        assert_eq!(
            json["data"]["tag:stale"],
            serde_json::json!({ "value": -0.0, "ptimeMs": 44, "quality": "stale" }),
            "有限値（`-0.0` も有限）を非有限と取り違えている"
        );
    }

    /// 接続状態の言い分けを**表で**固定する（純関数 [`connections_readout`]）。
    /// 3 つ目の結末（[`Readout::Unavailable`]）は上限を掛けている
    /// [`CollectorService::connections`] 側の担当で、
    /// `reading_current_values_does_not_wait_behind_a_slow_stop_...` が見ている。
    ///
    /// 反証（回帰の検出）: `connections_readout` の `None` 分岐を空の `Ready` に
    /// 変えると `assert_ne!` が落ちる。
    #[test]
    fn the_connections_readout_keeps_not_running_zero_and_real_connections_apart() {
        let not_running = connections_readout(None);
        assert_eq!(not_running.as_str(), "notRunning");
        assert!(not_running.data().is_none());

        let zero = connections_readout(Some(HashMap::new()));
        assert_eq!(zero.as_str(), "ready");
        assert!(zero.data().expect("ready").is_empty());
        assert_ne!(
            zero.as_str(),
            not_running.as_str(),
            "「接続 0 件」と「走っていない」が同じ値になっている"
        );

        let statuses: HashMap<String, ConnectionStatus> = [
            ("conn:1".to_string(), ConnectionStatus::Connected),
            (
                "conn:2".to_string(),
                ConnectionStatus::Reconnecting { attempt: 3 },
            ),
            ("conn:3".to_string(), ConnectionStatus::Stopped),
        ]
        .into_iter()
        .collect();
        let json = serde_json::to_value(connections_readout(Some(statuses))).expect("serialize");
        assert_eq!(json["state"], "ready");
        assert_eq!(
            json["data"]["conn:1"],
            serde_json::json!({"status": "connected"})
        );
        assert_eq!(
            json["data"]["conn:2"],
            serde_json::json!({"status": "reconnecting", "attempt": 3}),
            "再接続の試行回数はヘルス表示の実用情報なので落とさない"
        );
        assert_eq!(
            json["data"]["conn:3"],
            serde_json::json!({"status": "stopped"})
        );
    }

    /// `collect_events` に 1 行入れる。**`detail` を呼び出し側が決められる**
    /// ので、漏れの検査に使える。`EventSink::emit` は `banto-collect` の
    /// `pub(crate)` なので、ここは同じ列に直接書く。
    async fn seed_event(pool: &SqlitePool, ts_ms: i64, kind: &str, detail: Option<&str>) {
        sqlx::query(
            "INSERT INTO collect_events (ts, kind, connection_key, tag_key, level, value, detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ts_ms)
        .bind(kind)
        .bind("conn:1")
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<f64>)
        .bind(detail)
        .execute(pool)
        .await
        .expect("insert collect_events");
    }

    /// **イベント一覧の 3 状態**を表で固定する:
    ///
    /// 1. 記録が 1 件も無い → **読めて 0 件**（`notRunning` ではない）、
    /// 2. **収集は止まったまま**記録が 3 件 → **読める**（過去の記録を
    ///    「走っていないので」と隠さない - このモジュールの doc
    ///    「イベント一覧だけ「走っていない」を返さない」）、
    /// 3. DB が読めない → **読めなかった**（`Unavailable`。**0 件ではない**）。
    ///
    /// 3 の作り方はプールを閉じること - 実際に起こりうる「読めない」
    /// （終了処理中のアクセス、ファイルが失われた等）と同じ経路で
    /// `sqlx` がエラーを返す。
    ///
    /// 反証（回帰の検出）: [`CollectorService::events`] の
    /// `Ok(Err(err)) => Readout::Unavailable` を
    /// `Readout::Ready { data: CollectEventList { rows: vec![], total_count: 0, as_of_id: 0 } }` に
    /// 変えると 3 の `assert_eq!` が落ちる（読めなかったのに「0 件です」と
    /// 答えている）。収集の状態を見て `NotRunning` を返す実装にすると 2 が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_events_list_tells_unreadable_apart_from_zero_rows_and_stays_readable_while_stopped(
    ) {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        assert_eq!(
            svc.state(),
            CollectorState::Stopped,
            "前提: 収集は動いていない"
        );

        // 1. 1 件も無い。
        let empty = svc.events(EventPage::default()).await;
        assert_eq!(
            empty.as_str(),
            "ready",
            "記録が無いだけなのに「読めなかった」/「走っていない」と答えている: {empty:?}"
        );
        let page = empty.data().expect("ready");
        assert!(page.rows.is_empty());
        assert_eq!(page.total_count, 0);

        // 2. 収集は止まったままでも、記録は読める。
        seed_event(&pool, 100, "collection_started", None).await;
        seed_event(&pool, 200, "plc_connected", None).await;
        seed_event(&pool, 300, "collection_stopped", None).await;
        let listed = svc.events(EventPage::default()).await;
        let page = listed
            .data()
            .unwrap_or_else(|| panic!("停止中に過去のイベントが読めない: {listed:?}"));
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.total_count, 3);
        assert_eq!(
            page.rows[0].kind, "collection_stopped",
            "新しい順になっていない: {:?}",
            page.rows
        );

        // 3. 読めない。
        pool.close().await;
        let unreadable = svc.events(EventPage::default()).await;
        assert_eq!(
            unreadable.as_str(),
            "unavailable",
            "読めなかったのに「0 件」と答えている: {unreadable:?}"
        );
        assert!(unreadable.data().is_none());
    }

    /// ページングは**新しい順**で、総件数は**ページングの前**の件数
    /// （`crate::audit::AuditLogService::list` と同じ形）。**同じ `ts` の行が
    /// あっても**ページ境界で重複・欠落しないこと - 並び順の第 2 キーに
    /// `id` を入れてあるのはそのため。
    ///
    /// 反証（回帰の検出）: `read_events` の `ORDER BY` から `, id DESC` を
    /// 外すと、同じ `ts` の 2 行の並びが不定になり、最後の
    /// 「全 id がちょうど 1 回ずつ」が落ちうる（SQLite は同順の行の順序を
    /// 保証しない）。`total_count` をページ内の件数に変えると `assert_eq!` が
    /// 落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_events_list_pages_newest_first_and_reports_the_total_before_paging() {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        // ts が重複する行を混ぜる（500 が 2 行）。
        for ts in [100, 200, 300, 500, 500] {
            seed_event(&pool, ts, "plc_connected", None).await;
        }

        let mut seen: Vec<i64> = Vec::new();
        let mut offset = 0;
        loop {
            let readout = svc.events(EventPage::new(Some(offset), Some(2))).await;
            let page = readout.data().expect("ready");
            assert_eq!(
                page.total_count, 5,
                "総件数はページングの前の件数であること: {page:?}"
            );
            if page.rows.is_empty() {
                break;
            }
            assert!(page.rows.len() <= 2, "limit を超えて返している: {page:?}");
            seen.extend(page.rows.iter().map(|row| row.id));
            offset += 2;
        }

        // 新しい順（ts 降順、同 ts は id 降順）= 5,4,3,2,1。
        assert_eq!(
            seen,
            vec![5, 4, 3, 2, 1],
            "ページを跨いだ並びが「新しい順」で一意になっていない"
        );
    }

    /// **公開型が内部情報を持たないこと**（#407 レビュー P2-2 と同じ作法）:
    /// `collect_events.detail` は自由文で、切断理由（接続先を含みうる）や
    /// 書き込みエラー（ファイルパスを含みうる）がそのまま入る列なので、
    /// **ワイヤに出さない**。目印を仕込んで、出てこないことを固定する。
    ///
    /// 落とすのは `detail` **だけ**で、種類・時刻・鍵は残る（画面が表を
    /// 描けなくなっては本末転倒）ことも同時に見る。
    ///
    /// 反証（回帰の検出）: [`CollectEventRow`] に `detail` を足して
    /// `read_events` の `SELECT` にも足すと、目印の `assert!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_row_never_carries_the_free_text_detail() {
        const MARKER: &str = "CHRONOGAZER-LEAK-CANARY";
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;
        seed_event(
            &pool,
            100,
            "plc_disconnected",
            Some(&format!("接続エラー: {MARKER} への接続に失敗しました")),
        )
        .await;

        let json = serde_json::to_value(svc.events(EventPage::default()).await).expect("serialize");
        assert_eq!(json["state"], "ready");
        assert!(
            !json.to_string().contains(MARKER),
            "自由文の detail がワイヤに漏れている: {json}"
        );
        let row = &json["data"]["rows"][0];
        assert!(
            row.get("detail").is_none(),
            "`detail` フィールドがワイヤに出ている: {row}"
        );
        // 落とすのは detail だけ。
        assert_eq!(row["kind"], "plc_disconnected");
        assert_eq!(row["connectionKey"], "conn:1");
        assert_eq!(row["tsMs"], 100);
        assert_eq!(
            json["data"]["totalCount"], 1,
            "総件数の綴りは監査一覧と同じ"
        );
        assert_eq!(
            json["data"]["asOfId"], 1,
            "使ったスナップショット境界が応答に無い: {json}"
        );
    }

    /// **スナップショット境界**（#409 レビュー P2-2）: 画面は 1 つの世代の
    /// 最初の応答で `asOfId` を固定し、後続ブロックに渡す。その間に足された
    /// イベントは**件数にも行にも入らない**ので、ブロック境界で重複も欠落も
    /// 起きない。表が空のときの境界は `0`（どの行も含まない）。
    ///
    /// 反証（回帰の検出）: `read_events` が `page.as_of_id` を無視して常に
    /// その時点の最大 `id` を使うと、2 ブロック目が `id 2` を返し
    /// （1 ブロック目の末尾と重複）、`id 1` が漏れて「全 id がちょうど 1 回
    /// ずつ」と件数の `assert_eq!` が落ちる。`COUNT(*)` から `WHERE id <= ?` を
    /// 外すと件数の `assert_eq!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_events_list_pins_a_snapshot_so_events_added_between_blocks_neither_shift_nor_count(
    ) {
        let dir = TempDir::new();
        let (pool, svc) = service(&dir).await;

        // 表が空: 境界は 0。後から足しても、境界 0 の集合は空のまま。
        let empty = svc.events(EventPage::default()).await;
        let page = empty.data().expect("ready");
        assert_eq!(page.as_of_id, 0, "空の表の境界は 0: {page:?}");
        assert_eq!(page.total_count, 0);

        for ts in [100, 200, 300] {
            seed_event(&pool, ts, "plc_connected", None).await;
        }
        let pinned_empty = svc.events(EventPage::default().as_of(Some(0))).await;
        let page = pinned_empty.data().expect("ready");
        assert!(page.rows.is_empty(), "境界 0 なのに行が入った: {page:?}");
        assert_eq!(page.total_count, 0);
        assert_eq!(page.as_of_id, 0, "渡した境界をそのまま返すこと");

        // 1 ブロック目（境界未指定 = その時点の最大 id = 3）。
        let first = svc.events(EventPage::new(Some(0), Some(2))).await;
        let first = first.data().expect("ready").clone();
        assert_eq!(first.as_of_id, 3);
        assert_eq!(first.total_count, 3);

        // ブロックの合間に新しいイベントが先頭へ足される（id 4、ts 最新）。
        seed_event(&pool, 400, "plc_disconnected", None).await;

        // 2 ブロック目は 1 ブロック目の境界で取る。
        let second = svc
            .events(EventPage::new(Some(2), Some(2)).as_of(Some(first.as_of_id)))
            .await;
        let second = second.data().expect("ready").clone();
        assert_eq!(second.as_of_id, first.as_of_id);
        assert_eq!(
            second.total_count, 3,
            "境界の後に足されたイベントが件数に入った: {second:?}"
        );
        let ids: Vec<i64> = first
            .rows
            .iter()
            .chain(second.rows.iter())
            .map(|row| row.id)
            .collect();
        assert_eq!(
            ids,
            vec![3, 2, 1],
            "ブロック境界で重複・欠落した（境界の後の行が混ざった）"
        );

        // 新しい世代（境界を外す）では新しいイベントが入る。
        let fresh = svc.events(EventPage::new(Some(0), Some(2))).await;
        let fresh = fresh.data().expect("ready").clone();
        assert_eq!(fresh.as_of_id, 4);
        assert_eq!(fresh.total_count, 4);
        assert_eq!(fresh.rows[0].id, 4);
    }

    /// [`EventPage::new`] の丸め（純関数）。`?limit=` は URL に誰でも書けるので
    /// **上限を掛ける**（この口は `viewer` にも開いている）。`0` を素通しに
    /// しないのは、「0 件読めました」という**意味の無い `Ready`** を作らない
    /// ため。
    #[test]
    fn an_event_page_clamps_its_limit_and_defaults_to_the_audit_logs_page_size() {
        assert_eq!(
            EventPage::new(None, None),
            EventPage {
                offset: 0,
                limit: COLLECT_EVENTS_DEFAULT_LIMIT,
                as_of_id: None
            }
        );
        assert_eq!(
            EventPage::new(None, None).as_of(Some(7)).as_of_id,
            Some(7),
            "スナップショット境界が落ちている"
        );
        assert_eq!(EventPage::new(Some(20), Some(10)).offset, 20);
        assert_eq!(EventPage::new(None, Some(10)).limit, 10);
        assert_eq!(
            EventPage::new(None, Some(0)).limit,
            1,
            "0 件だけ読む要求を素通しにしている"
        );
        assert_eq!(
            EventPage::new(None, Some(u64::MAX)).limit,
            COLLECT_EVENTS_MAX_LIMIT,
            "表全体を 1 リクエストで抜ける"
        );
        assert_eq!(
            EventPage::new(Some(u64::MAX), None).offset,
            i64::MAX as u64,
            "巨大な offset が負に化けて「先頭ページ」に戻ってしまう"
        );
    }

    // --- data.dir の解決 --------------------------------------------------

    #[test]
    fn relative_data_dir_resolves_against_the_app_data_dir() {
        let base = Path::new("C:/appdata");
        assert_eq!(
            resolve_data_dir(base, "./data"),
            base.join("./data"),
            "既定の相対パスは作業ディレクトリではなくデータディレクトリ基準"
        );
    }

    #[test]
    fn absolute_data_dir_is_used_as_is() {
        let base = Path::new("C:/appdata");
        let absolute = if cfg!(windows) { "D:/ts" } else { "/var/ts" };
        assert_eq!(resolve_data_dir(base, absolute), PathBuf::from(absolute));
    }

    #[test]
    fn blank_data_dir_falls_back_to_the_default_location() {
        let base = Path::new("C:/appdata");
        assert_eq!(resolve_data_dir(base, "   "), base.join("data"));
    }
}
