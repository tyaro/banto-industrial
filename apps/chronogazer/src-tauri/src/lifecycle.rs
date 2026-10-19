//! シェルのライフサイクルの状態機械（#392 B2-4c の後、2026-10-15 オーナー決定）。
//!
//! 「終わってよいか・今どの状態か・何をどれだけ待つか」の判断を、**1 つのロック**と
//! **1 つの表**（状態 × 出来事 → 許す／断る（理由）／待つ（上限））に集めたもの。
//! PR #608 では、終了 × 移り変わり × 復旧の競合の指摘が同じ辺りで 1 件ずつ出続けた。
//! 判断が `ExitCleanupOnce`（終了の後始末の 1 回化）・`QuitGate`（トレイの終了の確認の
//! 段階）・`ShellGate`（移り変わりの札・世代・終了の扉・復旧の札・終了の予約）・
//! `RecoveryGate`（対処の窓の処理中）・`exit_budgets`（待ちの上限）・`RunEvent::Exit` の
//! 扱いに散っていて、入り口を足すたびにその場で確かめ方を書き直していたため。
//!
//! ここにあるのは Tauri に依存しない部分だけ:
//!
//! - **純関数の核** [`step`]`(state, event) -> (state, decision)`。判断はすべてここで、
//!   表のテスト（下の「表」を、実装から導かずに書き下したもの）で総当たりにする。
//! - **薄い入れ物** [`Lifecycle`]（`Mutex` + `Condvar`）。出来事を 1 つずつロックの中で
//!   [`step`] に通し、札が返ったら待っている側を起こす。待つ（[`Lifecycle::wait_idle`]）
//!   のは条件変数で、ロックを握ったまま待たない。
//!
//! 後始末を 1 回に限る仕組み（`banto_desktop_tray::ExitCleanupOnce`、banto-hub と共有）は
//! そのまま使う。いつ後始末へ進み、その前に何をどれだけ待つかをここが決める
//! （[`Lifecycle::begin_exit`] → `crate::exit_cleanup`）。トレイの終了の確認の段階は
//! 共有 crate の純関数（`decide_request`・`decide_answer`）をこの表の中で使う
//! （`QuitGate` の入れ物は使わない。共有 crate の振る舞いは変えていない）。
//!
//! # 状態（[`LifecycleState`]）
//!
//! 状態は直交するいくつかの印の組で、表で使う名前（[`Phase`]）はその見方:
//!
//! | 名前            | 印                                                           |
//! | --------------- | ------------------------------------------------------------ |
//! | Starting        | 画面がまだ決まっていない（`setup()` の起動の判定の前）       |
//! | Showing(画面)   | Desktop・Service・Fallback のどれかを見せている（札なし）    |
//! | Transitioning   | 移り変わりの札（見張り・「再試行」・対処の窓のやり直し）     |
//! | Recovering      | 復旧の札（対処の窓の「作り直す」「バックアップから戻す」）   |
//! | QuitReserved    | トレイの終了が確定した（扉はまだ開いている）                 |
//! | Exiting         | 終了の扉が閉じた（`exit_cleanup` に入った）                  |
//!
//! ほかに、表の判断に効く印として「終了の確認中（ダイアログが出ている）」
//! （`quit == Confirming`）・「エンジンを置いた」（`engine`、`start()` の直後に置く
//! `DesktopEngine`）・「対処の窓の処理中」（`recovery_action`）・画面の世代と見張りの
//! 不健康の回数がある。印は組み合わさる（例: 移り変わりの最中にトレイの終了が確定する、
//! 扉が閉じた後に移り変わりの終わりを待つ）。名前は優先の順（Exiting → QuitReserved →
//! Recovering → Transitioning → Starting → Showing）で 1 つに決める。
//!
//! # 表（[`step`]。テスト `transition_table` が同じ表を書き下して総当たりにする）
//!
//! 列の状態（テストの代表）: **St** = Starting、**D** = Desktop（エンジンあり）、
//! **Df** = Desktop（起動に失敗して対処の窓、エンジンなし）、**S** = Service、
//! **F** = Fallback、**Fc** = Fallback で終了の確認中、**T** = Fallback から移り変わり中
//! （エンジンなし）、**Te** = 同じくエンジンを置いた後、**R** = 復旧中（対処の窓は処理中）、
//! **Rb** = 対処の窓が処理中（札なし。フォルダの選択など）、**Q** = 終了の確定（Desktop）、
//! **Qt** = 終了の確定（Fallback の移り変わり中）、**X** = 扉が閉じた（Desktop）、
//! **Xt** = 扉が閉じた（移り変わり中）、**Xo** = OS の終了で扉が閉じた（Service、確認なし）。
//!
//! 判断の略号: `go` = 許す（札を出す・進む）、`ok` = 反映した、`ign` = 何もしない、
//! `exit`/`busy`/`stale`/`nofb`/`rec`/`conf`/`quit`/`qing`/`rbusy` = 断る（終了処理中・
//! 別の札がある・観測が古い・Fallback でない・復旧中・確認中・終了処理中（トレイ）・
//! 終了が確定している・対処の窓が処理中）、`eng` = 断る（エンジンが置かれている。
//! 注 2）、`obs`/`skip`/`stop` = 見張りの観測・今回は見ない・見張りを終える、
//! `ask`/`noask` = 確認を出す／出さずに同意として進む（Fallback）、
//! `proceed`/`again`/`cancel` = 確認の答え（終了へ進む・収集中の文言で確かめ直す・
//! 取り消し）、`w60`/`w5`/`nowait` = 移り変わりの終わりを 60 秒・5 秒待つ・待たない。
//!
//! | 出来事（入り口）                                   | St     | D       | Df      | S       | F       | Fc      | T      | Te     | R      | Rb      | Q       | Qt     | X      | Xt     | Xo     |
//! | -------------------------------------------------- | ------ | ------- | ------- | ------- | ------- | ------- | ------ | ------ | ------ | ------- | ------- | ------ | ------ | ------ | ------ |
//! | 起動の判定（`shell::start`）                       | ok     | ign     | ign     | ign     | ign     | ign     | ign    | ign    | ign    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 見張りの控え（`monitor`）                          | stop   | stop    | stop    | obs     | obs     | obs     | skip   | skip   | stop   | stop    | stop    | skip   | stop   | stop   | stop   |
//! | 不健康の回数（同じ世代）                           | ok     | ok      | ok      | ok      | ok      | ok      | ign    | ign    | ign    | ok      | ok      | ign    | ok     | ign    | ok     |
//! | 不健康の回数（古い世代）                           | ign    | ign     | ign     | ign     | ign     | ign     | ign    | ign    | ign    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 見張りの適用（同じ世代）                           | go     | go      | go      | go      | go      | go      | busy   | busy   | busy   | go      | go      | busy   | exit   | exit   | exit   |
//! | 見張りの適用（古い世代）                           | stale  | stale   | stale   | stale   | stale   | stale   | busy   | busy   | busy   | stale   | stale   | busy   | exit   | exit   | exit   |
//! | 「再試行」（Fallback の窓・トレイ）                | nofb   | nofb    | nofb    | nofb    | go      | go      | busy   | busy   | busy   | nofb    | nofb    | busy   | exit   | exit   | exit   |
//! | 対処の後の起動（確かめ直す・既定・選び直す）（注 1）| go     | go      | go      | go      | go      | go      | busy   | busy   | busy   | go      | go      | busy   | exit   | exit   | exit   |
//! | 復旧の札（作り直す・戻す）（注 2）                 | go     | eng     | go      | go      | go      | go      | busy   | busy   | busy   | go      | exit    | exit   | exit   | exit   | exit   |
//! | 札を返す（新しい画面を置く）                       | ign    | ign     | ign     | ign     | ign     | ign     | ok     | ok     | ok     | ign     | ign     | ok     | ign    | ok     | ign    |
//! | 札を返す（取りやめ）                               | ign    | ign     | ign     | ign     | ign     | ign     | ok     | ok     | ok     | ign     | ign     | ok     | ign    | ok     | ign    |
//! | 起動を続けてよいか（`launch_desktop`）             | go     | go      | go      | go      | go      | go      | go     | go     | go     | go      | go      | go     | exit   | exit   | exit   |
//! | エンジンを置く（`start_and_register`）             | go     | go      | go      | go      | go      | go      | go     | go     | go     | go      | go      | go     | exit   | exit   | exit   |
//! | トレイ・Fallback の「終了」                        | ask    | ask     | ask     | ask     | noask   | conf    | noask  | noask  | rec    | ask     | quit    | quit   | quit   | quit   | ask    |
//! | 確認の答え: 取り消し                               | ign    | ign     | ign     | ign     | ign     | cancel  | ign    | ign    | ign    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 確認の答え: 同意（暇 → 暇）                        | ign    | ign     | ign     | ign     | ign     | proceed | ign    | ign    | rec    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 確認の答え: 同意（暇の文言 → 今は収集中）          | ign    | ign     | ign     | ign     | ign     | again   | ign    | ign    | rec    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 確認の答え: 同意（収集中の文言 → 収集中）          | ign    | ign     | ign     | ign     | ign     | proceed | ign    | ign    | rec    | ign     | ign     | ign    | ign    | ign    | ign    |
//! | 終了の後始末（トレイ・Fallback の「終了」）        | nowait | nowait  | nowait  | nowait  | nowait  | nowait  | w60    | w60    | w60    | nowait  | nowait  | w60    | nowait | w60    | nowait |
//! | 終了の後始末（`RunEvent::Exit` = OS の終了を含む） | nowait | nowait  | nowait  | nowait  | nowait  | nowait  | w5     | nowait | w5     | nowait  | nowait  | w5     | nowait | w5     | nowait |
//! | 窓を開く（「開く」・左クリック・2 つ目の起動）     | go     | go      | go      | go      | go      | go      | go     | go     | go     | go      | qing    | qing   | qing   | qing   | go     |
//! | トレイの書き換え（ふつう）                         | go     | go      | go      | go      | go      | go      | go     | go     | go     | go      | qing    | qing   | qing   | qing   | go     |
//! | トレイの書き換え（「終了処理中」）                 | go     | go      | go      | go      | go      | go      | go     | go     | go     | go      | go      | go     | go     | go     | go     |
//! | 対処の窓のボタン（注 1）                           | go     | go      | go      | go      | go      | go      | go     | go     | rbusy  | rbusy   | go      | go     | go     | go     | go     |
//! | 対処の窓の処理の終わり                             | ok     | ok      | ok      | ok      | ok      | ok      | ok     | ok     | ok     | ok      | ok      | ok     | ok     | ok     | ok     |
//! | 対処の窓の ×                                       | go     | go      | go      | go      | go      | go      | go     | go     | rbusy  | rbusy   | go      | go     | go     | go     | go     |
//! | 運用モードへ（設定画面。注 3）                     | nodesk | go      | go      | nodesk  | nodesk  | conf    | busy   | busy   | rec    | rbusy   | quit    | quit   | exit   | exit   | exit   |
//! | モードの切り替えの札（注 4）                       | nosvc  | nosvc   | nosvc   | go      | go      | go      | busy   | busy   | busy   | nosvc   | quit    | quit   | exit   | exit   | exit   |
//!
//! 表の注:
//!
//! - **注 1（Q・Qt の「対処の窓のボタン」「対処の後の起動」が `go`）**: 終了が確定しても
//!   扉はまだ開いているので、対処の窓の操作は受け付ける（旧実装と同じ）。意図したもの:
//!   ファイルの名前を変える対処（作り直す・戻す）は次の「復旧の札」の行が `exit` で断り
//!   （対処の窓に「終了処理中のため、この対処を始めませんでした」）、DB の復旧が終了で
//!   中断されることはない。ファイルを変えない対処（確かめ直す・既定に戻す・選び直す）の
//!   後の起動は札を取って `launch_desktop` に入るが、トレイの終了は確定の直後に専用の
//!   スレッドで扉を閉じ（`exit_cleanup` → [`Lifecycle::begin_exit`]）、起動は「起動を
//!   続けてよいか」「エンジンを置く」の行（`exit`）で止まる（置く前なら
//!   `start_and_register` がその場で止める）。扉より先に置けたエンジンは、後始末が札の
//!   終わりを待って（トレイの終了は 60 秒まで）見つけて止める。どちらの経路も最終 flush
//!   を通る。
//! - **注 2（D の「復旧の札」が `eng`）**: エンジンが置かれた後は復旧の札を出さない
//!   （PR #611 のレビュー）。OS の終了の「エンジンが置かれていれば移り変わりを待たない」
//!   （[`step`] の `ExitBegin`）は、**待たずに進んでも DB の復旧（名前の変更・リストアの
//!   適用）の途中で切れない**ことを前提にしている。前提が成り立つ理由: 復旧の札を取るのは
//!   エンジンが無いときだけ（この行。今の配線でも「作り直す」「戻す」を出すのは DB を
//!   開けなかったときだけで、エンジンを置いた後の失敗（画面を作れない）は「終了」だけの
//!   窓）で、札を持ったままエンジンが置かれるのは、対処の後の起動（`launch_desktop`）が
//!   `start()` を終えた後だけ。名前の変更・リストアの適用は `launch_desktop` の**前**に
//!   済んでいるので、その後に待たずに後始末へ進んでも復旧は半端にならない（置いた
//!   エンジンは後始末が止める）。つまり「復旧の札とエンジン」の組は存在するが、
//!   **エンジンを置く出来事でしか作られない**（約束のテストで確かめる）。`ExitBegin` に
//!   「この組が無い」という `debug_assert!` は置けない（対処の後の起動の最中に OS の終了が
//!   来ると正当にこの組になる）。
//!
//! - **注 3（運用モードへ、#623 PR-4）**: 設定画面の「運用モードへ切り替える」。この PC の中の
//!   エンジンは A1 の排他をプロセスの終わりまで持つ（`DesktopEngine` は外せない）ので、
//!   切り替えは**終了と同じ**: 許すと終了が確定し（`quit = Quitting`、確認ダイアログの同意と
//!   同じ印）、後始末（エンジンの停止と最終 flush）の後に新しいシェルへ続きを渡してプロセスを
//!   終える（`chronogazer_core::mode_switch` の「引き継ぎ」）。Desktop の画面のときだけ（`nodesk`）。
//!   確認中・復旧中・対処の窓の処理中・移り変わり中・終了の後は断る（終了の行と同じ理由）。
//!   「Desktop から他の画面へは移らない」の約束はそのまま（移るのは新しいプロセス）。
//! - **注 4（モードの切り替えの札、#623 PR-4）**: トレイの「設定モードに切り替える」、Fallback の窓の
//!   [運用を再開]・[設定モードに切り替える]・[設定モードに戻す]、引き継いだ新しいシェルの
//!   「運用モードへ」の続き。札の種類は移り変わり（`Transition`）で、Service・Fallback の画面の
//!   ときだけ（`nosvc`。Desktop は上の注 3 の道）。終了が確定した後は始めない（`quit`。サービスへ
//!   開始・停止を出してから終了が走ると、切り替えが途中で見捨てられる）。
//!
//! 表に出てこない約束（テスト `invariants_hold_from_every_state` が印のすべての組で
//! 確かめる）:
//!
//! - 扉が閉じた後は、札（移り変わり・復旧）もエンジンを置くことも起動を続けることも
//!   許さない。扉は二度と開かない。終了の確定（QuitReserved）も戻らない。
//! - 見張りは Desktop を観測しない（Desktop・Starting では見張りを終える。移り変わりの
//!   判断 `shell::on_tick` が Desktop を返さないことは `shell` の表のテスト）。
//! - 復旧の札と終了の確定は同時に立たない（どちらも同じロックの中で相手を見て断る）。
//! - 復旧の札を取るのはエンジンが無いときだけ。復旧の札とエンジンが並ぶのは、札を
//!   持ったままエンジンを置いた（対処の後の起動が `start()` を終えた）ときだけ（表の注 2）。
//! - 札は常に 1 つ。見張りの古い世代の適用は断る。世代は札を返して新しい画面を置いた
//!   ときだけ 1 増える。
//! - OS の終了は、移り変わりの待ちと後始末で 1 つの 5 秒の期限を分け合う
//!   （[`exit_budgets`]）。エンジンが既に置かれていれば移り変わりを待たない。
//!
//! # 入り口を足すとき
//!
//! 1. [`Event`] に出来事を 1 つ足す（入り口ごとに 1 つ。似ていても別の行にする）。
//! 2. [`step`] にその判断を書く（`match` は網羅なので、書かないとコンパイルが通らない）。
//! 3. テストの `transition_table` に行を 1 つ足し、15 の列すべての期待値を書き下す
//!    （実装から導かない）。`all_events` にも足す（約束のテストがその出来事も通す）。
//! 4. 入り口の側は [`Lifecycle`] の口を 1 つ呼んで判断に従うだけにする（その場で
//!    別の印を読んで確かめ直さない）。
//! 5. この doc の表と設計書（`docs/chronogazer-service-design.md` §4 の B2-4c の後の
//!    メモ）の表に同じ行を足す。
//!
//! # ロックと待ち
//!
//! - ロックは 1 つ（[`Lifecycle`] の `Mutex`）。ロックを握ったまま呼ぶ外の処理は 2 つ
//!   だけ: エンジンを置く `install`（[`Lifecycle::admit`]）と、終了の確認の答えでの収集の
//!   受付（`QuitAdmission`、[`Lifecycle::answer_quit`]）。ロックの順序は「この状態機械 →
//!   収集の受付」の一方向（収集側はここを参照しない）。窓・トレイの操作はロックの外で行う。
//! - **ロックを握ったまま呼ぶ処理（`install`・`QuitAdmission`）の中では**（PR #611 の
//!   オーナーレビュー 2）:
//!   - **この状態機械（`Lifecycle` のどのメソッドも）を呼ばない** - 同じロックをもう一度
//!     取りに行って止まる（`std::sync::Mutex` は再入できない）。
//!   - **メインスレッドを待たない**（窓・トレイ・ダイアログの操作をしない）- メインスレッドが
//!     このロックを待っていると互いに止まる。
//!   - してよいのは即時に終わる登録・読み取りだけ（`install` なら `AppHandle::manage`、
//!     収集の受付なら収集側の短いロックの中の印の上げ下げ）。
//!
//!   デバッグビルドでは、この約束を破った再入を検知して panic する（スレッドローカルの
//!   印。ロックを握ったまま外の処理を呼ぶ間だけ立て、その間にこのスレッドがロックを
//!   取りに来たら `debug_assert!` で落とす。止まるより先に落ちるので原因が分かる）。
//!   リリースビルドには入らない。
//! - 待つのは [`Lifecycle::wait_idle`]（条件変数。待つ間はロックを手放す）だけで、上限は
//!   [`exit_budgets`] が出来事ごとに決める。メインスレッドから呼ぶのは `RunEvent::Exit`
//!   だけで、その上限は後始末と合わせて 5 秒。トレイの終了は専用のスレッドで待つ。

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use banto_desktop_tray::{
    decide_answer, decide_request, QuitAdmission, QuitAnswerDecision, QuitPhase,
    QuitRequestDecision,
};

use crate::shell::{FallbackState, ShellView};

// ---------------------------------------------------------------------------
// 再入の検知（デバッグビルドだけ。モジュール doc「ロックと待ち」）
// ---------------------------------------------------------------------------

#[cfg(debug_assertions)]
thread_local! {
    /// このスレッドが、ロックを握ったまま外の処理（`install`・`QuitAdmission`）を
    /// 呼んでいる最中か。
    static IN_LOCKED_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// ロックを握ったまま外の処理 `f` を呼ぶ（デバッグビルドでは、その間の再入を検知する
/// 印を立てる。panic しても印は下ろす）。
fn call_under_lock<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(debug_assertions)]
    {
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                IN_LOCKED_CALLBACK.with(|flag| flag.set(self.0));
            }
        }
        let _reset = Reset(IN_LOCKED_CALLBACK.with(|flag| flag.replace(true)));
        f()
    }
    #[cfg(not(debug_assertions))]
    {
        f()
    }
}

// ---------------------------------------------------------------------------
// 待ちの上限（純関数）
// ---------------------------------------------------------------------------

/// How long the exit cleanup (`crate::shutdown_app_state`) is allowed to take in
/// total before the app gives up and exits anyway (#383 R1-C's prerequisite).
/// Both entries into the cleanup ([`ExitTrigger`]: the tray's 「終了」 and
/// `RunEvent::Exit`) use this same budget (as before #392 B1-2).
///
/// **Why a budget at all**: every step below can, in principle, wait on
/// something outside this process - the LAN server's graceful shutdown waits
/// for in-flight requests (a LAN browser's long-lived `GET /api/events` SSE
/// stream is exactly such a connection), and `SqlitePool::close` waits for
/// every checked-out connection to come back. A window closing must never
/// leave a process the user cannot see hanging around; **freezing on exit is
/// worse than losing some of the cleanup**, because the OS tears the sockets
/// and the file handles down for us anyway (SQLite recovers a `-wal` on the
/// next launch by design).
///
/// **Why 5 seconds**: everything here is local (a WS close frame to a Hub on
/// the same PC/LAN, an in-process axum shutdown, an SQLite close) and
/// normally completes in milliseconds. 5 seconds is far enough above that to
/// never cut a healthy shutdown short, and short enough that a user who
/// closed the window does not notice. Windows also gives an app roughly 5
/// seconds to handle `WM_ENDSESSION` (OS sign-out/shutdown) before it shows
/// 「このアプリがシャットダウンを妨げています」.
///
/// [`EXIT_CLEANUP_BUDGET`] は **5 秒のまま据え置いた**（#383 R1-C）。
/// 収集の停止は「接続タスクを join して tstore を最終 flush する」だけで、
/// どれもローカルのディスク操作。**実機で 5 秒に収まらないことが分かったら、
/// 勝手に増やさずオーナーに報告すること。**
///
/// 収集の停止は自分でも上限を持つ（`COLLECT_OPERATION_TIMEOUT` = 30 秒）が、
/// **この 5 秒の方が短いので、終了経路では必ずこちらが先に切る** - 窓を
/// 閉じた利用者を 30 秒待たせないため（意図どおり）。予算が切れても、
/// `CollectorService::stop` は**自分のライフサイクルタスクに依頼して待つ**
/// だけなので、打ち切られるのは**待つ側だけ**で、タスク側の停止処理
/// （接続タスクの join と最終 flush）は**そのまま続く**。直後にプロセスが
/// 終わるので実害は無く、失われうるのは最後の未 flush 分だけ - 詳しくは
/// `chronogazer_core::collect` のモジュール doc「終了フックからの停止」。
///
/// #392 B2-4c の後: `lib.rs` からこのモジュールへ移した（待ちの上限は状態機械の判断）。
pub(crate) const EXIT_CLEANUP_BUDGET: Duration = Duration::from_secs(5);

/// 自発的な終了（トレイ・Fallback の「終了」）が移り変わりを待つ上限。
pub(crate) const TRANSITION_WAIT_ON_QUIT: Duration = Duration::from_secs(60);

/// 終了の後始末に入った経路（記録の文言と待ちの上限 [`exit_budgets`] のため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitTrigger {
    /// トレイの「終了」（確認の後、`AppHandle::exit` の**前**）。Fallback の窓の
    /// 「終了」も同じ。
    TrayQuit,
    /// `tauri::RunEvent::Exit`。トレイの終了の `app.exit(0)` の後にも来るほか、
    /// OS のサインアウト・シャットダウンで来る（tao が `WM_ENDSESSION` で
    /// イベントループを畳み、戻った直後に `std::process::exit` する。`WM_ENDSESSION`
    /// を別に受ける口は無く、この 1 つの入り口に合流する）。
    RunEventExit,
}

impl ExitTrigger {
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExitTrigger::TrayQuit => "トレイの終了",
            ExitTrigger::RunEventExit => {
                "アプリの終了イベント（OS のサインアウト・シャットダウンを含む）"
            }
        }
    }
}

/// 終了の後始末の待ちの上限（[`exit_budgets`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitBudgets {
    /// シェルの移り変わりの終わりを待つ上限。
    pub(crate) transition_wait: Duration,
    /// 後始末の上限（`shared_deadline` なら移り変わりの待ちと分け合う全体）。
    pub(crate) cleanup: Duration,
    /// 移り変わりの待ちと後始末で 1 つの期限を分け合う。
    pub(crate) shared_deadline: bool,
}

impl ExitBudgets {
    /// 移り変わりを `waited` だけ待った後の、後始末の予算。
    pub(crate) fn cleanup_after(&self, waited: Duration) -> Duration {
        if self.shared_deadline {
            self.cleanup.saturating_sub(waited)
        } else {
            self.cleanup
        }
    }
}

/// 終了の起点ごとの待ちの上限（純関数。PR #608 の全体監査 F2・F3）。
///
/// - トレイの「終了」（Fallback の窓の「終了」を含む。専用のスレッドで待つ）: 移り
///   変わりは長く（60 秒）待つ - 短く切ると、「再試行」で起動し終えたエンジン（自動開始
///   した収集を含む）を止めずにプロセスが終わりうる。後始末は今までどおり 5 秒。
/// - `RunEvent::Exit`（OS のサインアウト・シャットダウン、メインスレッド）: 移り変わりの
///   待ちと後始末で **1 つの 5 秒の期限**を分け合う（OS の猶予を超えない）。
pub(crate) fn exit_budgets(trigger: ExitTrigger) -> ExitBudgets {
    match trigger {
        ExitTrigger::TrayQuit => ExitBudgets {
            transition_wait: TRANSITION_WAIT_ON_QUIT,
            cleanup: EXIT_CLEANUP_BUDGET,
            shared_deadline: false,
        },
        ExitTrigger::RunEventExit => ExitBudgets {
            transition_wait: EXIT_CLEANUP_BUDGET,
            cleanup: EXIT_CLEANUP_BUDGET,
            shared_deadline: true,
        },
    }
}

// ---------------------------------------------------------------------------
// 状態・出来事・判断
// ---------------------------------------------------------------------------

/// 札（移り変わり・復旧）。常に高々 1 つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ticket {
    None,
    /// 移り変わり（見張りの Service ↔ Fallback、「再試行」、対処の窓のやり直し）。
    Transition,
    /// DB の復旧（対処の窓の「作り直す」「バックアップから戻す」を受け付けた時点から、
    /// 起動のやり直しの終わりまで）。移り変わりの札に「復旧中」の印を足したもの。
    Recovery,
}

/// ライフサイクルの状態（モジュール doc「状態」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LifecycleState {
    /// 今の画面（`None` は起動の判定の前 = Starting）。
    pub(crate) view: Option<ShellView>,
    /// 画面が変わるたびに 1 増える（見張りの古い観測を捨てるため）。
    pub(crate) generation: u64,
    /// Service の画面で続けて不健康だった回数。
    pub(crate) streak: u32,
    pub(crate) ticket: Ticket,
    /// トレイの終了の段階（`Quitting` = 終了の確定 = 予約。新しい復旧を始めない）。
    pub(crate) quit: QuitPhase,
    /// 終了の扉が閉じた（`exit_cleanup` に入った）。二度と開かない。
    pub(crate) exiting: bool,
    /// エンジン（`DesktopEngine`）を置いた。
    pub(crate) engine: bool,
    /// 対処の窓の処理中（二度押しを無視し、窓の × を止める）。
    pub(crate) recovery_action: bool,
}

impl LifecycleState {
    /// 起動の判定の前。
    pub(crate) fn starting() -> Self {
        Self {
            view: None,
            generation: 0,
            streak: 0,
            ticket: Ticket::None,
            quit: QuitPhase::Idle,
            exiting: false,
            engine: false,
            recovery_action: false,
        }
    }

    /// 表で使う名前（モジュール doc「状態」の優先の順。判断には使わない - 判断は印を
    /// 直接見る。表のテストの列の意味を確かめるため）。
    #[cfg(test)]
    pub(crate) fn phase(&self) -> Phase {
        if self.exiting {
            Phase::Exiting
        } else if self.quit == QuitPhase::Quitting {
            Phase::QuitReserved
        } else {
            match (self.ticket, &self.view) {
                (Ticket::Recovery, _) => Phase::Recovering,
                (Ticket::Transition, _) => Phase::Transitioning,
                (Ticket::None, None) => Phase::Starting,
                (Ticket::None, Some(view)) => Phase::Showing(ViewKind::of(view)),
            }
        }
    }
}

/// 画面の種類（[`Phase::Showing`]）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewKind {
    Desktop,
    Service,
    Fallback,
}

#[cfg(test)]
impl ViewKind {
    fn of(view: &ShellView) -> Self {
        match view {
            ShellView::Desktop => ViewKind::Desktop,
            ShellView::Service { .. } => ViewKind::Service,
            ShellView::Fallback(_) => ViewKind::Fallback,
        }
    }
}

/// 状態の名前（モジュール doc「状態」）。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Showing(ViewKind),
    Transitioning,
    Recovering,
    QuitReserved,
    Exiting,
}

/// 出来事（入り口ごとに 1 つ。モジュール doc「入り口を足すとき」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// `shell::start`: 起動の判定で最初の画面が決まった。
    Started(ShellView),
    /// 見張りが観測の前に控えを取る。
    MonitorPoll,
    /// 見張りの不健康の回数を残す（控えの世代つき）。
    MonitorStreak { generation: u64, streak: u32 },
    /// 見張りの結果を適用する札（控えの世代のままなら）。
    MonitorApply { generation: u64 },
    /// Fallback の「再試行」（窓のボタン・トレイ）の札。
    Retry,
    /// 対処の窓の、ファイルを変えない対処（確かめ直す・既定に戻す・選び直す）の後の
    /// 起動のやり直しの札。
    RecoveryRelaunch,
    /// 対処の窓の「作り直す」「バックアップから戻す」の復旧の札。
    RecoveryBegin,
    /// 札を返して新しい画面を置く。
    TransitionFinish(ShellView),
    /// 札を返す（画面は変えない。失敗・取りやめ）。
    TransitionAbort,
    /// `launch_desktop` が続けてよいか（始める前・`start` の前・画面を作る前）。
    LaunchCheck,
    /// `start()` の直後にエンジンを置く（`start_and_register`）。
    EngineAdmit,
    /// トレイ・Fallback の窓の「終了」が押された。
    QuitRequest,
    /// 終了の確認の答え（確認しない態では同意として）。`now_busy` は、同意で確認中・
    /// 復旧中でないときだけ、収集の受付の印を立てて読んだ値（それ以外は使わない）。
    QuitAnswer {
        confirmed: bool,
        prompted_busy: bool,
        now_busy: bool,
    },
    /// 終了の後始末に入る（扉を閉じる）。
    ExitBegin(ExitTrigger),
    /// 窓を前に出す（トレイの「開く」・左クリック・2 つ目の起動）。
    OpenWindow,
    /// トレイの表示の書き換え（`quitting_label` は「終了処理中」の書き換え）。トレイの
    /// 状態の見張りも、これが断られたら抜ける。
    TrayRepaint { quitting_label: bool },
    /// 対処の窓のボタン（今の選択肢に含まれるものだけ、呼び出し側が確かめた後）。
    RecoveryAction,
    /// 対処の窓の処理が終わった（描き直した・新しい窓を出した・成功して閉じた）。
    RecoveryActionSettled,
    /// 対処の窓の ×。
    RecoveryWindowClose,
    /// 設定画面の「運用モードへ切り替える」（モジュール doc の表の注 3。#623 PR-4）。
    SwitchToOperation,
    /// モードの切り替えの札（モジュール doc の表の注 4。#623 PR-4）。
    ModeSwitch,
}

/// 断った理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// 別の札が出ている。
    Busy,
    /// 終了に入った（扉が閉じた）。復旧の札では、終了が確定した後も。
    Exiting,
    /// 観測の後に画面が変わった（見張りの古い結果）。
    Stale,
    /// 今の画面が Fallback ではない（古いメニューから押された「再試行」）。
    NotFallback,
    /// DB の復旧の途中（トレイの「終了」を断る）。
    RecoveryInProgress,
    /// 終了の確認ダイアログが既に出ている。
    AlreadyConfirming,
    /// 終了が既に確定している（終了処理を 2 回走らせない）。
    AlreadyQuitting,
    /// 終了が確定した後（窓を開かない・「終了処理中」を上書きしない）。
    Quitting,
    /// 対処の窓が処理中。
    RecoveryBusy,
    /// エンジンが置かれている（復旧の札を出さない。モジュール doc の表の注 2）。
    EngineRunning,
    /// 今の画面が Desktop ではない（運用モードへの切り替えは Desktop から。注 3）。
    NotDesktop,
    /// 今の画面が Service・Fallback ではない（モードの切り替えの札。注 4）。
    NotServiceSide,
}

/// 終了の確認の答えの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitAnswerOutcome {
    /// 終了処理へ進む（終了は確定し、新しい復旧を始めない）。
    Proceed,
    /// 収集中の文言で確かめ直す（収集の受付の印は下ろす）。
    AskAgain,
    /// DB の復旧の途中なので断った（確認の段階は戻した）。
    RecoveryInProgress,
    /// 取り消し。
    Cancelled,
    /// 確認中ではないのに届いた答え（何もしない）。
    Ignored,
}

/// [`step`] の判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// 反映した。
    Applied,
    /// 何もしない（この状態では意味の無い出来事・古い控え）。
    Ignored,
    Refused(Refusal),
    /// 許した（札を出した・進んでよい）。
    Proceed,
    /// 見張り: 観測してよい（控え）。
    Observe {
        view: ShellView,
        generation: u64,
        streak: u32,
    },
    /// 見張り: 移り変わりの最中なので今回は見ない。
    SkipTick,
    /// 見張りを終える（終了に入った・Desktop になった・画面が決まっていない）。
    StopMonitor,
    /// 終了: 確認を出す（`confirm`）／出さずに同意として進む（Fallback）。
    AskQuit {
        confirm: bool,
    },
    QuitAnswer(QuitAnswerOutcome),
    /// 終了の後始末へ: 移り変わりの終わりを `transition_wait` まで待ってから
    /// （`None` なら待たずに）後始末へ進む。
    Exit {
        budgets: ExitBudgets,
        transition_wait: Option<Duration>,
    },
}

/// 確認の答えで収集の受付の印を立てて読むか（純関数）。同意で、確認中で、復旧中で
/// ないときだけ（`QuitGate::answer` と同じ。断る・取り消し・遅れた答えでは触らない）。
pub(crate) fn needs_admission(state: &LifecycleState, confirmed: bool) -> bool {
    confirmed && state.quit == QuitPhase::Confirming && state.ticket != Ticket::Recovery
}

/// 状態機械の核（純関数。モジュール doc の表）。
pub(crate) fn step(state: &LifecycleState, event: &Event) -> (LifecycleState, Decision) {
    let mut next = state.clone();
    let decision = match event {
        Event::Started(view) => {
            if state.view.is_none() {
                next.view = Some(view.clone());
                Decision::Applied
            } else {
                Decision::Ignored
            }
        }
        Event::MonitorPoll => {
            let watched = matches!(
                state.view,
                Some(ShellView::Service { .. } | ShellView::Fallback(_))
            );
            if state.exiting || !watched {
                Decision::StopMonitor
            } else if state.ticket != Ticket::None {
                Decision::SkipTick
            } else {
                Decision::Observe {
                    view: state.view.clone().expect("watched view"),
                    generation: state.generation,
                    streak: state.streak,
                }
            }
        }
        Event::MonitorStreak { generation, streak } => {
            if *generation == state.generation && state.ticket == Ticket::None {
                next.streak = *streak;
                Decision::Applied
            } else {
                Decision::Ignored
            }
        }
        Event::MonitorApply { generation } => {
            grant_transition(state, &mut next, Some(*generation), false)
        }
        Event::Retry => grant_transition(state, &mut next, None, true),
        Event::RecoveryRelaunch => grant_transition(state, &mut next, None, false),
        Event::RecoveryBegin => {
            if state.exiting || state.quit == QuitPhase::Quitting {
                Decision::Refused(Refusal::Exiting)
            } else if state.ticket != Ticket::None {
                Decision::Refused(Refusal::Busy)
            } else if state.engine {
                // 前提の固定（モジュール doc の表の注 2）: 今の配線では来ない。
                // エンジンが置かれた後の復旧は、OS の終了が待たずに進む前提を崩す。
                Decision::Refused(Refusal::EngineRunning)
            } else {
                next.ticket = Ticket::Recovery;
                Decision::Proceed
            }
        }
        Event::TransitionFinish(view) => {
            if state.ticket == Ticket::None {
                Decision::Ignored
            } else {
                next.view = Some(view.clone());
                next.generation = state.generation.wrapping_add(1);
                next.streak = 0;
                next.ticket = Ticket::None;
                Decision::Applied
            }
        }
        Event::TransitionAbort => {
            if state.ticket == Ticket::None {
                Decision::Ignored
            } else {
                next.ticket = Ticket::None;
                Decision::Applied
            }
        }
        Event::LaunchCheck => {
            if state.exiting {
                Decision::Refused(Refusal::Exiting)
            } else {
                Decision::Proceed
            }
        }
        Event::EngineAdmit => {
            if state.exiting {
                Decision::Refused(Refusal::Exiting)
            } else {
                next.engine = true;
                Decision::Proceed
            }
        }
        Event::QuitRequest => match decide_request(state.quit) {
            (_, QuitRequestDecision::AlreadyConfirming) => {
                Decision::Refused(Refusal::AlreadyConfirming)
            }
            (_, QuitRequestDecision::AlreadyQuitting) => {
                Decision::Refused(Refusal::AlreadyQuitting)
            }
            // DB の復旧の途中なら確認も出さずに断る（確定の時点でも確かめ直す）。
            (_, QuitRequestDecision::ShowPrompt) if state.ticket == Ticket::Recovery => {
                Decision::Refused(Refusal::RecoveryInProgress)
            }
            (phase, QuitRequestDecision::ShowPrompt) => {
                next.quit = phase;
                // Fallback は止めるものが無いので確認しない（`tray` のモジュール doc）。
                let mode = crate::shell::tray_mode_of(state.view.as_ref());
                Decision::AskQuit {
                    confirm: crate::tray::needs_quit_confirm(&mode),
                }
            }
        },
        Event::QuitAnswer {
            confirmed,
            prompted_busy,
            now_busy,
        } => {
            let outcome = if !confirmed {
                let (phase, decision) = decide_answer(state.quit, false, *prompted_busy, false);
                next.quit = phase;
                match decision {
                    QuitAnswerDecision::Cancelled => QuitAnswerOutcome::Cancelled,
                    _ => QuitAnswerOutcome::Ignored,
                }
            } else if state.ticket == Ticket::Recovery {
                // 断る: 答えを「取り消し」として渡して確認の段階を戻す。
                let (phase, _) = decide_answer(state.quit, false, *prompted_busy, false);
                next.quit = phase;
                QuitAnswerOutcome::RecoveryInProgress
            } else {
                let (phase, decision) = decide_answer(state.quit, true, *prompted_busy, *now_busy);
                next.quit = phase;
                match decision {
                    QuitAnswerDecision::Proceed => QuitAnswerOutcome::Proceed,
                    QuitAnswerDecision::AskAgain => QuitAnswerOutcome::AskAgain,
                    QuitAnswerDecision::Cancelled => QuitAnswerOutcome::Cancelled,
                    QuitAnswerDecision::Ignored => QuitAnswerOutcome::Ignored,
                }
            };
            Decision::QuitAnswer(outcome)
        }
        Event::ExitBegin(trigger) => {
            // 前提（モジュール doc の表の注 2）: 下の「エンジンが置かれていれば待たない」は、
            // 待たずに進んでも DB の復旧の途中で切れないことを前提にする。復旧の札は
            // エンジンが無いときだけ取り、札を持ったままエンジンが置かれるのは、復旧（名前の
            // 変更・リストアの適用）が済んだ後の起動が `start()` を終えた後だけ。
            next.exiting = true;
            let budgets = exit_budgets(*trigger);
            // エンジンが既に置かれていれば、OS の終了では移り変わりを待たない（窓を作る
            // 移り変わりはメインスレッドを待つので終わらない。エンジンは見つかる）。
            let skip = budgets.shared_deadline && state.engine;
            let transition_wait =
                (state.ticket != Ticket::None && !skip).then_some(budgets.transition_wait);
            Decision::Exit {
                budgets,
                transition_wait,
            }
        }
        Event::OpenWindow => {
            if state.quit == QuitPhase::Quitting {
                Decision::Refused(Refusal::Quitting)
            } else {
                Decision::Proceed
            }
        }
        Event::TrayRepaint { quitting_label } => {
            if *quitting_label || state.quit != QuitPhase::Quitting {
                Decision::Proceed
            } else {
                Decision::Refused(Refusal::Quitting)
            }
        }
        Event::RecoveryAction => {
            if state.recovery_action {
                Decision::Refused(Refusal::RecoveryBusy)
            } else {
                next.recovery_action = true;
                Decision::Proceed
            }
        }
        Event::RecoveryActionSettled => {
            next.recovery_action = false;
            Decision::Applied
        }
        Event::RecoveryWindowClose => {
            if state.recovery_action {
                Decision::Refused(Refusal::RecoveryBusy)
            } else {
                Decision::Proceed
            }
        }
        Event::SwitchToOperation => {
            if state.exiting {
                Decision::Refused(Refusal::Exiting)
            } else if state.quit == QuitPhase::Quitting {
                Decision::Refused(Refusal::AlreadyQuitting)
            } else if state.quit == QuitPhase::Confirming {
                Decision::Refused(Refusal::AlreadyConfirming)
            } else if state.ticket == Ticket::Recovery {
                Decision::Refused(Refusal::RecoveryInProgress)
            } else if state.ticket != Ticket::None {
                Decision::Refused(Refusal::Busy)
            } else if state.recovery_action {
                Decision::Refused(Refusal::RecoveryBusy)
            } else if state.view != Some(ShellView::Desktop) {
                Decision::Refused(Refusal::NotDesktop)
            } else {
                // 終了の確定と同じ印（注 3）。以後は新しい復旧も窓を開くことも断る。
                next.quit = QuitPhase::Quitting;
                Decision::Proceed
            }
        }
        Event::ModeSwitch => {
            if state.exiting {
                Decision::Refused(Refusal::Exiting)
            } else if state.quit == QuitPhase::Quitting {
                Decision::Refused(Refusal::AlreadyQuitting)
            } else if state.ticket != Ticket::None {
                Decision::Refused(Refusal::Busy)
            } else if !matches!(
                state.view,
                Some(ShellView::Service { .. } | ShellView::Fallback(_))
            ) {
                Decision::Refused(Refusal::NotServiceSide)
            } else {
                next.ticket = Ticket::Transition;
                Decision::Proceed
            }
        }
    };
    (next, decision)
}

/// 移り変わりの札（見張りの適用・「再試行」・対処の後の起動）。
fn grant_transition(
    state: &LifecycleState,
    next: &mut LifecycleState,
    generation: Option<u64>,
    fallback_only: bool,
) -> Decision {
    if state.exiting {
        return Decision::Refused(Refusal::Exiting);
    }
    if state.ticket != Ticket::None {
        return Decision::Refused(Refusal::Busy);
    }
    if generation.is_some_and(|generation| generation != state.generation) {
        return Decision::Refused(Refusal::Stale);
    }
    if fallback_only && !matches!(state.view, Some(ShellView::Fallback(_))) {
        return Decision::Refused(Refusal::NotFallback);
    }
    next.ticket = Ticket::Transition;
    Decision::Proceed
}

// ---------------------------------------------------------------------------
// 入れ物（Mutex + Condvar）
// ---------------------------------------------------------------------------

/// DB の復旧の途中のトレイの「終了」を断ったときに利用者へ出す文。
pub(crate) const RECOVERY_IN_PROGRESS_MESSAGE: &str = "データベースの復旧の途中です。途中で終了すると復旧が半端なまま残るおそれがあるため、終了できません。対処の窓で復旧が終わってから、もう一度終了してください。";

/// 見張りが観測の前に取る控え。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PollSnapshot {
    /// 見張りを終える（終了に入った・Desktop になった）。
    Stop,
    /// 移り変わりの最中（今回は見ない）。
    Busy,
    /// 観測してよい。
    Ready {
        view: ShellView,
        generation: u64,
        streak: u32,
    },
}

/// トレイ・Fallback の「終了」が押されたときにすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitRequestOutcome {
    /// 確認を出す（`confirm`）／出さずに同意として進む（Fallback）。
    Ask { confirm: bool },
    /// DB の復旧の途中なので断る（知らせる）。
    RecoveryInProgress,
    /// 確認ダイアログが出ている・終了処理が走っている - 何もしない。
    AlreadyHandled,
}

/// 終了の後始末に入るときの計画（[`Lifecycle::begin_exit`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExitPlan {
    pub(crate) budgets: ExitBudgets,
    /// 移り変わりの終わりを待つ上限（`None` なら待たない）。
    pub(crate) transition_wait: Option<Duration>,
}

/// ライフサイクルの状態機械の入れ物（モジュール doc）。`run()` が `Arc` で manage する。
#[derive(Debug)]
pub(crate) struct Lifecycle {
    state: Mutex<LifecycleState>,
    /// 札が返ったときに起こす（[`Self::wait_idle`]）。
    idle: Condvar,
}

impl Lifecycle {
    /// 起動の判定の前（Starting）で作る。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LifecycleState::starting()),
            idle: Condvar::new(),
        })
    }

    /// 最初の画面が決まった状態で作る（テスト）。
    #[cfg(test)]
    pub(crate) fn started(view: ShellView) -> Arc<Self> {
        let lifecycle = Self::new();
        lifecycle.start(view);
        lifecycle
    }

    /// 中身は代入だけで書き換えるので、毒化は無視して中身を使う。デバッグビルドでは、
    /// ロックを握ったまま呼んだ外の処理からの再入を検知して panic する（モジュール doc
    /// 「ロックと待ち」。そのまま取りに行くと止まる）。
    fn lock(&self) -> MutexGuard<'_, LifecycleState> {
        // 印（`IN_LOCKED_CALLBACK`）がデバッグビルドにしか無いので、`cfg` で囲んで普通の
        // `assert!` にする（`debug_assert!` だと release でも式の型検査で印が要る）。
        #[cfg(debug_assertions)]
        assert!(
            !IN_LOCKED_CALLBACK.with(std::cell::Cell::get),
            "lifecycle の再入: ロックを握ったまま呼んだ install・QuitAdmission の中から lifecycle を呼んだ"
        );
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 出来事を 1 つ [`step`] に通す。札が返ったら待っている側を起こす。
    fn apply(&self, event: Event) -> Decision {
        let released = {
            let mut state = self.lock();
            let (next, decision) = step(&state, &event);
            let released = state.ticket != Ticket::None && next.ticket == Ticket::None;
            *state = next;
            (released, decision)
        };
        if released.0 {
            self.idle.notify_all();
        }
        released.1
    }

    /// 今の状態の写し（テスト）。
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> LifecycleState {
        self.lock().clone()
    }

    /// 今の画面（`None` は起動の判定の前）。
    pub(crate) fn view(&self) -> Option<ShellView> {
        self.lock().view.clone()
    }

    /// 終了の扉が閉じているか（テスト。本番の入り口は表の行 - [`Self::launch_may_proceed`]）。
    #[cfg(test)]
    pub(crate) fn is_exiting(&self) -> bool {
        self.lock().exiting
    }

    /// 復旧の途中か。
    #[cfg(test)]
    pub(crate) fn is_recovering(&self) -> bool {
        self.lock().ticket == Ticket::Recovery
    }

    /// 終了が確定しているか（トレイの「終了処理中」）。
    #[cfg(test)]
    pub(crate) fn quitting(&self) -> bool {
        self.lock().quit == QuitPhase::Quitting
    }

    /// 起動の判定で最初の画面が決まった。
    pub(crate) fn start(&self, view: ShellView) {
        self.apply(Event::Started(view));
    }

    /// 見張りの控え。
    pub(crate) fn poll(&self) -> PollSnapshot {
        match self.apply(Event::MonitorPoll) {
            Decision::Observe {
                view,
                generation,
                streak,
            } => PollSnapshot::Ready {
                view,
                generation,
                streak,
            },
            Decision::SkipTick => PollSnapshot::Busy,
            _ => PollSnapshot::Stop,
        }
    }

    /// 見張りの不健康の回数を残す（控えの後に画面が変わった・移り変わりの最中なら
    /// 捨てる）。
    pub(crate) fn record_streak(&self, generation: u64, streak: u32) {
        self.apply(Event::MonitorStreak { generation, streak });
    }

    fn ticket(self: &Arc<Self>, event: Event) -> Result<TransitionGuard, Refusal> {
        match self.apply(event) {
            Decision::Proceed => Ok(TransitionGuard {
                lifecycle: Arc::clone(self),
                finished: false,
            }),
            Decision::Refused(refusal) => Err(refusal),
            other => unreachable!("ticket events decide Proceed or Refused: {other:?}"),
        }
    }

    /// 見張りの控えの世代のままなら札を取る。
    pub(crate) fn begin_transition_at(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<TransitionGuard, Refusal> {
        self.ticket(Event::MonitorApply { generation })
    }

    /// 「再試行」の札を取る（今の画面が Fallback のときだけ）。札と、札を取った時点の
    /// Fallback の状態を返す（札がある間は画面は変わらない）。
    pub(crate) fn begin_retry(
        self: &Arc<Self>,
    ) -> Result<(TransitionGuard, FallbackState), Refusal> {
        let guard = self.ticket(Event::Retry)?;
        match guard.view() {
            Some(ShellView::Fallback(state)) => Ok((guard, state)),
            // 札がある間は画面が変わらないので来ない（来ても札を返して断る）。
            _ => Err(Refusal::NotFallback),
        }
    }

    /// 設定画面の「運用モードへ切り替える」（表の注 3）。許したら終了が確定している（呼び出し側は
    /// 後始末へ進み、新しいシェルへ続きを渡してプロセスを終える）。
    pub(crate) fn begin_switch_to_operation(&self) -> Result<(), Refusal> {
        match self.apply(Event::SwitchToOperation) {
            Decision::Proceed => Ok(()),
            Decision::Refused(refusal) => Err(refusal),
            other => unreachable!("SwitchToOperation decides Proceed or Refused: {other:?}"),
        }
    }

    /// モードの切り替えの札（表の注 4。Service・Fallback の画面のときだけ）。札と、札を取った
    /// 時点の画面を返す。
    pub(crate) fn begin_mode_switch(
        self: &Arc<Self>,
    ) -> Result<(TransitionGuard, ShellView), Refusal> {
        let guard = self.ticket(Event::ModeSwitch)?;
        match guard.view() {
            Some(view) => Ok((guard, view)),
            // 札を取れたのは Service・Fallback のときだけなので来ない。
            None => Err(Refusal::NotServiceSide),
        }
    }

    /// 対処の窓の、ファイルを変えない対処の後の起動のやり直しの札。
    pub(crate) fn begin_relaunch(self: &Arc<Self>) -> Result<TransitionGuard, Refusal> {
        self.ticket(Event::RecoveryRelaunch)
    }

    /// DB の復旧の札（対処の窓の「作り直す」「バックアップから戻す」を受け付けた時点から、
    /// 起動のやり直しの終わりまで持つ）。終了の確定・終了の後は断る（`Exiting`）。札を
    /// 返すと「復旧中」も終わる。
    pub(crate) fn begin_recovery(self: &Arc<Self>) -> Result<TransitionGuard, Refusal> {
        self.ticket(Event::RecoveryBegin)
    }

    /// `launch_desktop` が続けてよいか（扉が閉じていれば偽）。
    pub(crate) fn launch_may_proceed(&self) -> bool {
        self.apply(Event::LaunchCheck) == Decision::Proceed
    }

    /// 扉が閉じていなければ、同じロックの中で `install` を走らせる（エンジンを置く）。
    /// 走らせたら `true`。閉じていれば走らせずに `false`（呼び出し側がエンジンを止める）。
    ///
    /// **`install` はロックを握ったまま呼ぶ**（扉の確認と置くことを分けないため）。
    /// `install` の中では、この状態機械（`Lifecycle` のどのメソッドも）を呼ばない（再入で
    /// 止まる。デバッグビルドでは検知して panic する）・メインスレッドを待たない（窓・
    /// トレイ・ダイアログを操作しない）。してよいのは `AppHandle::manage` のような即時の
    /// 登録だけ（モジュール doc「ロックと待ち」）。
    pub(crate) fn admit(&self, install: impl FnOnce()) -> bool {
        let mut state = self.lock();
        let (next, decision) = step(&state, &Event::EngineAdmit);
        *state = next;
        if decision != Decision::Proceed {
            return false;
        }
        call_under_lock(install);
        drop(state);
        true
    }

    /// トレイ・Fallback の「終了」が押された。
    pub(crate) fn request_quit(&self) -> QuitRequestOutcome {
        match self.apply(Event::QuitRequest) {
            Decision::AskQuit { confirm } => QuitRequestOutcome::Ask { confirm },
            Decision::Refused(Refusal::RecoveryInProgress) => {
                QuitRequestOutcome::RecoveryInProgress
            }
            _ => QuitRequestOutcome::AlreadyHandled,
        }
    }

    /// 終了の確認の答え（確認しない態では同意として呼ぶ）。同意で確認中・復旧中で
    /// ないときだけ、**同じロックの中で**収集の受付の印を立てて忙しいかを読み
    /// （`QuitAdmission::begin_quit_confirm`）、確かめ直すときは印を下ろす。ロックの順序は
    /// 「この状態機械 → 収集の受付」の一方向。`admission` の中では `admit` の `install` と
    /// 同じく、この状態機械を呼ばない・メインスレッドを待たない。
    pub(crate) fn answer_quit(
        &self,
        confirmed: bool,
        prompted_busy: bool,
        admission: &dyn QuitAdmission,
    ) -> QuitAnswerOutcome {
        let mut state = self.lock();
        let now_busy = needs_admission(&state, confirmed)
            && call_under_lock(|| admission.begin_quit_confirm());
        let (next, decision) = step(
            &state,
            &Event::QuitAnswer {
                confirmed,
                prompted_busy,
                now_busy,
            },
        );
        *state = next;
        let Decision::QuitAnswer(outcome) = decision else {
            unreachable!("QuitAnswer decides QuitAnswer: {decision:?}");
        };
        if outcome == QuitAnswerOutcome::AskAgain {
            call_under_lock(|| admission.cancel_quit_confirm());
        }
        outcome
    }

    /// 終了の扉を閉じ（以後の移り変わり・エンジンを置くこと・起動の続きを断る）、
    /// 移り変わりの終わりを待つかと待ちの上限を返す（待たない）。何度呼んでもよい。
    pub(crate) fn begin_exit(&self, trigger: ExitTrigger) -> ExitPlan {
        match self.apply(Event::ExitBegin(trigger)) {
            Decision::Exit {
                budgets,
                transition_wait,
            } => ExitPlan {
                budgets,
                transition_wait,
            },
            other => unreachable!("ExitBegin decides Exit: {other:?}"),
        }
    }

    /// 走っている札の終わりを `budget` まで待つ。終わりを待てた（または無かった）ら
    /// `true`。待つ間はロックを手放す（条件変数）。
    pub(crate) fn wait_idle(&self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        let mut state = self.lock();
        while state.ticket != Ticket::None {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .idle
                .wait_timeout(state, deadline - now)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
        true
    }

    /// 扉を閉じて札の終わりを `budget` まで待つ（テストの近道。以前の
    /// `ShellGate::begin_exit(budget)` と同じ）。
    #[cfg(test)]
    pub(crate) fn close_and_wait(&self, budget: Duration) -> bool {
        self.begin_exit(ExitTrigger::TrayQuit);
        self.wait_idle(budget)
    }

    /// 窓を前に出してよいか（終了が確定した後は開かない）。
    pub(crate) fn may_open_window(&self) -> bool {
        self.apply(Event::OpenWindow) == Decision::Proceed
    }

    /// トレイを書き換えてよいか（`quitting_label` は「終了処理中」の書き換え）。
    pub(crate) fn tray_may_apply(&self, quitting_label: bool) -> bool {
        self.apply(Event::TrayRepaint { quitting_label }) == Decision::Proceed
    }

    /// 対処の窓のボタンを受け付ける（処理中なら偽 = 二度押しを無視する）。
    pub(crate) fn begin_recovery_action(&self) -> bool {
        self.apply(Event::RecoveryAction) == Decision::Proceed
    }

    /// 対処の窓の処理が終わった（選び直せる・× で閉じられる）。
    pub(crate) fn settle_recovery_action(&self) {
        self.apply(Event::RecoveryActionSettled);
    }

    /// 対処の窓を × で閉じてよいか（処理中は閉じさせない）。
    pub(crate) fn recovery_window_may_close(&self) -> bool {
        self.apply(Event::RecoveryWindowClose) == Decision::Proceed
    }
}

/// 札。[`Self::finish`] で新しい画面を置いて返す。そのまま落とすと画面は変えずに返す
/// （失敗・取りやめ）。
#[derive(Debug)]
pub(crate) struct TransitionGuard {
    lifecycle: Arc<Lifecycle>,
    finished: bool,
}

impl TransitionGuard {
    /// 今の画面（札がある間は変わらない）。
    pub(crate) fn view(&self) -> Option<ShellView> {
        self.lifecycle.view()
    }

    /// 新しい画面を置いて札を返す。
    pub(crate) fn finish(mut self, view: ShellView) {
        self.finished = true;
        self.lifecycle.apply(Event::TransitionFinish(view));
    }
}

impl Drop for TransitionGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.lifecycle.apply(Event::TransitionAbort);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use banto_service_control::ScmState;
    use chronogazer_core::shell_launch::ScmObservation;

    use crate::shell::{FallbackCause, FallbackState};

    // ---- 表（実装から導かずに書き下す） ----

    /// 表の代表の状態の世代（「古い世代」はこれより 1 小さい）。
    const GEN: u64 = 3;

    fn fallback_view() -> ShellView {
        ShellView::Fallback(FallbackState {
            scm: ScmObservation::State(ScmState::Stopped),
            health: None,
            cause: FallbackCause::AtStartup,
        })
    }

    fn base(view: Option<ShellView>) -> LifecycleState {
        LifecycleState {
            view,
            generation: GEN,
            streak: 1,
            ..LifecycleState::starting()
        }
    }

    /// 表の列（モジュール doc の表と同じ順・同じ意味）。
    fn columns() -> Vec<(&'static str, LifecycleState)> {
        let desktop = Some(ShellView::Desktop);
        let service = Some(ShellView::Service { port: 8721 });
        let fallback = Some(fallback_view());
        vec![
            ("St", base(None)),
            (
                "D",
                LifecycleState {
                    engine: true,
                    ..base(desktop.clone())
                },
            ),
            ("Df", base(desktop.clone())),
            ("S", base(service.clone())),
            ("F", base(fallback.clone())),
            (
                "Fc",
                LifecycleState {
                    quit: QuitPhase::Confirming,
                    ..base(fallback.clone())
                },
            ),
            (
                "T",
                LifecycleState {
                    ticket: Ticket::Transition,
                    ..base(fallback.clone())
                },
            ),
            (
                "Te",
                LifecycleState {
                    ticket: Ticket::Transition,
                    engine: true,
                    ..base(fallback.clone())
                },
            ),
            (
                "R",
                LifecycleState {
                    ticket: Ticket::Recovery,
                    recovery_action: true,
                    ..base(desktop.clone())
                },
            ),
            (
                "Rb",
                LifecycleState {
                    recovery_action: true,
                    ..base(desktop.clone())
                },
            ),
            (
                "Q",
                LifecycleState {
                    quit: QuitPhase::Quitting,
                    engine: true,
                    ..base(desktop.clone())
                },
            ),
            (
                "Qt",
                LifecycleState {
                    quit: QuitPhase::Quitting,
                    ticket: Ticket::Transition,
                    ..base(fallback.clone())
                },
            ),
            (
                "X",
                LifecycleState {
                    quit: QuitPhase::Quitting,
                    engine: true,
                    exiting: true,
                    ..base(desktop.clone())
                },
            ),
            (
                "Xt",
                LifecycleState {
                    quit: QuitPhase::Quitting,
                    ticket: Ticket::Transition,
                    exiting: true,
                    ..base(fallback.clone())
                },
            ),
            (
                "Xo",
                LifecycleState {
                    exiting: true,
                    ..base(service.clone())
                },
            ),
        ]
    }

    /// 判断の略号（モジュール doc の表の凡例）。
    fn code(decision: &Decision) -> &'static str {
        match decision {
            Decision::Applied => "ok",
            Decision::Ignored => "ign",
            Decision::Refused(Refusal::Busy) => "busy",
            Decision::Refused(Refusal::Exiting) => "exit",
            Decision::Refused(Refusal::Stale) => "stale",
            Decision::Refused(Refusal::NotFallback) => "nofb",
            Decision::Refused(Refusal::RecoveryInProgress) => "rec",
            Decision::Refused(Refusal::AlreadyConfirming) => "conf",
            Decision::Refused(Refusal::AlreadyQuitting) => "quit",
            Decision::Refused(Refusal::Quitting) => "qing",
            Decision::Refused(Refusal::RecoveryBusy) => "rbusy",
            Decision::Refused(Refusal::EngineRunning) => "eng",
            Decision::Refused(Refusal::NotDesktop) => "nodesk",
            Decision::Refused(Refusal::NotServiceSide) => "nosvc",
            Decision::Proceed => "go",
            Decision::Observe { .. } => "obs",
            Decision::SkipTick => "skip",
            Decision::StopMonitor => "stop",
            Decision::AskQuit { confirm: true } => "ask",
            Decision::AskQuit { confirm: false } => "noask",
            Decision::QuitAnswer(QuitAnswerOutcome::Proceed) => "proceed",
            Decision::QuitAnswer(QuitAnswerOutcome::AskAgain) => "again",
            Decision::QuitAnswer(QuitAnswerOutcome::RecoveryInProgress) => "rec",
            Decision::QuitAnswer(QuitAnswerOutcome::Cancelled) => "cancel",
            Decision::QuitAnswer(QuitAnswerOutcome::Ignored) => "ign",
            Decision::Exit {
                transition_wait, ..
            } => match transition_wait {
                None => "nowait",
                Some(wait) if *wait == Duration::from_secs(60) => "w60",
                Some(wait) if *wait == Duration::from_secs(5) => "w5",
                Some(_) => "w?",
            },
        }
    }

    fn answer(confirmed: bool, prompted_busy: bool, now_busy: bool) -> Event {
        Event::QuitAnswer {
            confirmed,
            prompted_busy,
            now_busy,
        }
    }

    /// 状態 × 出来事 → 判断の表（モジュール doc の表と同じ。**期待値は書き下し**）。
    #[rustfmt::skip]
    fn table() -> Vec<(&'static str, Event, [&'static str; 15])> {
        vec![
            //                                                   St       D        Df       S        F        Fc         T        Te       R        Rb       Q        Qt       X        Xt       Xo
            ("起動の判定", Event::Started(ShellView::Desktop), ["ok", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("見張りの控え", Event::MonitorPoll, ["stop", "stop", "stop", "obs", "obs", "obs", "skip", "skip", "stop", "stop", "stop", "skip", "stop", "stop", "stop"]),
            ("不健康の回数（同じ世代）", Event::MonitorStreak { generation: GEN, streak: 1 }, ["ok", "ok", "ok", "ok", "ok", "ok", "ign", "ign", "ign", "ok", "ok", "ign", "ok", "ign", "ok"]),
            ("不健康の回数（古い世代）", Event::MonitorStreak { generation: GEN - 1, streak: 1 }, ["ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("見張りの適用（同じ世代）", Event::MonitorApply { generation: GEN }, ["go", "go", "go", "go", "go", "go", "busy", "busy", "busy", "go", "go", "busy", "exit", "exit", "exit"]),
            ("見張りの適用（古い世代）", Event::MonitorApply { generation: GEN - 1 }, ["stale", "stale", "stale", "stale", "stale", "stale", "busy", "busy", "busy", "stale", "stale", "busy", "exit", "exit", "exit"]),
            ("再試行", Event::Retry, ["nofb", "nofb", "nofb", "nofb", "go", "go", "busy", "busy", "busy", "nofb", "nofb", "busy", "exit", "exit", "exit"]),
            ("対処の後の起動", Event::RecoveryRelaunch, ["go", "go", "go", "go", "go", "go", "busy", "busy", "busy", "go", "go", "busy", "exit", "exit", "exit"]),
            ("復旧の札", Event::RecoveryBegin, ["go", "eng", "go", "go", "go", "go", "busy", "busy", "busy", "go", "exit", "exit", "exit", "exit", "exit"]),
            ("札を返す（新しい画面）", Event::TransitionFinish(ShellView::Desktop), ["ign", "ign", "ign", "ign", "ign", "ign", "ok", "ok", "ok", "ign", "ign", "ok", "ign", "ok", "ign"]),
            ("札を返す（取りやめ）", Event::TransitionAbort, ["ign", "ign", "ign", "ign", "ign", "ign", "ok", "ok", "ok", "ign", "ign", "ok", "ign", "ok", "ign"]),
            ("起動を続けてよいか", Event::LaunchCheck, ["go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "exit", "exit", "exit"]),
            ("エンジンを置く", Event::EngineAdmit, ["go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "exit", "exit", "exit"]),
            ("終了", Event::QuitRequest, ["ask", "ask", "ask", "ask", "noask", "conf", "noask", "noask", "rec", "ask", "quit", "quit", "quit", "quit", "ask"]),
            ("答え: 取り消し", answer(false, false, false), ["ign", "ign", "ign", "ign", "ign", "cancel", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("答え: 同意（暇 → 暇）", answer(true, false, false), ["ign", "ign", "ign", "ign", "ign", "proceed", "ign", "ign", "rec", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("答え: 同意（暇 → 収集中）", answer(true, false, true), ["ign", "ign", "ign", "ign", "ign", "again", "ign", "ign", "rec", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("答え: 同意（収集中 → 収集中）", answer(true, true, true), ["ign", "ign", "ign", "ign", "ign", "proceed", "ign", "ign", "rec", "ign", "ign", "ign", "ign", "ign", "ign"]),
            ("後始末（トレイ）", Event::ExitBegin(ExitTrigger::TrayQuit), ["nowait", "nowait", "nowait", "nowait", "nowait", "nowait", "w60", "w60", "w60", "nowait", "nowait", "w60", "nowait", "w60", "nowait"]),
            ("後始末（OS の終了）", Event::ExitBegin(ExitTrigger::RunEventExit), ["nowait", "nowait", "nowait", "nowait", "nowait", "nowait", "w5", "nowait", "w5", "nowait", "nowait", "w5", "nowait", "w5", "nowait"]),
            ("窓を開く", Event::OpenWindow, ["go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "qing", "qing", "qing", "qing", "go"]),
            ("トレイの書き換え", Event::TrayRepaint { quitting_label: false }, ["go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "qing", "qing", "qing", "qing", "go"]),
            ("トレイの「終了処理中」", Event::TrayRepaint { quitting_label: true }, ["go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go", "go"]),
            ("対処の窓のボタン", Event::RecoveryAction, ["go", "go", "go", "go", "go", "go", "go", "go", "rbusy", "rbusy", "go", "go", "go", "go", "go"]),
            ("対処の窓の処理の終わり", Event::RecoveryActionSettled, ["ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok", "ok"]),
            ("対処の窓の ×", Event::RecoveryWindowClose, ["go", "go", "go", "go", "go", "go", "go", "go", "rbusy", "rbusy", "go", "go", "go", "go", "go"]),
            ("運用モードへ（設定画面）", Event::SwitchToOperation, ["nodesk", "go", "go", "nodesk", "nodesk", "conf", "busy", "busy", "rec", "rbusy", "quit", "quit", "exit", "exit", "exit"]),
            ("モードの切り替えの札", Event::ModeSwitch, ["nosvc", "nosvc", "nosvc", "go", "go", "go", "busy", "busy", "busy", "nosvc", "quit", "quit", "exit", "exit", "exit"]),
        ]
    }

    /// 表の総当たり: すべての（状態, 出来事）で判断が書き下した表のとおり。食い違いは
    /// 全部まとめて出す（1 つ直して次、にしない）。
    #[test]
    fn transition_table() {
        let columns = columns();
        let mut mismatches = Vec::new();
        for (label, event, expected) in table() {
            for ((name, state), want) in columns.iter().zip(expected) {
                let (_, decision) = step(state, &event);
                let got = code(&decision);
                if got != want {
                    mismatches.push(format!("{label} × {name}: 期待 {want}、実際 {got}"));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    /// 表の行が出来事の種類をすべて覆う（出来事を足して表に行を足し忘れると落ちる）。
    #[test]
    fn table_covers_every_event_kind() {
        let kinds: std::collections::BTreeSet<_> = table()
            .iter()
            .map(|(_, event, _)| std::mem::discriminant(event))
            .map(|discriminant| format!("{discriminant:?}"))
            .collect();
        let all: std::collections::BTreeSet<_> = all_events()
            .iter()
            .map(|event| format!("{:?}", std::mem::discriminant(event)))
            .collect();
        assert_eq!(kinds, all);
    }

    /// 列の状態の名前（[`Phase`]）がモジュール doc の「状態」の意味どおり。
    #[test]
    fn column_phases() {
        let phases: Vec<_> = columns()
            .into_iter()
            .map(|(name, state)| (name, state.phase()))
            .collect();
        assert_eq!(
            phases,
            vec![
                ("St", Phase::Starting),
                ("D", Phase::Showing(ViewKind::Desktop)),
                ("Df", Phase::Showing(ViewKind::Desktop)),
                ("S", Phase::Showing(ViewKind::Service)),
                ("F", Phase::Showing(ViewKind::Fallback)),
                ("Fc", Phase::Showing(ViewKind::Fallback)),
                ("T", Phase::Transitioning),
                ("Te", Phase::Transitioning),
                ("R", Phase::Recovering),
                ("Rb", Phase::Showing(ViewKind::Desktop)),
                ("Q", Phase::QuitReserved),
                ("Qt", Phase::QuitReserved),
                ("X", Phase::Exiting),
                ("Xt", Phase::Exiting),
                ("Xo", Phase::Exiting),
            ]
        );
    }

    // ---- 約束（印のすべての組で） ----

    fn all_views() -> Vec<Option<ShellView>> {
        vec![
            None,
            Some(ShellView::Desktop),
            Some(ShellView::Service { port: 8721 }),
            Some(fallback_view()),
        ]
    }

    /// 復旧の札とエンジンが同時に立つ組（モジュール doc の表の注 2。エンジンを置く出来事
    /// でしか作られない）。
    fn recovery_with_engine(state: &LifecycleState) -> bool {
        state.engine && state.ticket == Ticket::Recovery
    }

    /// 印のすべての組（画面 4 × 札 3 × 終了の段階 3 × 扉 2 × エンジン 2 × 対処の窓 2）。
    fn all_states() -> Vec<LifecycleState> {
        let mut states = Vec::new();
        for view in all_views() {
            for ticket in [Ticket::None, Ticket::Transition, Ticket::Recovery] {
                for quit in [QuitPhase::Idle, QuitPhase::Confirming, QuitPhase::Quitting] {
                    for exiting in [false, true] {
                        for engine in [false, true] {
                            for recovery_action in [false, true] {
                                states.push(LifecycleState {
                                    view: view.clone(),
                                    generation: GEN,
                                    streak: 1,
                                    ticket,
                                    quit,
                                    exiting,
                                    engine,
                                    recovery_action,
                                });
                            }
                        }
                    }
                }
            }
        }
        states
    }

    /// すべての出来事（引数の組も）。出来事を足したらここにも足す。
    fn all_events() -> Vec<Event> {
        let mut events = vec![
            Event::MonitorPoll,
            Event::MonitorStreak {
                generation: GEN,
                streak: 2,
            },
            Event::MonitorStreak {
                generation: GEN - 1,
                streak: 2,
            },
            Event::MonitorApply { generation: GEN },
            Event::MonitorApply {
                generation: GEN - 1,
            },
            Event::Retry,
            Event::RecoveryRelaunch,
            Event::RecoveryBegin,
            Event::TransitionAbort,
            Event::LaunchCheck,
            Event::EngineAdmit,
            Event::QuitRequest,
            Event::ExitBegin(ExitTrigger::TrayQuit),
            Event::ExitBegin(ExitTrigger::RunEventExit),
            Event::OpenWindow,
            Event::TrayRepaint {
                quitting_label: false,
            },
            Event::TrayRepaint {
                quitting_label: true,
            },
            Event::RecoveryAction,
            Event::RecoveryActionSettled,
            Event::RecoveryWindowClose,
            Event::SwitchToOperation,
            Event::ModeSwitch,
        ];
        for view in all_views().into_iter().flatten() {
            events.push(Event::Started(view.clone()));
            events.push(Event::TransitionFinish(view));
        }
        for confirmed in [false, true] {
            for prompted_busy in [false, true] {
                for now_busy in [false, true] {
                    events.push(answer(confirmed, prompted_busy, now_busy));
                }
            }
        }
        events
    }

    fn grants_a_ticket(event: &Event) -> bool {
        matches!(
            event,
            Event::MonitorApply { .. }
                | Event::Retry
                | Event::RecoveryRelaunch
                | Event::RecoveryBegin
                | Event::ModeSwitch
        )
    }

    /// 表に出てこない約束（モジュール doc）を、印のすべての組 × すべての出来事で確かめる。
    #[test]
    fn invariants_hold_from_every_state() {
        for state in all_states() {
            for event in all_events() {
                let (next, decision) = step(&state, &event);
                let at = format!("{state:?} × {event:?} -> {next:?} / {decision:?}");

                // 扉は二度と開かない。扉が閉じた後は札・エンジン・起動の続きを許さない。
                assert!(!state.exiting || next.exiting, "扉が開いた: {at}");
                if state.exiting {
                    if grants_a_ticket(&event)
                        || matches!(event, Event::EngineAdmit | Event::LaunchCheck)
                    {
                        assert_eq!(decision, Decision::Refused(Refusal::Exiting), "{at}");
                    }
                    assert_eq!(next.engine, state.engine, "扉の後に置いた: {at}");
                    if state.ticket == Ticket::None {
                        assert_eq!(next.ticket, Ticket::None, "扉の後の札: {at}");
                    }
                }
                // 終了の確定は戻らない。
                if state.quit == QuitPhase::Quitting {
                    assert_eq!(next.quit, QuitPhase::Quitting, "確定が戻った: {at}");
                }
                // 見張りは Desktop・画面が決まる前を観測しない。
                if let Decision::Observe { view, .. } = &decision {
                    assert_ne!(*view, ShellView::Desktop, "{at}");
                    assert_eq!(state.view.as_ref(), Some(view), "{at}");
                }
                // 復旧の札と終了の確定は同時に立たない（立っていない状態から立たない）。
                let both = |s: &LifecycleState| {
                    s.ticket == Ticket::Recovery && s.quit == QuitPhase::Quitting
                };
                if !both(&state) {
                    assert!(!both(&next), "復旧と終了の確定が並んだ: {at}");
                }
                // 復旧の札とエンジンが並ぶのは、札を持ったままエンジンを置いたときだけ
                // （復旧の札はエンジンが無いときだけ取る。表の注 2）。
                if !recovery_with_engine(&state) && recovery_with_engine(&next) {
                    assert_eq!(event, Event::EngineAdmit, "{at}");
                    assert_eq!(state.ticket, Ticket::Recovery, "{at}");
                }
                // 札は常に 1 つ: 札がある間に別の札を出さない（返すのは札を返す出来事だけ）。
                if state.ticket != Ticket::None {
                    match event {
                        Event::TransitionFinish(_) | Event::TransitionAbort => {
                            assert_eq!(next.ticket, Ticket::None, "{at}")
                        }
                        _ => assert_eq!(next.ticket, state.ticket, "{at}"),
                    }
                    if grants_a_ticket(&event) {
                        assert_ne!(decision, Decision::Proceed, "{at}");
                    }
                }
                // 札の種類は出来事で決まる。
                if state.ticket == Ticket::None && next.ticket != Ticket::None {
                    let expected = match event {
                        Event::RecoveryBegin => Ticket::Recovery,
                        Event::MonitorApply { .. }
                        | Event::Retry
                        | Event::RecoveryRelaunch
                        | Event::ModeSwitch => Ticket::Transition,
                        _ => panic!("札を出す出来事ではない: {at}"),
                    };
                    assert_eq!(next.ticket, expected, "{at}");
                }
                // 見張りの古い世代の適用は断る。
                if let Event::MonitorApply { generation } = event {
                    if generation != state.generation {
                        assert_ne!(decision, Decision::Proceed, "{at}");
                    }
                }
                // 世代は札を返して新しい画面を置いたときだけ 1 増える。
                if next.generation != state.generation {
                    assert!(matches!(event, Event::TransitionFinish(_)), "{at}");
                    assert_ne!(state.ticket, Ticket::None, "{at}");
                    assert_eq!(next.generation, state.generation + 1, "{at}");
                }
                // 画面が変わるのは、起動の判定と札を返して新しい画面を置いたときだけ
                // （見張り・「再試行」・終了は画面を直接変えない）。
                if next.view != state.view {
                    assert!(
                        matches!(event, Event::Started(_) | Event::TransitionFinish(_)),
                        "{at}"
                    );
                }
                // 復旧中の「終了」・同意は断る。同意で終了へ進むのは確認中だけ。
                if state.ticket == Ticket::Recovery {
                    if event == Event::QuitRequest && state.quit == QuitPhase::Idle {
                        assert_eq!(
                            decision,
                            Decision::Refused(Refusal::RecoveryInProgress),
                            "{at}"
                        );
                    }
                    if let Event::QuitAnswer {
                        confirmed: true, ..
                    } = event
                    {
                        assert_eq!(
                            decision,
                            Decision::QuitAnswer(QuitAnswerOutcome::RecoveryInProgress),
                            "{at}"
                        );
                    }
                }
                // 運用モードへ（注 3）は Desktop の画面で、札・確認・復旧が無いときだけ許し、
                // 許したら終了が確定する。モードの切り替えの札（注 4）は Desktop・画面の前では出ない。
                if event == Event::SwitchToOperation && decision == Decision::Proceed {
                    assert_eq!(state.view, Some(ShellView::Desktop), "{at}");
                    assert_eq!(state.ticket, Ticket::None, "{at}");
                    assert_eq!(state.quit, QuitPhase::Idle, "{at}");
                    assert_eq!(next.quit, QuitPhase::Quitting, "{at}");
                }
                if event == Event::ModeSwitch && decision == Decision::Proceed {
                    assert!(
                        matches!(
                            state.view,
                            Some(ShellView::Service { .. } | ShellView::Fallback(_))
                        ),
                        "{at}"
                    );
                    assert_ne!(state.quit, QuitPhase::Quitting, "{at}");
                }
                if decision == Decision::QuitAnswer(QuitAnswerOutcome::Proceed) {
                    assert_eq!(state.quit, QuitPhase::Confirming, "{at}");
                    assert_ne!(state.ticket, Ticket::Recovery, "{at}");
                }
                // 終了の後始末: OS の終了は待ち + 後始末が 5 秒に収まり、エンジンが既に
                // 置かれていれば待たない。札が無ければ待たない。
                if let (
                    Event::ExitBegin(trigger),
                    Decision::Exit {
                        budgets,
                        transition_wait,
                    },
                ) = (&event, &decision)
                {
                    assert_eq!(*budgets, exit_budgets(*trigger), "{at}");
                    if state.ticket == Ticket::None {
                        assert_eq!(*transition_wait, None, "{at}");
                    }
                    if *trigger == ExitTrigger::RunEventExit {
                        let waited = transition_wait.unwrap_or(Duration::ZERO);
                        assert!(
                            waited + budgets.cleanup_after(waited) <= EXIT_CLEANUP_BUDGET,
                            "{at}"
                        );
                        if state.engine {
                            assert_eq!(*transition_wait, None, "{at}");
                        }
                    }
                }
                // 窓を開く・ふつうの書き換えは、終了の確定の後は断る。
                if matches!(
                    event,
                    Event::OpenWindow
                        | Event::TrayRepaint {
                            quitting_label: false
                        }
                ) {
                    assert_eq!(
                        decision == Decision::Proceed,
                        state.quit != QuitPhase::Quitting,
                        "{at}"
                    );
                }
            }
        }
    }

    /// どの状態からでも、終了に入った後は札・復旧・エンジンを置くこと・起動の続きを
    /// 許さない（出来事を 2 つ続けて）。
    #[test]
    fn nothing_is_admitted_after_exit_from_any_state() {
        for state in all_states() {
            for trigger in [ExitTrigger::TrayQuit, ExitTrigger::RunEventExit] {
                let (exiting, _) = step(&state, &Event::ExitBegin(trigger));
                assert!(exiting.exiting);
                for event in all_events() {
                    let (next, decision) = step(&exiting, &event);
                    if grants_a_ticket(&event)
                        || matches!(event, Event::EngineAdmit | Event::LaunchCheck)
                    {
                        assert_eq!(
                            decision,
                            Decision::Refused(Refusal::Exiting),
                            "{state:?} → {trigger:?} → {event:?}"
                        );
                    }
                    assert!(next.exiting);
                    assert_eq!(next.engine, state.engine);
                }
            }
        }
    }

    /// 表の注 2: エンジンが置かれた状態からは、どの状態でも復旧の札を出さない。
    #[test]
    fn recovery_ticket_is_only_taken_without_an_engine() {
        assert_eq!(all_states().len(), 288);
        for state in all_states().into_iter().filter(|state| state.engine) {
            let (next, decision) = step(&state, &Event::RecoveryBegin);
            assert_ne!(decision, Decision::Proceed, "{state:?}");
            assert_eq!(next.ticket, state.ticket, "{state:?}");
            if !state.exiting && state.quit != QuitPhase::Quitting && state.ticket == Ticket::None {
                assert_eq!(
                    decision,
                    Decision::Refused(Refusal::EngineRunning),
                    "{state:?}"
                );
            }
        }
    }

    /// PR #611 のレビュー 3: 対処の窓が札を取れなかった理由は、`refused_notice` が想定する
    /// ものだけ（`Exiting`・`Busy`）。対処の窓が「作り直す」「戻す」を出すのはエンジンが
    /// 置かれていないときだけなので、復旧の札はエンジンの無い状態で確かめる。デバッグ
    /// ビルドでは、想定外の理由を渡すと `refused_notice` の `debug_assert!` で落ちる。
    #[test]
    fn recovery_window_refusals_are_only_the_ones_refused_notice_expects() {
        for state in all_states() {
            let mut events = vec![Event::RecoveryRelaunch];
            if !state.engine {
                events.push(Event::RecoveryBegin);
            }
            for event in events {
                if let (_, Decision::Refused(refusal)) = step(&state, &event) {
                    assert!(
                        matches!(refusal, Refusal::Exiting | Refusal::Busy),
                        "{state:?} × {event:?} -> {refusal:?}"
                    );
                    assert!(!crate::startup::refused_notice(refusal).is_empty());
                }
            }
        }
    }

    /// 確認の答えで収集の受付を触るのは、同意で確認中・復旧中でないときだけ
    /// （共有の `QuitGate::answer` と同じ）。
    #[test]
    fn admission_is_needed_only_for_a_confirmed_answer_while_confirming() {
        for state in all_states() {
            for confirmed in [false, true] {
                assert_eq!(
                    needs_admission(&state, confirmed),
                    confirmed
                        && state.quit == QuitPhase::Confirming
                        && state.ticket != Ticket::Recovery,
                    "{state:?} confirmed={confirmed}"
                );
            }
        }
    }

    /// PR #608 の全体監査 F2・F3: 待ちの上限の表。トレイの終了は移り変わりを長く待ち
    /// 後始末は 5 秒、OS の終了は移り変わりの待ちと後始末を合わせて 5 秒に収める。
    #[test]
    fn exit_budget_table() {
        let tray = exit_budgets(ExitTrigger::TrayQuit);
        assert_eq!(tray.transition_wait, Duration::from_secs(60));
        assert!(!tray.shared_deadline);
        for waited in [0, 1, 5, 59, 60] {
            assert_eq!(
                tray.cleanup_after(Duration::from_secs(waited)),
                EXIT_CLEANUP_BUDGET
            );
        }
        let os = exit_budgets(ExitTrigger::RunEventExit);
        assert_eq!(os.transition_wait, EXIT_CLEANUP_BUDGET);
        assert!(os.shared_deadline);
        for waited_ms in [0u64, 1, 1_000, 4_999, 5_000, 6_000] {
            let waited = Duration::from_millis(waited_ms).min(os.transition_wait);
            let total = waited + os.cleanup_after(waited);
            assert!(total <= EXIT_CLEANUP_BUDGET, "{waited_ms} ms -> {total:?}");
        }
        assert_eq!(
            os.cleanup_after(Duration::from_secs(1)),
            Duration::from_secs(4)
        );
        assert_eq!(os.cleanup_after(Duration::from_secs(9)), Duration::ZERO);
    }

    // ---- 入れ物（以前の `ShellGate`・`QuitGate`・`RecoveryGate` のテストを移したもの） ----

    #[test]
    fn transitions_are_one_at_a_time_and_stale_results_are_dropped() {
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        let PollSnapshot::Ready { generation, .. } = gate.poll() else {
            panic!("ready");
        };
        let guard = gate.begin_relaunch().expect("first");
        assert_eq!(gate.begin_relaunch().unwrap_err(), Refusal::Busy);
        assert_eq!(gate.poll(), PollSnapshot::Busy);
        // 札の最中の不健康の回数は残さない。
        gate.record_streak(generation, 1);
        guard.finish(fallback_view());
        // 控えの後に画面が変わった: 見張りの古い結果は捨てる。
        assert_eq!(
            gate.begin_transition_at(generation).unwrap_err(),
            Refusal::Stale
        );
        gate.record_streak(generation, 1);
        let PollSnapshot::Ready {
            generation: next,
            streak,
            ..
        } = gate.poll()
        else {
            panic!("ready");
        };
        assert_ne!(next, generation);
        assert_eq!(streak, 0);
        // 同じ世代なら取れる。落とすと画面は変わらずに札が返る。
        let guard = gate.begin_transition_at(next).expect("same generation");
        drop(guard);
        assert!(matches!(gate.view(), Some(ShellView::Fallback(_))));
        assert!(gate.begin_relaunch().is_ok());
    }

    #[test]
    fn desktop_view_and_exit_stop_the_monitor() {
        let gate = Lifecycle::started(ShellView::Desktop);
        assert_eq!(gate.poll(), PollSnapshot::Stop);
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        assert!(gate.close_and_wait(Duration::from_millis(10)));
        assert_eq!(gate.poll(), PollSnapshot::Stop);
        // 画面が決まる前（Starting）も見張らない。トレイの態は Desktop。
        let starting = Lifecycle::new();
        assert_eq!(starting.poll(), PollSnapshot::Stop);
        assert_eq!(starting.view(), None);
        assert_eq!(
            crate::shell::tray_mode_of(starting.view().as_ref()),
            crate::tray::TrayMode::Desktop
        );
    }

    /// 「再試行」は Fallback のときだけ札を出し、札と Fallback の状態を返す。
    #[test]
    fn retry_needs_the_fallback_view() {
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        assert_eq!(gate.begin_retry().unwrap_err(), Refusal::NotFallback);
        // 断っても札は残らない。
        assert!(gate.begin_relaunch().is_ok());
        let gate = Lifecycle::started(fallback_view());
        let (guard, state) = gate.begin_retry().expect("fallback");
        assert_eq!(ShellView::Fallback(state), fallback_view());
        assert_eq!(gate.begin_retry().unwrap_err(), Refusal::Busy);
        drop(guard);
        gate.begin_exit(ExitTrigger::TrayQuit);
        assert_eq!(gate.begin_retry().unwrap_err(), Refusal::Exiting);
    }

    /// 終了の扉: 閉じた後は移り変わりもエンジンを置くことも断る（置かなかった
    /// エンジンは呼び出し側が止める）。閉じる前なら置ける。
    #[test]
    fn exit_closes_the_door_for_transitions_and_engines() {
        let gate = Lifecycle::started(fallback_view());
        let mut installed = false;
        assert!(gate.admit(|| installed = true));
        assert!(installed);
        assert!(gate.launch_may_proceed());
        assert!(gate.close_and_wait(Duration::from_millis(10)));
        assert!(gate.is_exiting());
        let mut installed_after = false;
        assert!(!gate.admit(|| installed_after = true));
        assert!(!installed_after);
        assert!(!gate.launch_may_proceed());
        assert_eq!(gate.begin_relaunch().unwrap_err(), Refusal::Exiting);
        // 何度呼んでもよい。
        assert!(gate.close_and_wait(Duration::from_millis(10)));
    }

    /// 終了は走っている移り変わり（「再試行」の Desktop の起動）の終わりを待つ。
    #[test]
    fn exit_waits_for_the_running_transition() {
        let gate = Lifecycle::started(fallback_view());
        let guard = gate.begin_relaunch().expect("transition");
        let (tx, rx) = mpsc::channel();
        let waiter = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let waited = gate.close_and_wait(Duration::from_secs(10));
                tx.send(waited).unwrap();
            })
        };
        // 札がある間は戻らない。
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        // 札の中で扉が閉じたのが見える（起動し終えたエンジンは置かずに止める）。
        assert!(gate.is_exiting());
        assert!(!gate.admit(|| panic!("扉が閉じた後に置いてはいけない")));
        let finished_at = Instant::now();
        guard.finish(ShellView::Desktop);
        assert!(rx.recv_timeout(Duration::from_secs(10)).expect("returns"));
        // 長い上限（10 秒）でも、札が返れば待たずに戻る（PR #608 の全体監査 F2）。
        assert!(finished_at.elapsed() < Duration::from_secs(5));
        waiter.join().unwrap();
    }

    /// 扉を閉じるだけなら待たない。閉じた後の待ちは札の終わりまで。
    #[test]
    fn close_does_not_wait_and_wait_idle_is_bounded() {
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        let guard = gate.begin_relaunch().expect("transition");
        let started = Instant::now();
        let plan = gate.begin_exit(ExitTrigger::TrayQuit);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(plan.transition_wait, Some(TRANSITION_WAIT_ON_QUIT));
        assert!(gate.is_exiting());
        assert!(!gate.wait_idle(Duration::from_millis(50)));
        drop(guard);
        assert!(gate.wait_idle(Duration::from_millis(50)));
    }

    /// 移り変わりが終わらなければ、予算で待つのをやめる。
    #[test]
    fn exit_wait_is_bounded() {
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        let _guard = gate.begin_relaunch().expect("transition");
        let started = Instant::now();
        assert!(!gate.close_and_wait(Duration::from_millis(100)));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// 終了の計画: OS の終了は、エンジンが置かれていれば移り変わりを待たない（置く前なら
    /// 5 秒）。トレイの終了は 60 秒。札が無ければ待たない。
    #[test]
    fn exit_plan_skips_the_wait_once_the_engine_is_in_place() {
        let gate = Lifecycle::started(fallback_view());
        let _guard = gate.begin_retry().expect("retry");
        assert!(gate.admit(|| {}));
        let plan = gate.begin_exit(ExitTrigger::RunEventExit);
        assert_eq!(plan.transition_wait, None);
        assert_eq!(plan.budgets, exit_budgets(ExitTrigger::RunEventExit));

        let gate = Lifecycle::started(fallback_view());
        let _guard = gate.begin_retry().expect("retry");
        let plan = gate.begin_exit(ExitTrigger::RunEventExit);
        assert_eq!(plan.transition_wait, Some(EXIT_CLEANUP_BUDGET));

        let gate = Lifecycle::started(fallback_view());
        let _guard = gate.begin_retry().expect("retry");
        assert!(gate.admit(|| {}));
        let plan = gate.begin_exit(ExitTrigger::TrayQuit);
        assert_eq!(plan.transition_wait, Some(TRANSITION_WAIT_ON_QUIT));

        let gate = Lifecycle::started(ShellView::Desktop);
        assert_eq!(gate.begin_exit(ExitTrigger::TrayQuit).transition_wait, None);
    }

    struct NotBusy;
    impl QuitAdmission for NotBusy {
        fn begin_quit_confirm(&self) -> bool {
            false
        }
        fn cancel_quit_confirm(&self) {}
    }

    /// 収集の受付の印の上げ下げを数える。
    #[derive(Default)]
    struct Counting {
        busy: bool,
        begun: AtomicUsize,
        cancelled: AtomicUsize,
    }
    impl QuitAdmission for Counting {
        fn begin_quit_confirm(&self) -> bool {
            self.begun.fetch_add(1, Ordering::SeqCst);
            self.busy
        }
        fn cancel_quit_confirm(&self) {
            self.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// PR #608 のオーナーレビュー P2: DB の復旧の途中のトレイの「終了」は断られ（押した
    /// 時点でも、確定の時点でも）、扉も閉じない。復旧が終われば通る。
    #[test]
    fn quit_is_refused_while_recovering_and_allowed_after() {
        let gate = Lifecycle::started(ShellView::Desktop);
        let recovery = gate.begin_recovery().expect("recovery");
        assert!(gate.is_recovering());
        assert_eq!(gate.request_quit(), QuitRequestOutcome::RecoveryInProgress);
        assert_eq!(gate.snapshot().quit, QuitPhase::Idle);
        // 断っても扉は閉じない（終了処理へ進んでいない）。
        assert!(!gate.is_exiting());
        // 復旧の失敗（札を落とす）でも成功（finish）でも、終われば通る。
        drop(recovery);
        assert!(!gate.is_recovering());
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: true }
        );
        assert_eq!(
            gate.answer_quit(false, false, &NotBusy),
            QuitAnswerOutcome::Cancelled
        );
        let recovery = gate.begin_recovery().expect("recovery again");
        assert_eq!(gate.request_quit(), QuitRequestOutcome::RecoveryInProgress);
        recovery.finish(ShellView::Desktop);
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: true }
        );
        assert_eq!(
            gate.answer_quit(true, false, &NotBusy),
            QuitAnswerOutcome::Proceed
        );
    }

    /// 確認を出した後に復旧が始まったら、確定の時点で断り（確認の段階は戻す）、
    /// 終了処理へ進まない。復旧の後は通り、通った後は新しい復旧を始めない。
    #[test]
    fn quit_confirmed_during_recovery_is_refused_then_allowed_after() {
        let gate = Lifecycle::started(ShellView::Desktop);
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: true }
        );
        let recovery = gate.begin_recovery().expect("recovery");
        let admission = Counting::default();
        assert_eq!(
            gate.answer_quit(true, false, &admission),
            QuitAnswerOutcome::RecoveryInProgress
        );
        // 断るときは収集の受付に触らない。
        assert_eq!(admission.begun.load(Ordering::SeqCst), 0);
        assert_eq!(gate.snapshot().quit, QuitPhase::Idle);
        assert!(!gate.is_exiting());

        drop(recovery);
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: true }
        );
        assert_eq!(
            gate.answer_quit(true, false, &NotBusy),
            QuitAnswerOutcome::Proceed
        );
        assert!(gate.quitting());
        // 確定は持ったまま: この後の新しい復旧は始まらない。
        assert_eq!(gate.begin_recovery().unwrap_err(), Refusal::Exiting);
        // 2 つ目の「終了」は何もしない。
        assert_eq!(gate.request_quit(), QuitRequestOutcome::AlreadyHandled);
    }

    /// 取り消し・確かめ直しでは確定しない（復旧を塞ぎ続けない）。普通の移り変わりの
    /// 札は「復旧中」にせず、その間の終了は確定できる。
    #[test]
    fn quit_is_not_reserved_unless_proceeding() {
        let gate = Lifecycle::started(ShellView::Desktop);
        gate.request_quit();
        assert_eq!(
            gate.answer_quit(false, false, &NotBusy),
            QuitAnswerOutcome::Cancelled
        );
        drop(gate.begin_recovery().expect("not reserved after cancel"));

        gate.request_quit();
        let busy = Counting {
            busy: true,
            ..Counting::default()
        };
        assert_eq!(
            gate.answer_quit(true, false, &busy),
            QuitAnswerOutcome::AskAgain
        );
        // 確かめ直すときは印を下ろす（`QuitGate::answer` と同じ）。
        assert_eq!(busy.begun.load(Ordering::SeqCst), 1);
        assert_eq!(busy.cancelled.load(Ordering::SeqCst), 1);
        assert_eq!(gate.snapshot().quit, QuitPhase::Confirming);
        drop(gate.begin_recovery().expect("not reserved after ask-again"));
        // 収集中の文言で同意すれば進む（印は立てたまま）。
        assert_eq!(
            gate.answer_quit(true, true, &busy),
            QuitAnswerOutcome::Proceed
        );
        assert_eq!(busy.begun.load(Ordering::SeqCst), 2);
        assert_eq!(busy.cancelled.load(Ordering::SeqCst), 1);

        let gate = Lifecycle::started(ShellView::Desktop);
        let plain = gate.begin_relaunch().expect("plain");
        assert!(!gate.is_recovering());
        gate.request_quit();
        assert_eq!(
            gate.answer_quit(true, false, &NotBusy),
            QuitAnswerOutcome::Proceed
        );
        drop(plain);
        // 遅れて届いた答えは何もしない（確定は戻らない・受付に触らない）。
        let late = Counting::default();
        assert_eq!(
            gate.answer_quit(true, false, &late),
            QuitAnswerOutcome::Ignored
        );
        assert_eq!(late.begun.load(Ordering::SeqCst), 0);
        assert!(gate.quitting());
    }

    /// Fallback の「終了」は確認しない（同意として答える）。確認ダイアログは 2 つ出さない。
    #[test]
    fn quit_request_asks_per_view() {
        let gate = Lifecycle::started(fallback_view());
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: false }
        );
        assert_eq!(gate.request_quit(), QuitRequestOutcome::AlreadyHandled);
        let gate = Lifecycle::started(ShellView::Service { port: 1 });
        assert_eq!(
            gate.request_quit(),
            QuitRequestOutcome::Ask { confirm: true }
        );
    }

    /// 終了が確定した後は「終了処理中」以外の書き換えをせず、窓も開かない。
    #[test]
    fn only_the_quitting_label_is_applied_after_quit() {
        let gate = Lifecycle::started(ShellView::Desktop);
        assert!(gate.tray_may_apply(false));
        assert!(gate.tray_may_apply(true));
        assert!(gate.may_open_window());
        gate.request_quit();
        gate.answer_quit(true, false, &NotBusy);
        assert!(!gate.tray_may_apply(false));
        assert!(gate.tray_may_apply(true));
        assert!(!gate.may_open_window());
    }

    /// PR #611 のオーナーレビュー 2: `install` の中から状態機械を呼ぶ（再入）と、
    /// デバッグビルドでは止まる前に panic で知らせる。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lifecycle の再入")]
    fn reentering_from_install_is_detected_in_debug_builds() {
        let gate = Lifecycle::started(ShellView::Desktop);
        gate.admit(|| {
            let _ = gate.view();
        });
    }

    /// 収集の受付の中からの再入も同じく検知する。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lifecycle の再入")]
    fn reentering_from_the_quit_admission_is_detected_in_debug_builds() {
        struct Reentrant<'a>(&'a Lifecycle);
        impl QuitAdmission for Reentrant<'_> {
            fn begin_quit_confirm(&self) -> bool {
                self.0.may_open_window()
            }
            fn cancel_quit_confirm(&self) {}
        }
        let gate = Lifecycle::started(ShellView::Desktop);
        gate.request_quit();
        gate.answer_quit(true, false, &Reentrant(&gate));
    }

    /// 外の処理が終われば（panic で抜けても）印は下り、同じスレッドから普通に呼べる。
    #[test]
    fn the_reentry_mark_is_cleared_after_the_callback() {
        let gate = Lifecycle::started(ShellView::Desktop);
        assert!(gate.admit(|| {}));
        assert_eq!(gate.view(), Some(ShellView::Desktop));
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gate.admit(|| panic!("install failed"));
        }));
        assert!(unwound.is_err());
        // 毒化は無視して中身を使う（`lock`）。印も残っていない。
        assert!(gate.launch_may_proceed());
    }

    /// 対処の窓: 処理中は二度押しを無視し × で閉じさせない。終われば戻る。
    #[test]
    fn recovery_window_is_busy_until_settled() {
        let gate = Lifecycle::started(ShellView::Desktop);
        assert!(gate.recovery_window_may_close());
        assert!(gate.begin_recovery_action());
        assert!(!gate.begin_recovery_action());
        assert!(!gate.recovery_window_may_close());
        gate.settle_recovery_action();
        assert!(gate.recovery_window_may_close());
        assert!(gate.begin_recovery_action());
    }
}
