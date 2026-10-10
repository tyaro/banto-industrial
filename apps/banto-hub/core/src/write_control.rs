//! 書き込み受付状態 (docs/tag-server-design.md §6-6)。[`WriteControl`] は
//! 「この hub プロセスはいま `/api/v1/values/{tag}` への書き込みを受け付けて
//! よいか」を持つ、読み取りがロックフリーの薄いフラグ保持者。
//!
//! ## ルール: 既定は有効、再起動では永続値を復元する
//!
//! (2026-09-09 オーナー決定, #340 - 旧ルール「起動時は必ず disabled」を撤回)
//!
//! 永続値の seed は `write_control_state.enabled_persisted = 1`
//! (`migrations-sqlite/0102_write_control_state.sql` 参照、既定で書き込み可)。banto-hub は
//! relay-wright のようなルールエンジンを持たない、外部クライアントの明示
//! 要求を1回転送するだけのパススルーであり、再起動後に条件が揃えば自律的に
//! 書き込みを再開するという危険が存在しない。書き込みの可否は per-tag
//! `writable` (既定 false) と API キーの `write` スコープが実質担っており、
//! このグローバルトグルは運用者が手で書き込みを止める**非常停止スイッチ**に
//! 徹する。手動で止めた状態はプロセス再起動を跨いで保持される。
//!
//! ## 停止の状態は 2 か所に記録する (#433、2026-09-24)
//!
//! #340 のままでは、停止を DB に保存できなかったとき (DB が完全に落ちて
//! いる等) に再起動すると、最後に保存された値 (有効) に**黙って戻った**。
//! これを塞ぐため、状態を DB (`write_control_state`) と、データ
//! ディレクトリ内の小さな状態ファイル ([`STATE_FILE_NAME`]、
//! [`state_file_path`]) の 2 か所に書く:
//!
//! - **起動**: 「どちらか一方でも停止なら停止」で起動する
//!   ([`decide_startup`]、純関数。総当たりの表はテスト参照)。DB を読めない・
//!   ファイルが壊れている・読めない・ファイルが「再開の途中」の場合も停止側に
//!   倒す。ファイルが無い (#433 より前からの環境・初回起動) だけは DB の値に
//!   従う。食い違いは [`StartupDecision::warning`] としてログと管理画面
//!   (状態画面の「書き込み受付」) に出す。
//! - **停止** ([`WriteControl::set_enabled`] の `false`): ライブフラグを
//!   **ロックを取る前に**落とし (書き込みは即座に止まる)、状態ファイル → DB
//!   の順に保存する。どちらか一方に保存できれば「停止した」(再起動しても
//!   停止のまま) とし、保存できなかった側は [`WriteControlChange::warning`]
//!   で応答・ログ・監査に出す。両方失敗したときだけ失敗とする。
//! - **再開** (`true`): 両方の保存に成功したときだけライブフラグを立てる。
//!   片方でも失敗したらライブフラグには触れずにエラーを返す。
//!
//! ### 再開は、確定するまで停止の記録を消さない (#439 レビュー P1-1、2026-09-25)
//!
//! 停止の記録は DB と状態ファイルの**どちらか一方にしか無い**ことがある
//! (停止の保存が片方だけ成功したとき)。再開が片方を「有効」にした後で
//! もう片方の保存に失敗すると、唯一の停止の記録が消え、再起動で黙って有効に
//! なる。保存の順番を入れ替えるだけでは、停止がもう片方にだけ残っている
//! 場合に同じことが起きる。そこで再開は次の 3 段で保存する:
//!
//! 1. 状態ファイルに **「再開の途中」** (`writeEnabled: false` +
//!    `resumePending: true`) を書く。起動時はこれを**停止**として扱う
//!    ([`FileStartupState::ResumePending`])。書けなければ DB に触れずに
//!    やめる。
//! 2. DB を「有効」にする。失敗したらファイルは「再開の途中」のまま。
//! 3. DB に保存できたときだけ、状態ファイルを「有効」にする。**この rename が
//!    再開の確定点**で、失敗したらファイルは「再開の途中」のまま。
//!
//! 守る性質: **直前の永続状態が停止なら、再開が失敗として返った・確定点より
//! 前で落ちた後の再起動は、必ず停止で起動する**。1 より前は元の状態 (停止)、
//! 1 の途中は原子的な置き換えなので元の状態か「再開の途中」、1 の後から
//! 3 の確定までは状態ファイルが「再開の途中」(DB の値によらず停止) だから。
//! 確定点の後は両方が「有効」で、再開は成功を返す。状態ファイルを持たない構成
//! ([`WriteControl::new`]) は DB だけで、この性質の対象外。
//! 表で確かめるテスト: `tests::a_failed_or_crashed_resume_always_restarts_stopped`。
//!
//! 「再開の途中」は `writeEnabled: false` と組にして書くので、`resumePending`
//! を知らない読み手にも停止に見える。項目の無い (足す前の) ファイルは従来
//! どおり読める (形式番号は 1 のまま)。
//!
//! ### ロック順序 (一方向に固定)
//!
//! `op_lock` (tokio の非同期ロック。保存の一連を直列化) → `stop_state`・
//! `lagging_stop_db`・`lagging_resume_db` (std の同期ロック。世代と並んでいる
//! 停止の数を読み書きする・ライブフラグを切り替える・書き込みの控えを出し
//! 入れする短い区間だけ) → 状態ファイルの書き込みの直列化
//! (`STATE_FILE_WRITE_LOCK`、std の同期ロック。`spawn_blocking` の中だけ)。
//! 同期ロックを持ったまま `op_lock` を待つ経路は無い (同期ロックは await を
//! またいで持たず、2 つを同時に持たない)。停止の知らせ (`stop_signal`、tokio の
//! `Notify`) は `stop_state` の中で鳴らす:
//!
//! - **停止**は `stop_state` を取って世代を進めライブフラグを落とし、
//!   「並んでいる停止」を 1 つ数え (`WriteControl::begin_stop`)、待っている
//!   再開に知らせ、**それを離してから** `op_lock` を待つ。`op_lock` を取れたら、
//!   もう一度世代を進めてライブフラグを落とし、「並んでいる停止」から外して
//!   から保存する。
//! - **再開**は、**最初に待つ前に** (`op_lock`、遅れている停止の DB 書き込み)
//!   世代を読んで覚え、すぐに離す (#439 レビュー P1-2)。待ち終えたら、世代が
//!   変わった**か、並んでいる停止がある**かを確かめ、どちらかなら何も保存せずに
//!   やめる (下の「並んでいる停止があれば、再開は始めない」)。保存が済んだら
//!   `stop_state` を取って、同じ条件に当たらないときだけライブフラグを立てる。
//!
//! これで:
//!
//! - 再開の保存が DB 待ちで詰まっていても、停止はライブフラグを即座に
//!   落とせる (ライブフラグは `op_lock` に並ぶ前に落ちる)。
//! - 再開の保存中・待機中 (`op_lock` や遅れている停止の DB 書き込みを
//!   待っている間) に停止が割り込んだら、その再開はライブフラグを立てない
//!   ([`WriteControlChange::interrupted_by_stop`])。割り込んだ停止は再開の
//!   後で保存するので、最後に残る永続値も停止になる。
//! - 停止が成功を返した後に、ライブフラグだけが有効で残ることは無い。
//!
//! ### 停止は、固まった再開の後ろで待たない (2026-10-10)
//!
//! 再開は `op_lock` を持ったまま、遅れている停止の DB 書き込みと自分の DB
//! 保存を**制限なしで**待つ (理由は下の「再開には制限を付けない」)。DB が
//! 固まると、その後ろで `op_lock` を待つ停止の応答も返らなくなっていた
//! (ライブフラグは先に落ちるので書き込みは止まるが、REST・MCP の応答が
//! 返らない)。そこで再開の長い待ちを、**停止が来たらやめる**形にした
//! (再開に制限時間を付けるのではない):
//!
//! - 停止は世代を進めたのと同じロックの中で `stop_signal` を鳴らす。再開は
//!   受け口を登録してから世代を確かめて待つので、知らせを取りこぼさない
//!   (`WriteControl::stopped_after`)。
//! - **遅れている停止の DB 書き込みを待っている間**に停止が来たら、待つのを
//!   やめ、終わっていない書き込みを `lagging_stop_db` に順番どおり戻して
//!   (次の再開がまた待つ) 何も保存せずにやめる。
//! - **自分の DB 保存の最中**に停止が来たら (DB 保存は別タスクで走らせて
//!   いる)、完了を待たずにやめる。書き込みは取り消さず、`op_lock` を離す
//!   **前に** `lagging_resume_db` へ渡す。状態ファイルは「再開の途中」
//!   (起動時は停止扱い) のままで「有効」は書かない。
//! - 停止は `op_lock` を取ってから `lagging_resume_db` を**全部取り出し**、
//!   自分の DB 書き込みを**それらの完了の後に**行う別タスクにする。だから
//!   見捨てた再開の「有効」が、停止の「停止」より後に DB に届くことは無い。
//!   取り出した書き込みを待つ時間も停止の制限時間 (5 秒) に含めるので、
//!   再開の DB 保存が固まっていれば停止は 5 秒で `db_timed_out` として返る
//!   (状態ファイルに「停止」を書けていれば成功)。打ち切った停止の書き込みは
//!   `lagging_stop_db` に入るので、次の再開は (間接的に) 見捨てた再開の
//!   書き込みの完了も待つ (これも停止が来ればやめられる)。
//! - **「再開の途中」を書いている間**に停止が来たら、書き終えた後で DB には
//!   触れずにやめる (要らない「有効」を DB に書かず、停止をその完了待ちに
//!   しない)。状態ファイルは「再開の途中」のまま。
//! - 守る性質: **割り込まれた再開には、必ず割り込んだ停止が後に続く**
//!   (再開が割り込みを検出するのは世代が進んだか並んでいる停止があるとき =
//!   停止が `begin_stop` (または [`WriteControl::disable`]) を通ったときで、
//!   運用の停止は必ずその後 `op_lock` を取って保存する。本番の呼び出しは
//!   停止を別タスクで最後まで走らせる - 下の「呼び出し側が future を捨てても」)。
//!   再開は `lagging_resume_db` へ渡してから `op_lock` を離すので、続く停止は
//!   必ずそれを引き取る。
//!
//! #### 並んでいる停止があれば、再開は始めない (2026-10-10 監査 P2-1)
//!
//! 停止はライブフラグを落とした (世代を進めた) 後に `op_lock` を待つ。その
//! 間に始まった再開は**進んだ後の**世代を覚えるので、世代の比較では割り込みが
//! 見えず、知らせも来ない。この再開が停止より先に `op_lock` を取り、DB 保存で
//! 固まると、停止は `op_lock` を制限なしで待つことになる (受付は止まっているが、
//! REST・MCP の応答が返らない)。そこで停止は世代を進めるのと同じロックの中で
//! 「並んでいる停止」の数を増やし、`op_lock` を取れたら減らす (`QueuedStop`。
//! 停止の future が捨てられた・panic したときも `Drop` で減らす)。再開は
//! 「世代が変わった**または**並んでいる停止がある」を「停止が来た」とみなす
//! (`StopState::stopped_since`) ので、停止が並んでいる間に始まった再開は、
//! 何も保存せずに割り込まれて返る (409。停止が勝つ)。停止が `op_lock` を
//! 取った後に来た再開は、通常どおりその停止の後に並ぶ。
//!
//! これで停止が `op_lock` を待つ時間は、次のどれかで抑えられる:
//!
//! - 先に `op_lock` を持つ再開の反応 (知らせを受けて、または並んでいる停止を
//!   見て即座にやめる)。ただし、その再開が状態ファイルを書いている最中なら、
//!   その書き込みが終わるまで (ローカルのファイル。停止自身の状態ファイルの
//!   書き込みと同じ種類)。
//! - 先に並んだ停止 (それぞれ状態ファイルの書き込み + 5 秒)。
//!
//! 状態ファイルを持たない構成 ([`WriteControl::new`]、テスト用) も同じで、
//! ファイルを書かないだけ。
//!
//! ### 呼び出し側が future を捨てても (2026-10-10、監査 P2-2)
//!
//! REST では、クライアントが切断すると hyper がハンドラの future を捨てる
//! (MCP も同じ)。**本番の呼び出し (REST・MCP) は
//! [`WriteControl::set_enabled_detached`] で停止・再開を別タスクにし、最後まで
//! 走らせる**。ハンドラの future が捨てられても、停止・再開そのものは途中で
//! 止まらない (停止は必ず `op_lock` を取って両方に保存し、再開は割り込まれる
//! か確定するまで進む)。結果を受け取る相手がいないだけで、注意書き・ログは
//! 通常どおり更新される。呼び出し側の監査 (REST の `record_write`・
//! 失敗の監査、MCP の監査) と通知は、`follow_up` として**同じ別タスクの中で**
//! 停止・再開の後に行うので、切断されても 1 件だけ残る (#437 の「非常停止の
//! 監査を失わない」を切断にも広げる。監査は従来どおり
//! `AuditLogService::record` = 3 秒で打ち切って保留、を通る)。停止・再開が
//! panic しても、保存できなかったものとして `follow_up` (失敗の監査) を呼ぶ。
//!
//! [`WriteControl::set_enabled`] の future を直接捨てたとき (テスト・
//! ランタイムの終了) に守るのは、次の 2 つだけ:
//!
//! - **走っている DB 書き込みを見失わない**。再開の DB 書き込みは
//!   `lagging_resume_db` (次の停止がその後に書く) へ、待っていた遅れている
//!   停止の DB 書き込みは `lagging_stop_db` の先頭へ順番どおり、制限時間を待つ
//!   間の停止の DB 書き込みは `lagging_stop_db` (次の再開が待つ) へ、それぞれの
//!   `Drop` が `op_lock` を離す前に戻す。停止が引き取った再開の書き込みは、
//!   取り出してから停止の DB タスクに渡すまで await を挟まない。並んでいる
//!   停止の数も `Drop` で戻す。
//! - **状態ファイルが半端な内容にならない** (原子的な置き換え。同じプロセスの
//!   書き込みどうしは `STATE_FILE_WRITE_LOCK` で直列)。
//!
//! **順序は守らない**: 状態ファイルの書き込みは `spawn_blocking` で、future を
//! 捨てても止まらず、`op_lock` を離した後で置き換わり得る。たとえば再開の
//! 確定 (状態ファイルを「有効」) の途中で捨てられ、直後の停止が「停止」を
//! 書いた後に遅れた「有効」が置き換わると、停止を受け付けたのに再起動で有効に
//! なり得る。また、`op_lock` を待つ間に捨てられた停止は、ライブフラグを落とす
//! だけで何も保存しない。これらを本番で起こさないために、上のとおり別タスクで
//! 最後まで走らせる。
//!
//! ### 停止の DB 保存は 5 秒で打ち切る (#433 監査、2026-09-24)
//!
//! DB がエラーを返さずに固まると、停止の応答が DB を待って返らない
//! (ライブフラグと状態ファイルは先に済んでいても)。そこで**停止の DB 保存
//! だけ**に [`STOP_DB_SAVE_TIMEOUT`] (5 秒) の制限を付け、超えたら
//! 「DB に保存できなかった (タイムアウト)」として扱う
//! ([`WriteControlChange::db_timed_out`])。状態ファイルに保存できていれば
//! 停止は成功 (200)。
//!
//! - 打ち切っても DB への書き込みは**取り消さない** (取り消しても SQLite の
//!   接続側で実行され得るので、完了を追えなくなるだけ)。書き込みは別タスクで
//!   続け、その完了を `lagging_stop_db` に覚える。中身は「停止」なので、遅れて
//!   完了しても安全側。
//! - **次の再開は、遅れている停止の DB 書き込みの完了を待ってから**自分の
//!   保存を始める (`op_lock` の中で待つ。待っている間に停止が来たらやめて、
//!   終わっていない書き込みは次の再開に回す)。これで「再開の『有効』を、遅れて
//!   届いた停止の『停止』が上書きする」順序の逆転が起きない。
//! - 遅れて完了したら、その停止より新しい停止・再開が記録されていない
//!   ときに限り、注意書きをその結果で更新する (両方に保存できたなら消える)。
//!   古い結果が新しい表示を巻き戻さないよう、記録の通し番号で比べる。
//! - **再開には制限を付けない**: 打ち切った書き込みが後から「有効」を DB に
//!   残すと、失敗を返したのに再起動で有効になり得るため。その代わり、再開の
//!   長い待ちは停止が来たらやめる (上の「停止は、固まった再開の後ろで待た
//!   ない」)。見捨てた書き込みは停止の DB 書き込みより前に並べるので、
//!   「有効」が最後に残ることは無い。
//!
//! REST の停止は、#431 のセッション照合 (`crate::rest` の
//! `STOP_SESSION_CHECK_TIMEOUT`、同じ 5 秒) を通ってから来るので、DB が
//! 固まっているときの REST の停止の応答は、停止の監査 (#437、監査の保留が
//! INSERT を待つ上限 3 秒 - `crate::audit_spool`) を足して最長でおよそ
//! 5 + 5 + 3 秒になる。同時に走っている再開が DB を待っていても、停止が来た
//! 時点でその再開は待つのをやめ、停止が並んでいる間に始まった再開は保存を
//! 始めないので、`op_lock` を待つ間は状態ファイルの書き込み程度で、この上限
//! から外れない (先に並んだ停止があれば、その分、つまりそれぞれ状態ファイルの
//! 書き込み + 5 秒だけ延びる。上の「並んでいる停止があれば、再開は始めない」)。
//! ライブフラグは並ぶ前に落ちる。
//!
//! ### ファイルの書き込み中に落ちた場合
//!
//! 一時ファイル (`<名前>.tmp`) に書いて `sync_all` し、`rename` で置き換える
//! (Unix では親ディレクトリも `sync_all`)。途中で落ちても、状態ファイルは
//! 前の内容か新しい内容のどちらかで、半端な内容は残らない。残った一時
//! ファイルは読まず、次の書き込みで上書きする。一時ファイルの名前は 1 つ
//! なので、同じプロセスの書き込みどうしは作成 → rename を
//! `STATE_FILE_WRITE_LOCK` で直列にする (他方の書きかけの一時ファイルを
//! rename しない)。
//!
//! [`WriteControl::was_enabled_before_restart`] は「起動時に復元した値」を
//! 指す名称として残す (外部クライアント Thermal Monitor が
//! `GET /api/v1/status` の `write_was_enabled_before_restart` を読んでいる
//! ため、フィールド名・REST フィールド名とも互換のため変更しない)。

use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_core::BantoError;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::collections::VecDeque;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

use crate::hub_log::{log_err_line, log_line};

/// データディレクトリ内の状態ファイル名 (#433)。tstore の剪定
/// (`banto_tstore::prune_files`) は `YYYYMMDD-NNN.sqlite3` の形のファイル
/// しか触らないので、同じディレクトリに置いても消されない。
pub const STATE_FILE_NAME: &str = "write-control-state.json";

/// 停止の DB 保存の制限時間 (このモジュール doc「停止の DB 保存は 5 秒で
/// 打ち切る」)。#431 のセッション照合の制限 (`crate::rest` の
/// `STOP_SESSION_CHECK_TIMEOUT`) と同じ 5 秒。再開には使わない。
pub const STOP_DB_SAVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 再開が DB に保存しなかった理由 (状態ファイルに「再開の途中」を書けなかった)。
const NOT_SAVED_FILE_FAILED: &str = "状態ファイルに保存できなかったため、DB には保存していません";

/// 再開が状態ファイルを「有効」にしなかった理由 (DB に保存できなかった)。
const KEPT_RESUME_PENDING: &str =
    "DB に保存できなかったため「再開の途中」のままにしました（再起動すると停止で起動します）";

/// 再開がどこにも保存しなかった理由 (待っている間に停止が来た)。
const NOT_SAVED_INTERRUPTED: &str = "停止が要求されたため保存していません";

/// 再開が DB への保存の完了を待つのをやめた理由 (DB の保存中に停止が来た)。
/// 書き込みは取り消していないので、遅れて「有効」が DB に残り得るが、割り込んだ
/// 停止の DB 保存はその完了を待ってから行う (このモジュール doc「停止は、
/// 固まった再開の後ろで待たない」)。
const DB_ABANDONED_ON_STOP: &str = "DB への保存中に停止が要求されたため、完了を待たずにやめました（書き込みは取り消していないので遅れて「有効」が保存される場合がありますが、停止の DB 保存は必ずその後に行います）";

/// 再開が状態ファイルを「有効」にしなかった理由 (「再開の途中」を書いている間に
/// 停止が来たので、DB に触れずにやめた)。
const KEPT_RESUME_PENDING_BEFORE_DB: &str = "「再開の途中」を書いている間に停止が要求されたため、DB には保存せず「再開の途中」のままにしました（「有効」は書いていません。再起動すると停止で起動します）";

/// 再開が状態ファイルを「有効」にしなかった理由 (DB の保存中に停止が来た)。
const KEPT_RESUME_PENDING_ON_STOP: &str = "DB への保存中に停止が要求されたため「再開の途中」のままにしました（「有効」は書いていません。再起動すると停止で起動します）";

/// 状態ファイルの形式番号。これ以外の値は「壊れている」として停止側に倒す。
const STATE_FILE_FORMAT: u64 = 1;

/// `data_dir` (実効値。`BANTO_HUB_DATA` 等の上書き適用後) の中の状態ファイル。
pub fn state_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE_NAME)
}

// --- 起動時の判定 (純関数) ----------------------------------------------------

/// 起動時に DB (`write_control_state`) から読んだ状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbStartupState {
    Enabled,
    Disabled,
    /// 読めなかった (理由)。
    Unreadable(String),
}

/// 起動時に状態ファイルから読んだ状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStartupState {
    /// ファイルが無い (#433 より前からの環境、または初回起動)。
    Absent,
    Enabled,
    Disabled,
    /// 「再開の途中」(#439 レビュー P1-1)。再開が状態ファイルに書く最初の
    /// 記録で、再開の両方の保存が済むまで停止の記録として残る。起動時は
    /// 停止として扱う。
    ResumePending,
    /// 読めない・壊れている (理由。パスを含む)。
    Corrupt(String),
}

/// 起動時にライブフラグをどうするか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupDecision {
    pub enabled: bool,
    /// DB と状態ファイルの食い違い・読めなかったことの説明。無ければ `None`。
    pub warning: Option<String>,
}

/// 起動時の判定 (#433)。**どちらか一方でも停止なら停止**。DB を読めない・
/// ファイルが壊れている・ファイルが「再開の途中」のときも停止。ファイルが
/// 無いときだけ DB に従う。
/// 総当たりの表は `tests::startup_decision_table` を参照。
pub fn decide_startup(db: &DbStartupState, file: &FileStartupState) -> StartupDecision {
    let db_allows = matches!(db, DbStartupState::Enabled);
    let file_allows = matches!(file, FileStartupState::Absent | FileStartupState::Enabled);
    let enabled = db_allows && file_allows;

    let mut problems: Vec<String> = Vec::new();
    if let DbStartupState::Unreadable(reason) = db {
        problems.push(format!("DB から状態を読めませんでした（{reason}）"));
    }
    if let FileStartupState::Corrupt(reason) = file {
        problems.push(format!("状態ファイルを読めませんでした（{reason}）"));
    }
    match (db, file) {
        (_, FileStartupState::ResumePending) => problems.push(
            "前回の書き込み受付の再開が完了していませんでした（状態ファイルが「再開の途中」のままでした）"
                .to_string(),
        ),
        (DbStartupState::Enabled, FileStartupState::Disabled) => problems.push(
            "DB は「有効」、状態ファイルは「停止」でした（前回の停止を DB に保存できなかった可能性があります）"
                .to_string(),
        ),
        (DbStartupState::Disabled, FileStartupState::Enabled) => problems.push(
            "DB は「停止」、状態ファイルは「有効」でした（前回の停止を状態ファイルに保存できなかった可能性があります）"
                .to_string(),
        ),
        _ => {}
    }

    let warning = if problems.is_empty() {
        None
    } else {
        Some(format!(
            "{}。安全のため書き込み受付を停止で起動しました。状態を確かめてから再開してください。",
            problems.join("／")
        ))
    };
    StartupDecision { enabled, warning }
}

/// 起動時に DB と状態ファイルを読み、[`decide_startup`] で判定する。
pub async fn load_startup_decision(pool: &SqlitePool, state_file: &Path) -> StartupDecision {
    let db = match load_persisted_enabled(pool).await {
        Ok(true) => DbStartupState::Enabled,
        Ok(false) => DbStartupState::Disabled,
        Err(err) => DbStartupState::Unreadable(err.to_string()),
    };
    let file = read_state_file(state_file).await;
    decide_startup(&db, &file)
}

/// 状態ファイルを読む。無ければ [`FileStartupState::Absent`]、読めない・
/// 形式が違うなら [`FileStartupState::Corrupt`]。
pub async fn read_state_file(path: &Path) -> FileStartupState {
    let owned = path.to_path_buf();
    match tokio::task::spawn_blocking(move || read_state_file_blocking(&owned)).await {
        Ok(state) => state,
        Err(err) => FileStartupState::Corrupt(format!("{}: {err}", path.display())),
    }
}

fn read_state_file_blocking(path: &Path) -> FileStartupState {
    match std::fs::read(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => FileStartupState::Absent,
        Err(err) => FileStartupState::Corrupt(format!("{}: {err}", path.display())),
        Ok(bytes) => match parse_state_file(&bytes) {
            Ok(state) => state,
            Err(reason) => FileStartupState::Corrupt(format!("{}: {reason}", path.display())),
        },
    }
}

/// 状態ファイルの中身を読む。`resumePending` は #439 レビュー P1-1 で足した
/// 省略可の項目 (無ければ `false`。足す前の形式もそのまま読める)。「再開の
/// 途中」は必ず `writeEnabled: false` と組にして書くので、項目を知らない
/// 読み手 (足す前の版) にも停止に見える。`resumePending: true` なのに
/// `writeEnabled: true` のような食い違いは「壊れている」(停止) にする。
fn parse_state_file(bytes: &[u8]) -> Result<FileStartupState, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    if value.get("format").and_then(Value::as_u64) != Some(STATE_FILE_FORMAT) {
        return Err("形式番号が違います".to_string());
    }
    let enabled = value
        .get("writeEnabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "writeEnabled がありません".to_string())?;
    let resume_pending = match value.get("resumePending") {
        None => false,
        Some(flag) => flag
            .as_bool()
            .ok_or_else(|| "resumePending が真偽値ではありません".to_string())?,
    };
    match (enabled, resume_pending) {
        (true, false) => Ok(FileStartupState::Enabled),
        (false, false) => Ok(FileStartupState::Disabled),
        (false, true) => Ok(FileStartupState::ResumePending),
        (true, true) => Err("writeEnabled と resumePending が食い違っています".to_string()),
    }
}

/// 状態ファイルに書く記録。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileRecord {
    Enabled,
    Disabled,
    /// 「再開の途中」(`writeEnabled: false` + `resumePending: true`)。
    ResumePending,
}

/// 状態ファイルを原子的に書く (一時ファイル → `sync_all` → `rename`)。
/// このモジュール doc「ファイルの書き込み中に落ちた場合」参照。
pub async fn write_state_file(
    path: &Path,
    enabled: bool,
    actor: Option<&str>,
) -> Result<(), String> {
    let record = if enabled {
        FileRecord::Enabled
    } else {
        FileRecord::Disabled
    };
    write_state_record(path, record, actor).await
}

async fn write_state_record(
    path: &Path,
    record: FileRecord,
    actor: Option<&str>,
) -> Result<(), String> {
    let owned = path.to_path_buf();
    let actor = actor.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        write_state_file_blocking(&owned, record, actor.as_deref())
            .map_err(|err| format!("{}: {err}", owned.display()))
    })
    .await
    .map_err(|err| format!("{}: {err}", path.display()))?
}

/// 状態ファイルの書き込み (一時ファイルの作成 → rename) を直列にする
/// ([`write_state_file_blocking`])。
static STATE_FILE_WRITE_LOCK: SyncMutex<()> = SyncMutex::new(());

fn write_state_file_blocking(
    path: &Path,
    record: FileRecord,
    actor: Option<&str>,
) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "親ディレクトリがありません",
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let changed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut body = json!({
        "format": STATE_FILE_FORMAT,
        "writeEnabled": record == FileRecord::Enabled,
        "changedAtUnixMs": changed_at_ms,
        "changedBy": actor,
    });
    if record == FileRecord::ResumePending {
        body["resumePending"] = json!(true);
    }
    let body = serde_json::to_vec_pretty(&body).map_err(std::io::Error::other)?;

    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    // 一時ファイルの名前は 1 つなので、同じプロセスの書き込みどうしは
    // 作成 → rename を直列にする (捨てられた future の書き込みが後の書き込みと
    // 重なっても、他方の書きかけの一時ファイルを rename しない。順序までは
    // 守らない - このモジュール doc「呼び出し側が future を捨てても」)。
    let _serialized = lock_ignoring_poison(&STATE_FILE_WRITE_LOCK);
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    #[cfg(unix)]
    {
        if let Ok(dir_handle) = std::fs::File::open(dir) {
            let _ = dir_handle.sync_all();
        }
    }
    Ok(())
}

// --- ライブフラグと停止・再開 ---------------------------------------------------

/// ライブの「書き込み受付中か」フラグ + 起動時に復元した値 + 停止・再開の
/// 保存 (#433)。`Arc` で共有する前提。
#[derive(Debug)]
pub struct WriteControl {
    /// ライブの「/api/v1/values/{tag} への書き込みを受け付けるか」フラグ。
    /// 読み取りはロックを取らない。
    enabled: AtomicBool,
    /// 起動時に復元した値 (= 構築時のライブフラグの初期値)。
    was_enabled_before_restart: bool,
    /// 状態ファイル。`None` は状態ファイルを持たない構成 ([`Self::new`]、
    /// 他機能のテスト用) - そのときは DB だけに保存する (#433 より前の挙動)。
    state_file: Option<PathBuf>,
    /// 停止の世代と、`op_lock` に並んでいる停止の数。ライブフラグの切り替えは
    /// このロックの中で行う (このモジュール doc「ロック順序」「並んでいる停止が
    /// あれば、再開は始めない」)。
    stop_state: SyncMutex<StopState>,
    /// 停止の知らせ。[`Self::disable`] が世代を進めたのと同じロックの中で
    /// `notify_waiters` する。再開は `op_lock` を持ったまま長く待つところ
    /// (遅れている停止の DB 書き込み・自分の DB 保存) でこれを待ち、停止が
    /// 来たら待つのをやめる (このモジュール doc「停止は、固まった再開の後ろで
    /// 待たない」)。取りこぼさない手順は [`Self::stopped_after`]。
    stop_signal: Notify,
    /// 停止・再開の保存の一連を直列化する (このモジュール doc「ロック順序」)。
    op_lock: AsyncMutex<()>,
    /// いまの保存状態の注意書き (起動時の食い違い、または直近の停止・再開の
    /// 保存の失敗)。両方に保存できた停止・再開で消える。遅れて完了した停止の
    /// DB 書き込みも更新するので `Arc` で共有する。
    persistence_warning: Arc<SyncMutex<WarningSlot>>,
    /// 制限時間を超えてまだ終わっていない、停止の DB 書き込み (打ち切った停止の
    /// 見届け役、または完了を見ずに捨てられた停止の DB 書き込みそのもの)。
    /// 次の再開はこれの完了を待つ (このモジュール doc「停止の DB 保存は 5 秒で
    /// 打ち切る」「呼び出し側が future を捨てても」)。
    lagging_stop_db: SyncMutex<Vec<DbWrite>>,
    /// 停止に割り込まれた・捨てられた再開が、完了を待たずに残した DB 書き込み
    /// (中身は「有効」)。次の停止が `op_lock` の中で取り出し、自分の DB 書き込み
    /// をこれらの完了の後に行う (このモジュール doc「停止は、固まった再開の
    /// 後ろで待たない」)。
    lagging_resume_db: SyncMutex<Vec<DbWrite>>,
}

/// 停止の世代と、並んでいる停止の数 ([`WriteControl::stop_state`])。
#[derive(Debug, Default)]
struct StopState {
    /// 停止のたびに進む ([`WriteControl::disable`])。
    generation: u64,
    /// ライブフラグを落としたが、まだ `op_lock` を取れていない停止の数
    /// ([`QueuedStop`])。0 より大きい間は、再開は「停止が来た」とみなす。
    queued: u64,
}

impl StopState {
    /// 再開が「停止が来た」とみなすか: `started_generation` を覚えた後に世代が
    /// 進んだ、**または**並んでいる停止がある (覚える前から並んでいた停止も
    /// 含む。このモジュール doc「並んでいる停止があれば、再開は始めない」)。
    fn stopped_since(&self, started_generation: u64) -> bool {
        self.generation != started_generation || self.queued > 0
    }
}

/// ライブフラグを落としてから `op_lock` を取るまでの停止 1 件
/// ([`WriteControl::begin_stop`])。`op_lock` を取れた・停止の future が捨て
/// られた・panic したときに `Drop` で数を戻す。
struct QueuedStop<'a> {
    state: &'a SyncMutex<StopState>,
}

impl Drop for QueuedStop<'_> {
    fn drop(&mut self) {
        let mut state = lock_ignoring_poison(self.state);
        state.queued = state.queued.saturating_sub(1);
    }
}

/// 完了を追いかける DB 書き込み (別タスク)。結果は待つ側では使わない
/// (停止・再開の順序を守るために完了だけを待つ)。
type DbWrite = JoinHandle<Result<(), String>>;

/// 走っている DB 書き込み 1 本を持ち、結果を受け取らないまま捨てられたら
/// (`op_lock` を持つ側の future が捨てられた・割り込まれた) `slot` に渡す。
/// `op_lock` のガードより**後に**宣言して、`op_lock` を離す前に渡す。
struct TrackedDbWrite<'a> {
    slot: &'a SyncMutex<Vec<DbWrite>>,
    task: Option<DbWrite>,
}

impl<'a> TrackedDbWrite<'a> {
    fn spawn<F>(slot: &'a SyncMutex<Vec<DbWrite>>, write: F) -> Self
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        Self {
            slot,
            task: Some(tokio::spawn(write)),
        }
    }

    /// 走っている書き込み (取り出す前は必ずある)。
    fn task(&mut self) -> &mut DbWrite {
        self.task
            .as_mut()
            .expect("the DB write has not been taken yet")
    }

    /// 結果を受け取った (または自分で後を引き受ける) ので、渡さない。
    fn take(&mut self) -> Option<DbWrite> {
        self.task.take()
    }
}

impl Drop for TrackedDbWrite<'_> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            lock_ignoring_poison(self.slot).push(task);
        }
    }
}

/// 再開が待っている、遅れている停止の DB 書き込みの残り (古い順)。待ち終わる
/// 前にやめた (停止が来た・future が捨てられた) ら、残りを `slot` の先頭に順番
/// どおり戻す (その間に足された分はその後ろ)。
struct PendingStopWrites<'a> {
    slot: &'a SyncMutex<Vec<DbWrite>>,
    pending: VecDeque<DbWrite>,
}

impl Drop for PendingStopWrites<'_> {
    fn drop(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut slot = lock_ignoring_poison(self.slot);
        let newer = std::mem::take(&mut *slot);
        slot.extend(self.pending.drain(..));
        slot.extend(newer);
    }
}

/// 注意書きと、それを記録した通し番号 (遅れて届く結果が新しい表示を巻き戻さ
/// ないため)。
#[derive(Debug, Default)]
struct WarningSlot {
    seq: u64,
    warning: Option<String>,
}

/// 停止・再開 1 回の結果 ([`WriteControl::set_enabled`])。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteControlChange {
    /// 要求した値 (`true` = 再開、`false` = 停止)。
    pub requested_enabled: bool,
    /// DB への保存の結果。
    pub db: Result<(), String>,
    /// 状態ファイルへの保存の結果。`None` は状態ファイルを持たない構成。
    pub file: Option<Result<(), String>>,
    /// 再開の保存中に停止が割り込んだため、再開しなかった。
    pub interrupted_by_stop: bool,
    /// 停止の DB 保存が [`STOP_DB_SAVE_TIMEOUT`] を超えたため打ち切った
    /// (`db` は `Err`)。書き込み自体は続いていて、遅れて完了し得る。制限時間
    /// には、割り込まれた再開が残した DB 書き込みの完了を待つ時間も含む。
    pub db_timed_out: bool,
}

impl WriteControlChange {
    pub fn db_saved(&self) -> bool {
        self.db.is_ok()
    }

    /// 状態ファイルに保存できたか (`None` = 状態ファイルを持たない構成)。
    pub fn file_saved(&self) -> Option<bool> {
        self.file.as_ref().map(Result::is_ok)
    }

    pub fn db_error(&self) -> Option<&str> {
        self.db.as_ref().err().map(String::as_str)
    }

    pub fn file_error(&self) -> Option<&str> {
        self.file
            .as_ref()
            .and_then(|r| r.as_ref().err())
            .map(String::as_str)
    }

    /// 要求どおりになり、再起動後もそのまま残るか。
    /// - 停止: どちらか一方に保存できた。
    /// - 再開: 両方に保存でき (状態ファイルを持たない構成では DB だけ)、
    ///   停止に割り込まれなかった。
    pub fn succeeded(&self) -> bool {
        if self.requested_enabled {
            self.db_saved() && self.file_saved().unwrap_or(true) && !self.interrupted_by_stop
        } else {
            self.db_saved() || self.file_saved() == Some(true)
        }
    }

    /// 両方に保存できたか (状態ファイルを持たない構成では DB だけ)。
    pub fn fully_persisted(&self) -> bool {
        self.db_saved() && self.file_saved().unwrap_or(true)
    }

    /// 応答・ログ・監査に出す説明。全部うまくいったときは `None`。
    pub fn warning(&self) -> Option<String> {
        if self.interrupted_by_stop {
            return Some(
                "停止が要求されたため、再開しませんでした（再開の保存中・保存を待っている間に停止が来た、または先に来た停止がまだ保存を始めていなかった）。"
                    .to_string(),
            );
        }
        if self.fully_persisted() {
            return None;
        }
        let db_part = match &self.db {
            Ok(()) => "DB: 保存済み".to_string(),
            Err(_) if self.db_timed_out => format!(
                "DB: タイムアウト（{} 秒以内に応答がありませんでした。遅れて保存される場合があります）",
                STOP_DB_SAVE_TIMEOUT.as_secs()
            ),
            Err(err) => format!("DB: 失敗（{err}）"),
        };
        let file_part = match &self.file {
            None => None,
            Some(Ok(())) => Some("状態ファイル: 保存済み".to_string()),
            Some(Err(err)) => Some(format!("状態ファイル: 失敗（{err}）")),
        };
        let detail = match file_part {
            Some(file_part) => format!("{db_part}／{file_part}"),
            None => db_part,
        };
        Some(if self.requested_enabled {
            format!("再開を保存できなかったため、書き込み受付を有効にしませんでした（{detail}）。")
        } else if self.succeeded() {
            format!(
                "書き込み受付を停止しました。保存できなかった側があります（{detail}）。\
                 保存できた側が停止を記録しているので、再起動しても停止のまま起動します。"
            )
        } else {
            format!(
                "書き込み受付は停止しましたが、どこにも保存できませんでした（{detail}）。\
                 再起動すると、それまでに保存されていた内容で起動します（DB と\
                 状態ファイルのどちらかが「停止」または「再開の途中」なら停止）。"
            )
        })
    }
}

impl WriteControl {
    /// 状態ファイルを持たない構成で構築する (他機能のテスト用。停止・再開は
    /// DB だけに保存する = #433 より前の挙動)。本番の起動経路
    /// (`crate::runtime`) は [`Self::restore`] を使う。
    pub fn new(enabled: bool) -> Self {
        Self::build(enabled, None, None)
    }

    /// 起動時の判定 ([`load_startup_decision`]) から構築する。`state_file` は
    /// [`state_file_path`] で求めたパス。
    pub fn restore(decision: StartupDecision, state_file: PathBuf) -> Self {
        Self::build(decision.enabled, Some(state_file), decision.warning)
    }

    fn build(enabled: bool, state_file: Option<PathBuf>, warning: Option<String>) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            was_enabled_before_restart: enabled,
            state_file,
            stop_state: SyncMutex::new(StopState::default()),
            stop_signal: Notify::new(),
            op_lock: AsyncMutex::new(()),
            persistence_warning: Arc::new(SyncMutex::new(WarningSlot { seq: 0, warning })),
            lagging_stop_db: SyncMutex::new(Vec::new()),
            lagging_resume_db: SyncMutex::new(Vec::new()),
        }
    }

    /// ライブフラグだけを立てる (保存しない。テスト用)。運用の再開は
    /// [`Self::set_enabled`] を使う。
    pub fn enable(&self) {
        let _state = lock_ignoring_poison(&self.stop_state);
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// ライブフラグだけを落とし、停止の世代を進めて、待っている再開に知らせる
    /// (保存しない)。運用の停止は [`Self::set_enabled`] を使う (これだけを
    /// 呼んで割り込んだ再開が残した DB 書き込みは、次の停止が引き取る)。
    pub fn disable(&self) {
        let mut state = lock_ignoring_poison(&self.stop_state);
        self.disable_locked(&mut state);
    }

    /// 停止の前半: [`Self::disable`] と同じことを行い、同じロックの中で
    /// 「並んでいる停止」を 1 つ数える。返したガードを `op_lock` を取れた
    /// ところで捨てる ([`Self::save_stop`])。それまでの間に始まった再開は、
    /// 世代が変わっていなくても「停止が来た」とみなす (このモジュール doc
    /// 「並んでいる停止があれば、再開は始めない」)。
    fn begin_stop(&self) -> QueuedStop<'_> {
        let mut state = lock_ignoring_poison(&self.stop_state);
        self.disable_locked(&mut state);
        state.queued = state.queued.saturating_add(1);
        QueuedStop {
            state: &self.stop_state,
        }
    }

    fn disable_locked(&self, state: &mut StopState) {
        state.generation = state.generation.wrapping_add(1);
        self.enabled.store(false, Ordering::SeqCst);
        // 世代を進めたのと同じロックの中で知らせる (世代を読んだ後に作った
        // 受け口には、その後の停止の知らせだけが届く)。
        self.stop_signal.notify_waiters();
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 起動時に復元した値 (`GET /api/v1/status` の
    /// `write_was_enabled_before_restart`)。以後の停止・再開では変わらない。
    pub fn was_enabled_before_restart(&self) -> bool {
        self.was_enabled_before_restart
    }

    /// 状態ファイルのパス (`None` = 状態ファイルを持たない構成)。
    pub fn state_file(&self) -> Option<&Path> {
        self.state_file.as_deref()
    }

    /// いまの保存状態の注意書き (管理画面の「書き込み受付」に出す)。
    pub fn persistence_warning(&self) -> Option<String> {
        lock_ignoring_poison(&self.persistence_warning)
            .warning
            .clone()
    }

    /// 停止・再開を**別タスクで最後まで**走らせ、その結果を待つ。本番の
    /// 呼び出し (REST・MCP) はこれを使う: 呼び出し側の future (クライアントが
    /// 切断した要求のハンドラ) が捨てられても、停止・再開は途中で止まらない
    /// (このモジュール doc「呼び出し側が future を捨てても」)。中身は
    /// [`Self::set_enabled`]。
    ///
    /// `follow_up` は、停止・再開が終わった後に**同じ別タスクの中で**結果を
    /// 渡して呼ぶ (呼び出し側の監査・通知。切断されても最後まで走り、結果を
    /// 返すのはその完了の後)。停止・再開が panic したときは呼ばない。
    pub async fn set_enabled_detached<A, Fut>(
        self: &Arc<Self>,
        pool: &SqlitePool,
        enabled: bool,
        actor: Option<&str>,
        follow_up: A,
    ) -> WriteControlChange
    where
        A: FnOnce(WriteControlChange) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let pool = pool.clone();
        let db_actor = actor.map(str::to_string);
        self.set_enabled_detached_with(
            enabled,
            actor,
            async move {
                persist_enabled(&pool, enabled, db_actor.as_deref())
                    .await
                    .map_err(|err| err.to_string())
            },
            follow_up,
        )
        .await
    }

    /// [`Self::set_enabled_detached`] の本体 (テストで DB の詰まりを差し込む
    /// ため)。
    async fn set_enabled_detached_with<F, A, Fut>(
        self: &Arc<Self>,
        enabled: bool,
        actor: Option<&str>,
        persist_db: F,
        follow_up: A,
    ) -> WriteControlChange
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
        A: FnOnce(WriteControlChange) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let control = Arc::clone(self);
        let actor = actor.map(str::to_string);
        let has_state_file = self.state_file.is_some();
        let task = tokio::spawn(async move {
            // 停止・再開はさらに別タスクにして、panic しても `follow_up` (失敗の
            // 監査) は呼ぶ。
            let operation = tokio::spawn(async move {
                control
                    .set_enabled_with(enabled, actor.as_deref(), persist_db)
                    .await
            });
            let change = match operation.await {
                Ok(change) => change,
                Err(err) => ended_abnormally(enabled, has_state_file, &err),
            };
            follow_up(change.clone()).await;
            change
        });
        match task.await {
            Ok(change) => change,
            Err(err) => ended_abnormally(enabled, has_state_file, &err),
        }
    }

    /// 停止 (`false`) / 再開 (`true`) し、DB と状態ファイルに保存する
    /// (このモジュール doc「停止の状態は 2 か所に記録する」)。この future を
    /// 途中で捨てると、状態ファイルの書き込みが `op_lock` を離した後に置き
    /// 換わり得る (このモジュール doc「呼び出し側が future を捨てても」)。
    /// 本番の呼び出しは [`Self::set_enabled_detached`] を使う。
    pub async fn set_enabled(
        &self,
        pool: &SqlitePool,
        enabled: bool,
        actor: Option<&str>,
    ) -> WriteControlChange {
        let pool = pool.clone();
        let db_actor = actor.map(str::to_string);
        self.set_enabled_with(enabled, actor, async move {
            persist_enabled(&pool, enabled, db_actor.as_deref())
                .await
                .map_err(|err| err.to_string())
        })
        .await
    }

    /// [`Self::set_enabled`] の本体。DB への保存を future として受け取る
    /// (テストで DB の失敗・詰まりを差し込むため)。停止では、制限時間を
    /// 超えても書き込みを続けられるよう別タスクで走らせるので `Send + 'static`。
    pub async fn set_enabled_with<F>(
        &self,
        enabled: bool,
        actor: Option<&str>,
        persist_db: F,
    ) -> WriteControlChange
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        if enabled {
            self.resume(actor, persist_db).await
        } else {
            // ロックを取る前にライブフラグを落とす (書き込みは即座に止まる)。
            // `op_lock` を取るまでは「並んでいる停止」として数える。
            let queued = self.begin_stop();
            self.save_stop(queued, actor, persist_db).await
        }
    }

    /// 再開 (このモジュール doc「再開は、確定するまで停止の記録を消さない」)。
    async fn resume<F>(&self, actor: Option<&str>, persist_db: F) -> WriteControlChange
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        // 停止の世代は、最初に待つ (`op_lock`・遅れている停止の DB 書き込み)
        // **前に**覚える (#439 レビュー P1-2)。待っている間に来た停止を
        // 「開始時からあった」ものとして取り込まないため。同期ロックは値を
        // 読んだらすぐ離す (await をまたいで持たない)。
        // ここで並んでいる停止 (ライブフラグを落としたが `op_lock` をまだ
        // 取れていない停止) があれば、下の `stopped_since` が「停止が来た」と
        // 判定する (このモジュール doc「並んでいる停止があれば、再開は始め
        // ない」)。
        let started_generation = lock_ignoring_poison(&self.stop_state).generation;
        let _op = self.op_lock.lock().await;
        // 遅れている停止の DB 書き込みが終わるまで待つ (再開の「有効」を
        // 後から「停止」で上書きされないように)。再開に制限時間は無いが、
        // 停止が来たら待つのをやめる (終わっていない分は戻す)。
        self.wait_lagging_stop_db(started_generation).await;
        // 待っている間に停止が来ていたら、何も保存せずにやめる。
        if self.stopped_since(started_generation) {
            let not_saved = || Err(NOT_SAVED_INTERRUPTED.to_string());
            let change = WriteControlChange {
                requested_enabled: true,
                db: not_saved(),
                file: self.state_file.as_ref().map(|_| not_saved()),
                interrupted_by_stop: true,
                db_timed_out: false,
            };
            self.record_outcome(&change);
            return change;
        }

        // 1. 状態ファイルに「再開の途中」(停止扱い) を書く。書けなければ
        //    DB には触れずにやめる (停止の記録を DB から消さない)。
        if let Some(path) = &self.state_file {
            if let Err(err) = write_state_record(path, FileRecord::ResumePending, actor).await {
                let change = WriteControlChange {
                    requested_enabled: true,
                    db: Err(NOT_SAVED_FILE_FAILED.to_string()),
                    file: Some(Err(err)),
                    interrupted_by_stop: false,
                    db_timed_out: false,
                };
                self.record_outcome(&change);
                return change;
            }
        }
        // 1 の書き込み中に停止が来ていたら、DB には触れずにやめる (要らない
        // 「有効」を DB に書かず、停止をその書き込みの完了待ちにしない)。
        // 状態ファイルは「再開の途中」(停止扱い) のまま。
        if self.stopped_since(started_generation) {
            let change = WriteControlChange {
                requested_enabled: true,
                db: Err(NOT_SAVED_INTERRUPTED.to_string()),
                file: self
                    .state_file
                    .as_ref()
                    .map(|_| Err(KEPT_RESUME_PENDING_BEFORE_DB.to_string())),
                interrupted_by_stop: true,
                db_timed_out: false,
            };
            self.record_outcome(&change);
            return change;
        }
        // 2. DB を「有効」にする。別タスクで走らせ、停止が来たら完了を待たずに
        //    やめる (停止を固まった再開の後ろに並ばせない)。書き込みは取り
        //    消さず、`op_lock` を離す**前に** `lagging_resume_db` へ渡す。割り
        //    込んだ停止は `op_lock` を取ってからそれを取り出し、自分の DB 書き
        //    込みをその完了の後に行うので、DB に最後に残るのは停止になる。
        //    状態ファイルは「再開の途中」(停止扱い) のままにする。この future
        //    自体が捨てられた (呼び出し側が切断した等) ときも、`db_write` の
        //    `Drop` が同じく `lagging_resume_db` へ渡す (`_op` より後に宣言して
        //    いるので、`op_lock` を離す前に渡る)。
        let mut db_write = TrackedDbWrite::spawn(&self.lagging_resume_db, persist_db);
        let joined = tokio::select! {
            biased;
            () = self.stopped_after(started_generation) => None,
            joined = db_write.task() => Some(joined),
        };
        let Some(joined) = joined else {
            // 割り込まれた: 書き込みは `db_write` を捨てるときに
            // `lagging_resume_db` へ渡る (捨てられたときと同じ経路)。
            drop(db_write);
            let change = WriteControlChange {
                requested_enabled: true,
                db: Err(DB_ABANDONED_ON_STOP.to_string()),
                file: self
                    .state_file
                    .as_ref()
                    .map(|_| Err(KEPT_RESUME_PENDING_ON_STOP.to_string())),
                interrupted_by_stop: true,
                db_timed_out: false,
            };
            self.record_outcome(&change);
            return change;
        };
        // 結果を受け取ったので、もう誰にも渡さない。
        db_write.take();
        let db = flatten_join(joined);
        // 3. DB に保存できたときだけ、状態ファイルを「有効」にする (ここが
        //    再開の確定点)。DB が失敗したら「再開の途中」のまま残す。
        let file = match &self.state_file {
            None => None,
            Some(_) if db.is_err() => Some(Err(KEPT_RESUME_PENDING.to_string())),
            Some(path) => Some(write_state_record(path, FileRecord::Enabled, actor).await),
        };
        let mut change = WriteControlChange {
            requested_enabled: true,
            db,
            file,
            interrupted_by_stop: false,
            db_timed_out: false,
        };
        if change.fully_persisted() {
            let state = lock_ignoring_poison(&self.stop_state);
            if !state.stopped_since(started_generation) {
                self.enabled.store(true, Ordering::SeqCst);
            } else {
                change.interrupted_by_stop = true;
            }
        }
        self.record_outcome(&change);
        change
    }

    /// `started_generation` を覚えた後に停止が来たか、または並んでいる停止が
    /// あるか ([`StopState::stopped_since`])。
    fn stopped_since(&self, started_generation: u64) -> bool {
        lock_ignoring_poison(&self.stop_state).stopped_since(started_generation)
    }

    /// `started_generation` を覚えた後に停止が来るまで待つ (もう来ていれば
    /// すぐ返る)。知らせを取りこぼさないよう、**受け口を登録してから**世代を
    /// 確かめる: 確かめた後の停止は登録済みの受け口に届き、確かめる前の停止は
    /// 世代の比較で分かる。起きたら世代を確かめ直す (起き違いは待ち直す)。
    /// 途中で捨ててよい (何も持たない)。
    async fn stopped_after(&self, started_generation: u64) {
        loop {
            let notified = self.stop_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.stopped_since(started_generation) {
                return;
            }
            notified.await;
        }
    }

    /// 再開が `op_lock` の中で、遅れている停止の DB 書き込みの完了を古い順に
    /// 待つ。`started_generation` の後に停止が来たら待つのをやめ、終わって
    /// いない書き込みを `lagging_stop_db` の先頭に順番どおり戻す (次の再開が
    /// また待つ。失わない)。この future が途中で捨てられたときも同じく戻す
    /// (どちらも [`PendingStopWrites`] の `Drop` の 1 経路)。停止が来たかは
    /// 呼び出し側が世代で確かめる。
    async fn wait_lagging_stop_db(&self, started_generation: u64) {
        let mut waiting = PendingStopWrites {
            slot: &self.lagging_stop_db,
            pending: std::mem::take(&mut *lock_ignoring_poison(&self.lagging_stop_db)).into(),
        };
        let stop = self.stopped_after(started_generation);
        tokio::pin!(stop);
        while let Some(handle) = waiting.pending.front_mut() {
            let finished = tokio::select! {
                biased;
                () = &mut stop => false,
                _ = handle => true,
            };
            if !finished {
                break;
            }
            // 完了を見届けたものだけを外す (この間に await は無い)。
            waiting.pending.pop_front();
        }
        // 残りがあれば `waiting` を捨てるときに戻る。
    }

    /// 停止の保存 ([`Self::begin_stop`] でライブフラグを落とした後に呼ぶ)。
    async fn save_stop<F>(
        &self,
        queued: QueuedStop<'_>,
        actor: Option<&str>,
        persist_db: F,
    ) -> WriteControlChange
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let _op = self.op_lock.lock().await;
        // `op_lock` を取れたら、もう一度落としてから「並んでいる停止」を外す。
        // 並んでいる間に始まった再開は `stopped_since` で止まるので、ライブ
        // フラグを立てて先に済ませることは無いが、`disable` だけを呼んだ
        // 経路 (テスト用) もあるので、ここでも落とす (#439 レビュー P1-2:
        // 停止が成功したのにライブフラグだけ有効、を残さない)。世代を進めて
        // から外すので、その間に世代を覚えた再開も割り込みとして見える。
        self.disable();
        drop(queued);
        let file = self.persist_file(false, actor).await;
        // 割り込まれた・捨てられた再開が残した DB 書き込み (「有効」) を引き
        // 取り、この停止の DB 書き込みはそれらの完了の後に行う (DB に最後に
        // 残るのは停止)。待つ時間も下の制限時間に含める。取り出してから
        // タスクに渡すまでの間に await を挟まない (この future が捨てられても
        // 取り出した書き込みを失わない)。
        let resume_writes: Vec<DbWrite> =
            std::mem::take(&mut *lock_ignoring_poison(&self.lagging_resume_db));
        // この future が制限時間を待つ間に捨てられたら、停止の DB 書き込みは
        // そのまま `lagging_stop_db` に入る (次の再開が待つ)。結果は誰も記録
        // していないので、注意書きは更新しない。
        let mut db_write = TrackedDbWrite::spawn(&self.lagging_stop_db, async move {
            for handle in resume_writes {
                let _ = handle.await;
            }
            persist_db.await
        });
        let timed = tokio::time::timeout(STOP_DB_SAVE_TIMEOUT, db_write.task()).await;
        // ここから先に await は無い。完了したら誰にも渡さず、打ち切ったときは
        // 見届け役に渡す。
        let task = db_write.take();
        let (db, db_timed_out) = match timed {
            Ok(joined) => (flatten_join(joined), false),
            Err(_) => (
                Err(format!(
                    "{} 秒以内に応答がありませんでした",
                    STOP_DB_SAVE_TIMEOUT.as_secs()
                )),
                true,
            ),
        };
        let change = WriteControlChange {
            requested_enabled: false,
            db,
            file,
            interrupted_by_stop: false,
            db_timed_out,
        };
        let seq = self.record_outcome(&change);
        if let (true, Some(task)) = (db_timed_out, task) {
            self.follow_lagging_stop_db(task, change.file.clone(), seq);
        }
        change
    }

    /// 打ち切った停止の DB 書き込みを見届ける (`op_lock` の中で呼ぶ)。完了
    /// したら、この停止より新しい記録が無いときだけ注意書きを更新する。
    fn follow_lagging_stop_db(&self, task: DbWrite, file: Option<Result<(), String>>, seq: u64) {
        let slot = self.persistence_warning.clone();
        let follower = tokio::spawn(async move {
            let late = WriteControlChange {
                requested_enabled: false,
                db: flatten_join(task.await),
                file,
                interrupted_by_stop: false,
                db_timed_out: false,
            };
            match &late.db {
                Ok(()) => log_line("banto-hub: 書き込み受付の停止を、遅れて DB に保存しました"),
                Err(err) => log_err_line(&format!(
                    "banto-hub: 書き込み受付の停止を DB に保存できませんでした（遅れて失敗）: {err}"
                )),
            }
            let mut slot = lock_ignoring_poison(&slot);
            if slot.seq == seq {
                slot.warning = late.warning();
            }
            late.db
        });
        lock_ignoring_poison(&self.lagging_stop_db).push(follower);
    }

    async fn persist_file(&self, enabled: bool, actor: Option<&str>) -> Option<Result<(), String>> {
        match &self.state_file {
            None => None,
            Some(path) => Some(write_state_file(path, enabled, actor).await),
        }
    }

    /// `op_lock` の中で呼ぶ。注意書きを更新し、失敗をログに出す。記録の
    /// 通し番号を返す。
    fn record_outcome(&self, change: &WriteControlChange) -> u64 {
        let mut slot = lock_ignoring_poison(&self.persistence_warning);
        if change.interrupted_by_stop {
            // 割り込んだ停止が、この後で自分の保存結果を記録する。
            log_err_line(
                "banto-hub: 書き込み受付の再開は、保存中に停止が要求されたため取りやめました",
            );
            return slot.seq;
        }
        let warning = change.warning();
        if let Some(message) = &warning {
            log_err_line(&format!("banto-hub: {message}"));
        }
        slot.seq = slot.seq.wrapping_add(1);
        slot.warning = warning;
        slot.seq
    }
}

/// 停止・再開の別タスクが panic した (またはランタイムの終了で取り消された)
/// ときの結果。どこまで保存できたか分からないので、保存できなかったものとして
/// 返す (停止なら 500。ライブフラグは停止なら最初に落ちている)。
fn ended_abnormally(
    enabled: bool,
    has_state_file: bool,
    err: &tokio::task::JoinError,
) -> WriteControlChange {
    let failed = || Err(format!("停止・再開の処理が途中で終了しました（{err}）"));
    WriteControlChange {
        requested_enabled: enabled,
        db: failed(),
        file: has_state_file.then(failed),
        interrupted_by_stop: false,
        db_timed_out: false,
    }
}

fn flatten_join(joined: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    joined.unwrap_or_else(|err| Err(err.to_string()))
}

fn lock_ignoring_poison<T>(mutex: &SyncMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- DB (write_control_state) ----------------------------------------------

/// `write_control_state.enabled_persisted` (id=1 の単一行) を読む。
/// `migrations-sqlite/0102_write_control_state.sql` が必ず1行 seed するので
/// `fetch_one` で問題ない。
pub async fn load_persisted_enabled(pool: &SqlitePool) -> Result<bool, BantoError> {
    let enabled: i64 =
        sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    Ok(enabled != 0)
}

/// `write_control_state` の永続値を更新する (誰が・いつ変更したかも記録)。
/// 通常は [`WriteControl::set_enabled`] 経由で呼ぶ (状態ファイルと対で保存
/// するため)。
pub async fn persist_enabled(
    pool: &SqlitePool,
    enabled: bool,
    actor: Option<&str>,
) -> Result<(), BantoError> {
    sqlx::query(
        "UPDATE write_control_state \
         SET enabled_persisted = ?, last_changed_at = datetime('now'), last_changed_by = ? \
         WHERE id = 1",
    )
    .bind(enabled as i64)
    .bind(actor)
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_db, migrate_memory};
    use std::sync::Arc;

    // --- 起動時の判定の総当たり (#433) ---------------------------------------

    /// DB: 有効/停止/読めない × ファイル: 無い/有効/停止/再開の途中/壊れている
    /// の 15 通り。「どちらか一方でも停止（または読めない・再開の途中）なら
    /// 停止」。ファイルが無いときだけ DB に従う。食い違い・読めない・再開の
    /// 途中のときは注意書きが付く。
    #[test]
    fn startup_decision_table() {
        use DbStartupState as D;
        use FileStartupState as F;
        let db_unreadable = D::Unreadable("db down".to_string());
        let file_corrupt = F::Corrupt("broken".to_string());
        // (DB, ファイル, 有効で起動するか, 注意書きが付くか)
        let table: Vec<(D, F, bool, bool)> = vec![
            (D::Enabled, F::Absent, true, false),
            (D::Enabled, F::Enabled, true, false),
            (D::Enabled, F::Disabled, false, true),
            (D::Enabled, F::ResumePending, false, true),
            (D::Enabled, file_corrupt.clone(), false, true),
            (D::Disabled, F::Absent, false, false),
            (D::Disabled, F::Enabled, false, true),
            (D::Disabled, F::Disabled, false, false),
            (D::Disabled, F::ResumePending, false, true),
            (D::Disabled, file_corrupt.clone(), false, true),
            (db_unreadable.clone(), F::Absent, false, true),
            (db_unreadable.clone(), F::Enabled, false, true),
            (db_unreadable.clone(), F::Disabled, false, true),
            (db_unreadable.clone(), F::ResumePending, false, true),
            (db_unreadable.clone(), file_corrupt.clone(), false, true),
        ];
        for (db, file, enabled, warns) in table {
            let decision = decide_startup(&db, &file);
            assert_eq!(
                decision.enabled, enabled,
                "enabled for DB={db:?} file={file:?}"
            );
            assert_eq!(
                decision.warning.is_some(),
                warns,
                "warning for DB={db:?} file={file:?}: {:?}",
                decision.warning
            );
        }
    }

    #[test]
    fn startup_warning_names_both_problems() {
        let decision = decide_startup(
            &DbStartupState::Unreadable("db down".to_string()),
            &FileStartupState::Corrupt("broken".to_string()),
        );
        let warning = decision.warning.unwrap();
        assert!(warning.contains("db down"), "{warning}");
        assert!(warning.contains("broken"), "{warning}");
    }

    // --- 状態ファイル ---------------------------------------------------------

    #[tokio::test]
    async fn state_file_round_trips_and_reads_absent_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file_path(dir.path());
        assert_eq!(read_state_file(&path).await, FileStartupState::Absent);

        write_state_file(&path, false, Some("admin")).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Disabled);
        write_state_file(&path, true, None).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Enabled);

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(
            !PathBuf::from(tmp).exists(),
            "no temporary file is left behind"
        );

        for broken in [
            &b""[..],
            b"{not json",
            br#"{"format":2,"writeEnabled":true}"#,
            br#"{"format":1}"#,
            br#"{"format":1,"writeEnabled":"yes"}"#,
            br#"{"format":1,"writeEnabled":true,"resumePending":true}"#,
            br#"{"format":1,"writeEnabled":false,"resumePending":"yes"}"#,
        ] {
            std::fs::write(&path, broken).unwrap();
            assert!(
                matches!(read_state_file(&path).await, FileStartupState::Corrupt(_)),
                "{:?}",
                String::from_utf8_lossy(broken)
            );
        }
    }

    /// 途中で落ちて一時ファイルだけが残った状態は、状態ファイルの内容に
    /// 影響しない (読むのは本体だけ。次の書き込みで一時ファイルを上書きする)。
    #[tokio::test]
    async fn a_leftover_temporary_file_is_ignored_and_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file_path(dir.path());
        write_state_file(&path, false, None).await.unwrap();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, b"{\"format\":1,\"writeEnab").unwrap();

        assert_eq!(read_state_file(&path).await, FileStartupState::Disabled);
        write_state_file(&path, true, None).await.unwrap();
        assert_eq!(read_state_file(&path).await, FileStartupState::Enabled);
        assert!(!tmp.exists());
    }

    /// 「再開の途中」の形式と互換 (#439 レビュー P1-1): `resumePending` の無い
    /// (足す前の) ファイルは従来どおり読め、「再開の途中」は `writeEnabled:
    /// false` と組で書かれる (項目を知らない読み手にも停止に見える)。
    #[tokio::test]
    async fn resume_pending_record_is_a_stop_for_old_and_new_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_file_path(dir.path());

        for (old_format, expected) in [
            (
                &br#"{"format":1,"writeEnabled":true,"changedAtUnixMs":1,"changedBy":null}"#[..],
                FileStartupState::Enabled,
            ),
            (
                br#"{"format":1,"writeEnabled":false,"changedAtUnixMs":1,"changedBy":"admin"}"#,
                FileStartupState::Disabled,
            ),
            (
                br#"{"format":1,"writeEnabled":false,"resumePending":false}"#,
                FileStartupState::Disabled,
            ),
        ] {
            std::fs::write(&path, old_format).unwrap();
            assert_eq!(read_state_file(&path).await, expected);
        }

        write_state_record(&path, FileRecord::ResumePending, Some("admin"))
            .await
            .unwrap();
        assert_eq!(
            read_state_file(&path).await,
            FileStartupState::ResumePending
        );
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["format"], json!(1), "the format number is unchanged");
        assert_eq!(
            raw["writeEnabled"],
            json!(false),
            "a reader that ignores resumePending still sees a stop"
        );
        assert_eq!(raw["resumePending"], json!(true));

        write_state_file(&path, true, None).await.unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(raw.get("resumePending").is_none());
    }

    /// 状態ファイルの場所がディレクトリになっている (書けない・読めない)。
    fn unwritable_state_file(dir: &Path) -> PathBuf {
        let path = state_file_path(dir);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// 状態ファイルの一時ファイルの場所をディレクトリにして、状態ファイルへの
    /// 書き込みだけを失敗させる (状態ファイル自体はそのまま読める)。
    fn block_state_file_writes(state_file: &Path) {
        let mut tmp = state_file.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::create_dir_all(PathBuf::from(tmp)).unwrap();
    }

    // --- 停止・再開 -----------------------------------------------------------

    fn db_ok() -> std::future::Ready<Result<(), String>> {
        std::future::ready(Ok(()))
    }

    fn db_down() -> std::future::Ready<Result<(), String>> {
        std::future::ready(Err("database is down".to_string()))
    }

    #[test]
    fn constructs_with_live_flag_from_the_decision() {
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        assert!(control.is_enabled());
        assert!(control.was_enabled_before_restart());
        assert_eq!(control.persistence_warning(), None);

        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: Some("mismatch".to_string()),
            },
            state_file_path(dir.path()),
        );
        assert!(!control.is_enabled());
        assert!(!control.was_enabled_before_restart());
        assert_eq!(control.persistence_warning().as_deref(), Some("mismatch"));
    }

    #[test]
    fn enable_disable_round_trips() {
        let control = WriteControl::new(false);
        assert!(!control.is_enabled());
        control.enable();
        assert!(control.is_enabled());
        control.disable();
        assert!(!control.is_enabled());
    }

    #[tokio::test]
    async fn persisted_state_seeds_enabled_and_round_trips_through_persist() {
        let pool = migrate_memory().await.expect("migrate_memory");
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "write_control_state should seed enabled_persisted=1 (既定は書き込み可, #340)"
        );

        persist_enabled(&pool, false, Some("admin")).await.unwrap();
        assert!(!load_persisted_enabled(&pool).await.unwrap());

        let by: Option<String> =
            sqlx::query_scalar("SELECT last_changed_by FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("admin"));
    }

    /// 通常の停止・再開の往復: 両方に保存され、再起動 (新しい判定と構築) を
    /// 跨いで保持される。注意書きは出ない。
    #[tokio::test]
    async fn normal_stop_and_resume_round_trip_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: true,
                warning: None
            },
            "fresh install (seed = enabled, no file) starts enabled"
        );
        let control = WriteControl::restore(decision, state_file.clone());

        let change = control.set_enabled(&pool, false, Some("admin")).await;
        assert!(change.succeeded() && change.fully_persisted(), "{change:?}");
        assert_eq!(change.warning(), None);
        assert!(!control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: false,
                warning: None
            }
        );
        let control = WriteControl::restore(decision, state_file.clone());
        let change = control.set_enabled(&pool, true, Some("admin")).await;
        assert!(change.succeeded() && change.fully_persisted(), "{change:?}");
        assert!(control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert_eq!(
            decision,
            StartupDecision {
                enabled: true,
                warning: None
            }
        );
    }

    /// #433 の本題: 停止の保存が DB だけ失敗 → 再起動 → 停止のまま起動する
    /// (DB は「有効」のまま、状態ファイルが「停止」を覚えている)。
    #[tokio::test]
    async fn a_stop_that_only_reached_the_file_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        let control = WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        );
        assert!(control.is_enabled());

        let change = control
            .set_enabled_with(false, Some("admin"), db_down())
            .await;
        assert!(change.succeeded(), "a stop saved to the file is a stop");
        assert!(!change.fully_persisted());
        assert!(change.warning().unwrap().contains("database is down"));
        assert!(!control.is_enabled());
        assert!(control.persistence_warning().is_some());
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "precondition: the DB still says enabled"
        );

        // 再開を試みるが、DB はまだ落ちている (#439 レビュー P1-1)。
        let resumed = control.set_enabled_with(true, None, db_down()).await;
        assert!(!resumed.succeeded(), "{resumed:?}");
        assert!(!control.is_enabled());
        assert!(
            load_persisted_enabled(&pool).await.unwrap(),
            "precondition: the DB recovers still saying enabled"
        );
        assert!(
            !load_startup_decision(&pool, &state_file).await.enabled,
            "a failed resume must not erase the only record of the stop"
        );

        // 再起動。
        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled, "the stop must survive the restart");
        assert!(decision.warning.is_some(), "the mismatch is reported");
        let control = WriteControl::restore(decision, state_file);
        assert!(!control.is_enabled());
        assert!(control.persistence_warning().is_some());
    }

    /// 逆のケース (#439 レビュー P1-1): 停止が DB にだけ残り (状態ファイルの
    /// 保存に失敗し、ファイルは前の「有効」のまま)、そこからの再開がまた
    /// 状態ファイルの保存に失敗する。再開は失敗し、再起動しても停止のまま。
    #[tokio::test]
    async fn a_stop_that_only_reached_the_db_survives_a_failed_resume_and_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        write_state_file(&state_file, true, None).await.unwrap();
        let control = WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        );
        assert!(control.is_enabled());

        block_state_file_writes(&state_file);
        let stopped = control.set_enabled(&pool, false, Some("admin")).await;
        assert!(
            stopped.succeeded() && stopped.file_error().is_some(),
            "{stopped:?}"
        );
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Enabled,
            "precondition: the file still says enabled; the DB holds the only stop"
        );

        let resumed = control.set_enabled(&pool, true, Some("admin")).await;
        assert!(!resumed.succeeded(), "{resumed:?}");
        assert!(resumed.file_error().is_some());
        assert!(!control.is_enabled());
        assert!(
            !load_persisted_enabled(&pool).await.unwrap(),
            "the DB (the only stop record) must not be switched to enabled"
        );
        assert!(!load_startup_decision(&pool, &state_file).await.enabled);
    }

    /// 再開の各段の失敗・落ちる時点の総当たり (#439 レビュー P1-1)。停止を
    /// 記録した状態 (DB と状態ファイルの組み合わせ) から再開して、失敗として
    /// 返った・確定点より前で落ちた、どの場合も再起動は停止で起動する。
    /// 対照として、全部成功した再開は有効で起動する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_or_crashed_resume_always_restarts_stopped() {
        #[derive(Debug, Clone, Copy)]
        enum Initial {
            Absent,
            Enabled,
            Disabled,
            ResumePending,
            Corrupt,
        }
        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Fault {
            /// 1. 「再開の途中」を状態ファイルに書けない。
            PendingWriteFails,
            /// 2. DB の保存が失敗する。
            DbFails,
            /// 3. DB は保存できたが、状態ファイルを「有効」にできない。
            FinalWriteFails,
            /// 2 の途中で落ちる (DB はまだ書いていない)。
            CrashBeforeDbCommit,
            /// 2 の途中で落ちる (DB は書き終えたが、応答が返る前)。
            CrashAfterDbCommit,
            /// 対照: 全部成功。
            None,
        }
        // 停止を記録した状態 (DB が有効か, 状態ファイル)。どれも起動時は停止。
        let initials = [
            (true, Initial::Disabled), // 停止がファイルにだけ残った (レビューの例)
            (false, Initial::Enabled), // 停止が DB にだけ残った (逆のケース)
            (false, Initial::Disabled),
            (false, Initial::Absent),       // #433 より前からの環境
            (true, Initial::ResumePending), // 前の再開が途中で終わった
            (false, Initial::ResumePending),
            (true, Initial::Corrupt),
        ];
        let faults = [
            Fault::PendingWriteFails,
            Fault::DbFails,
            Fault::FinalWriteFails,
            Fault::CrashBeforeDbCommit,
            Fault::CrashAfterDbCommit,
            Fault::None,
        ];
        for (db_enabled, initial) in initials {
            for fault in faults {
                let case = format!("DB enabled={db_enabled} file={initial:?} fault={fault:?}");
                let dir = tempfile::tempdir().unwrap();
                let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
                let state_file = state_file_path(&dir.path().join("data"));
                persist_enabled(&pool, db_enabled, None).await.unwrap();
                std::fs::create_dir_all(state_file.parent().unwrap()).unwrap();
                match initial {
                    Initial::Absent => {}
                    Initial::Enabled => write_state_file(&state_file, true, None).await.unwrap(),
                    Initial::Disabled => write_state_file(&state_file, false, None).await.unwrap(),
                    Initial::ResumePending => {
                        write_state_record(&state_file, FileRecord::ResumePending, None)
                            .await
                            .unwrap()
                    }
                    Initial::Corrupt => std::fs::write(&state_file, b"{broken").unwrap(),
                }
                let decision = load_startup_decision(&pool, &state_file).await;
                assert!(!decision.enabled, "precondition (stopped): {case}");
                let control = WriteControl::restore(decision, state_file.clone());

                if fault == Fault::PendingWriteFails {
                    block_state_file_writes(&state_file);
                }
                let crashed = Arc::new(tokio::sync::Notify::new());
                let db_future = {
                    let pool = pool.clone();
                    let state_file = state_file.clone();
                    let crashed = crashed.clone();
                    async move {
                        match fault {
                            Fault::DbFails => return Err("database is down".to_string()),
                            Fault::CrashBeforeDbCommit => {
                                crashed.notify_one();
                                std::future::pending::<()>().await;
                            }
                            Fault::CrashAfterDbCommit => {
                                persist_enabled(&pool, true, None).await.unwrap();
                                crashed.notify_one();
                                std::future::pending::<()>().await;
                            }
                            Fault::FinalWriteFails => {
                                persist_enabled(&pool, true, None).await.unwrap();
                                block_state_file_writes(&state_file);
                            }
                            Fault::PendingWriteFails | Fault::None => {
                                persist_enabled(&pool, true, None).await.unwrap();
                            }
                        }
                        Ok(())
                    }
                };
                let resume = control.set_enabled_with(true, None, db_future);
                let outcome = tokio::select! {
                    change = resume => Some(change),
                    // 「落ちる」: 再開の future をそこで捨てる (以後は何も書かない)。
                    _ = crashed.notified() => None,
                };
                let restart = load_startup_decision(&pool, &state_file).await;
                match (fault, outcome) {
                    (Fault::None, Some(change)) => {
                        assert!(change.succeeded(), "{case}: {change:?}");
                        assert!(control.is_enabled(), "{case}");
                        assert!(
                            restart.enabled,
                            "{case}: a completed resume restarts enabled"
                        );
                        assert_eq!(restart.warning, None, "{case}");
                    }
                    (Fault::CrashBeforeDbCommit | Fault::CrashAfterDbCommit, None) => {
                        assert!(!control.is_enabled(), "{case}");
                        assert!(
                            !restart.enabled,
                            "{case}: a crashed resume restarts stopped"
                        );
                    }
                    (_, Some(change)) => {
                        assert!(!change.succeeded(), "{case}: {change:?}");
                        assert!(!control.is_enabled(), "{case}");
                        assert!(!restart.enabled, "{case}: a failed resume restarts stopped");
                        assert!(restart.warning.is_some() || !db_enabled, "{case}");
                    }
                    (_, None) => panic!("{case}: unexpected crash"),
                }
            }
        }
    }

    /// 停止の保存がファイルだけ失敗 → DB が停止を覚えているので停止のまま。
    #[tokio::test]
    async fn a_stop_that_only_reached_the_db_is_still_a_stop() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = unwritable_state_file(dir.path());
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        );

        let change = control.set_enabled(&pool, false, Some("admin")).await;
        assert!(change.succeeded());
        assert!(change.file_error().is_some());
        assert!(!control.is_enabled());

        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled);
    }

    /// 両方に保存できなかった停止は失敗 (ライブフラグは止まったまま)。
    #[tokio::test]
    async fn a_stop_saved_nowhere_fails_but_writes_stay_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            unwritable_state_file(dir.path()),
        );
        let change = control.set_enabled_with(false, None, db_down()).await;
        assert!(!change.succeeded());
        assert!(!control.is_enabled());
        assert!(change
            .warning()
            .unwrap()
            .contains("どこにも保存できませんでした"));
    }

    /// 再開は、DB かファイルのどちらかに書けないと停止のまま。
    #[tokio::test]
    async fn resume_needs_both_saves() {
        // DB が失敗。
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        );
        let change = control.set_enabled_with(true, None, db_down()).await;
        assert!(!change.succeeded());
        assert!(!control.is_enabled(), "DB failure keeps writes stopped");
        assert!(control.persistence_warning().is_some());

        // ファイルが失敗。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            unwritable_state_file(dir.path()),
        );
        let change = control.set_enabled_with(true, None, db_ok()).await;
        assert!(!change.succeeded());
        assert!(change.file_error().is_some());
        assert!(!control.is_enabled(), "file failure keeps writes stopped");

        // 両方成功で、注意書きも消える。
        let control = WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: Some("startup mismatch".to_string()),
            },
            state_file,
        );
        let change = control.set_enabled_with(true, None, db_ok()).await;
        assert!(change.succeeded());
        assert!(control.is_enabled());
        assert_eq!(control.persistence_warning(), None);
    }

    /// 再開の保存 (DB) が詰まっているあいだに停止が来たら: 停止はすぐに
    /// 効き、再開は DB の完了を待たずに割り込まれて返り (ライブフラグを
    /// 立てない)、停止の DB 書き込みは再開の書き込みの後に行われ、最後に
    /// 残る永続値も停止になる。
    /// (有効な状態から、再開が重ねて要求されて詰まっている場面で確かめる -
    /// ライブフラグが「すぐに落ちる」ことを観測できるように。)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_during_a_stuck_resume_wins() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let resume = {
            let control = control.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        entered.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                    .await
            })
        };
        entered.notified().await;

        let stop = {
            let control = control.clone();
            tokio::spawn(async move { control.set_enabled_with(false, None, db_ok()).await })
        };
        // 停止は再開の保存を待たずにライブフラグを落とす。
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while control.is_enabled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        // 再開は自分の DB 保存の完了を待たずに、割り込まれて返る。
        let resumed = tokio::time::timeout(Duration::from_secs(2), resume)
            .await
            .expect("the resume must stop waiting for its DB once a stop arrives")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(!resumed.succeeded());
        assert_eq!(resumed.db_error(), Some(DB_ABANDONED_ON_STOP));
        assert_eq!(resumed.file_error(), Some(KEPT_RESUME_PENDING_ON_STOP));
        // 停止の DB 書き込みは、見捨てた再開の書き込みの完了を待つ。
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !stop.is_finished(),
            "the stop's DB write is ordered after the abandoned resume write"
        );

        release.notify_one();
        let stopped = stop.await.unwrap();
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert!(!stopped.db_timed_out);
        assert!(!control.is_enabled(), "the stop wins");
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled,
            "the last saved value is the stop"
        );
    }

    // --- 停止の DB 保存の制限時間 (#433 監査) ---------------------------------

    type Order = Arc<SyncMutex<Vec<&'static str>>>;

    /// DB への保存を `release` まで返さず、返るときに `label` を `order` に積む。
    async fn db_held_until(
        release: Arc<tokio::sync::Notify>,
        order: Order,
        label: &'static str,
        result: Result<(), String>,
    ) -> Result<(), String> {
        release.notified().await;
        order.lock().unwrap().push(label);
        result
    }

    fn no_follow_up(_: WriteControlChange) -> std::future::Ready<()> {
        std::future::ready(())
    }

    /// 呼ばれた回数と結果を覚える `follow_up`。
    type FollowUps = Arc<SyncMutex<Vec<WriteControlChange>>>;

    fn counting_follow_up(
        seen: &FollowUps,
    ) -> impl FnOnce(WriteControlChange) -> std::future::Ready<()> + Send + 'static {
        let seen = seen.clone();
        move |change| {
            seen.lock().unwrap().push(change);
            std::future::ready(())
        }
    }

    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
    }

    /// 固まる DB (返らない保存): 停止の応答は制限時間で返り、書き込みは
    /// 止まり、注意書きは「タイムアウト」と分かる形で、再起動で停止のまま
    /// 起動する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_with_a_hung_db_returns_within_the_limit_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        let control = WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        );
        assert!(control.is_enabled());

        let started = std::time::Instant::now();
        let change = tokio::time::timeout(
            STOP_DB_SAVE_TIMEOUT * 3,
            control.set_enabled_with(
                false,
                Some("admin"),
                std::future::pending::<Result<(), String>>(),
            ),
        )
        .await
        .expect("the stop must return within its DB save limit");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= STOP_DB_SAVE_TIMEOUT && elapsed < STOP_DB_SAVE_TIMEOUT * 2,
            "{elapsed:?}"
        );
        assert!(change.db_timed_out, "{change:?}");
        assert!(change.succeeded(), "saved to the file, so the stop holds");
        assert!(!control.is_enabled());
        assert!(change.warning().unwrap().contains("タイムアウト"));
        assert!(control
            .persistence_warning()
            .unwrap()
            .contains("タイムアウト"));

        // 再起動 (DB はまだ「有効」、状態ファイルが「停止」)。
        let decision = load_startup_decision(&pool, &state_file).await;
        assert!(!decision.enabled, "the stop survives the restart");
        assert!(decision.warning.is_some());
    }

    /// 打ち切った停止の DB 書き込みが遅れて完了する場合: 次の再開はその完了を
    /// 待ってから保存する (「有効」を遅れた「停止」で上書きされない)。遅れて
    /// 完了したら注意書きも更新される。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_next_resume_waits_for_a_lagging_stop_db_write() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        let stopped = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop", Ok(())),
            )
            .await;
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");

        let resume = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push("resume");
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            order.lock().unwrap().is_empty(),
            "the resume must not save before the lagging stop finishes"
        );
        assert!(!resume.is_finished());
        assert!(!control.is_enabled());

        release.notify_one();
        let resumed = resume.await.unwrap();
        assert!(resumed.succeeded(), "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop", "resume"]);
        assert!(control.is_enabled());
        assert_eq!(control.persistence_warning(), None);
    }

    /// 再開が遅れている停止の DB 書き込みを待っている間に、別の停止が来た
    /// (#439 レビュー P1-2)。再開は待つ前の世代と比べて割り込みを検出し、
    /// 何も保存せずにやめる。両方の操作が終わった後、書き込みは止まっている。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_while_the_resume_waits_for_a_lagging_stop_db_write_wins() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        // 停止 A: DB 保存が制限時間を超えて遅れる。
        let stopped = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop A", Ok(())),
            )
            .await;
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");

        // 再開 B: A の DB 書き込みの完了を待つ。
        let resume = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push("resume B");
                        Ok(())
                    })
                    .await
            })
        };
        wait_until("resume B takes over the lagging stop write", || {
            control.lagging_stop_db.lock().unwrap().is_empty()
        })
        .await;
        assert!(!resume.is_finished());

        // 停止 C: B が待っている間に来る。
        let stop = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, async move {
                        order.lock().unwrap().push("stop C");
                        Ok(())
                    })
                    .await
            })
        };
        // A の DB 書き込みは握ったまま: B は C が来た時点で待つのをやめ
        // (A の完了を待たない)、C はその場で保存して返る。
        let resumed = tokio::time::timeout(Duration::from_secs(5), resume)
            .await
            .expect("the interrupted resume returns without waiting for stop A")
            .unwrap();
        let stopped = tokio::time::timeout(Duration::from_secs(5), stop)
            .await
            .expect("stop C returns without waiting for stop A")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(!resumed.succeeded());
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert!(!control.is_enabled(), "the later stop wins");
        assert_eq!(
            *order.lock().unwrap(),
            vec!["stop C"],
            "the interrupted resume saves nothing; stop A is still held"
        );

        // 停止どうしの DB 書き込みの順番は問わない (どちらも「停止」。遅れた
        // 停止が後の停止の後に届くのは従来からある。
        // `a_late_stop_db_result_updates_only_its_own_warning` 参照)。
        release.notify_one();
        wait_until("the lagging stop A lands", || {
            order.lock().unwrap().len() == 2
        })
        .await;
        assert_eq!(*order.lock().unwrap(), vec!["stop C", "stop A"]);
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 再開が `op_lock` そのものを待っている間に停止が来た (#439 レビュー
    /// P1-2)。再開は割り込みを検出し、両方の操作が終わった後、書き込みは
    /// 止まっている。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_while_the_resume_waits_for_the_op_lock_wins() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        // 停止 A: DB 保存中 (制限時間内) で `op_lock` を持ったまま。
        let stop_a = {
            let control = control.clone();
            let release = release.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, db_held_until(release, order, "stop A", Ok(())))
                    .await
            })
        };
        wait_until("stop A takes effect", || !control.is_enabled()).await;

        // 再開 B: `op_lock` を待つ。
        let resume = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push("resume B");
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 停止 C: B が `op_lock` を待っている間に来る (B の後ろに並ぶ)。
        let before_c = control.stop_state.lock().unwrap().generation;
        let stop_c = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, async move {
                        order.lock().unwrap().push("stop C");
                        Ok(())
                    })
                    .await
            })
        };
        wait_until("stop C has started", || {
            control.stop_state.lock().unwrap().generation != before_c
        })
        .await;

        release.notify_one();
        assert!(stop_a.await.unwrap().succeeded());
        let resumed = resume.await.unwrap();
        let stopped = stop_c.await.unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert!(!control.is_enabled(), "the later stop wins");
        assert_eq!(*order.lock().unwrap(), vec!["stop A", "stop C"]);
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 停止がライブフラグを落としてから `op_lock` を取るまでの間に再開が来て、
    /// 停止より先に `op_lock` を取った (#439 レビュー P1-2 の残り、2026-10-10
    /// 監査 P2-1)。この再開は停止の後の世代を覚えて始まるので世代では割り込みが
    /// 見えないが、並んでいる停止があるので保存せずに割り込まれて返る (409)。
    /// 後で保存する停止が永続値もライブフラグも停止にする。停止の前半
    /// (`begin_stop`) と後半 (`save_stop`) を分けて、この順序を作る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_that_overtakes_a_queued_stop_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));
        let held = control.op_lock.lock().await;

        // 停止 C の前半: ライブフラグを落とす (まだ `op_lock` を取っていない)。
        let queued = control.begin_stop();
        // 再開 B: C の後の世代を覚えて、先に `op_lock` に並ぶ。
        let resume = {
            let control = control.clone();
            tokio::spawn(async move { control.set_enabled_with(true, None, db_ok()).await })
        };
        // B が `op_lock` に並ぶのを待つ (並んでいなくても、B は C の保存の前に
        // 取るか後に取るかで、どちらでも割り込まれる)。
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(held);
        let resumed = tokio::time::timeout(Duration::from_secs(5), resume)
            .await
            .expect("the resume returns without waiting for the queued stop")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(!resumed.succeeded());
        assert!(!control.is_enabled());
        assert_ne!(
            read_state_file(&state_file).await,
            FileStartupState::ResumePending,
            "the refused resume writes nothing"
        );

        // 停止 C の後半。
        let stopped = control.save_stop(queued, None, db_ok()).await;
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );

        // 停止が `op_lock` を取った後の再開は、通常どおり成功する。
        let resumed = control.set_enabled_with(true, None, db_ok()).await;
        assert!(resumed.succeeded(), "{resumed:?}");
        assert!(control.is_enabled());
    }

    /// 2026-10-10 監査 P2-1: 停止が世代を進めた後、`op_lock` を取る前に再開が
    /// 始まって先に `op_lock` を取り、その再開の DB 保存が返らない。再開は
    /// 世代が変わらないので知らせも届かないが、並んでいる停止があるので DB に
    /// 触れる前に割り込まれて返り、停止は制限時間の内に (再開を待たずに) 返る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_started_while_a_stop_is_queued_does_not_hold_the_stop() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        write_state_file(&state_file, true, None).await.unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));

        // 停止の前半だけ (世代は進んだが、まだ `op_lock` を取っていない)。
        let queued = control.begin_stop();
        assert!(!control.is_enabled());

        // 再開: `op_lock` は空いているので、停止より先に取る。DB 保存は返らない。
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut resume = {
            let control = control.clone();
            let entered = entered.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        entered.notify_one();
                        std::future::pending::<Result<(), String>>().await
                    })
                    .await
            })
        };
        let resumed = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                resumed = &mut resume => resumed.unwrap(),
                () = entered.notified() => {
                    panic!("the resume reached its DB save while a stop was queued")
                }
            }
        })
        .await
        .expect("the resume must return while a stop is queued");
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert_eq!(resumed.db_error(), Some(NOT_SAVED_INTERRUPTED));
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Enabled,
            "the refused resume writes nothing (not even 'resume pending')"
        );

        // 停止の後半: 再開に塞がれずに、すぐ保存して返る。
        let stopped = tokio::time::timeout(
            STOP_DB_SAVE_TIMEOUT,
            control.save_stop(queued, Some("admin"), db_ok()),
        )
        .await
        .expect("the stop must not wait behind the resume");
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 並んでいる停止の future が `op_lock` を待つ間に捨てられても、並んでいる
    /// 数は戻る (以後の再開を拒み続けない)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_queued_stop_does_not_block_later_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        ));
        let held = control.op_lock.lock().await;
        let stop = {
            let control = control.clone();
            tokio::spawn(async move { control.set_enabled_with(false, None, db_ok()).await })
        };
        wait_until("the stop is queued", || {
            control.stop_state.lock().unwrap().queued == 1
        })
        .await;
        stop.abort();
        assert!(stop.await.unwrap_err().is_cancelled());
        assert_eq!(control.stop_state.lock().unwrap().queued, 0);
        drop(held);

        let resumed = control.set_enabled_with(true, None, db_ok()).await;
        assert!(resumed.succeeded(), "{resumed:?}");
        assert!(control.is_enabled());
    }

    /// 遅れて完了した停止の DB 書き込みは、注意書きを自分の結果で更新する
    /// (成功なら消える) が、その後に記録された新しい停止・再開の表示は巻き
    /// 戻さない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_stop_db_result_updates_only_its_own_warning() {
        // 遅れて成功 → 注意書きが消える。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let change = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop", Ok(())),
            )
            .await;
        assert!(change.db_timed_out);
        assert!(control.persistence_warning().is_some());
        release.notify_one();
        wait_until("the late success clears the warning", || {
            control.persistence_warning().is_none()
        })
        .await;

        // 遅れて失敗するが、その前に新しい停止が両方に保存された → 新しい
        // 表示 (注意なし) のまま。
        let dir = tempfile::tempdir().unwrap();
        let control = WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        );
        let release = Arc::new(tokio::sync::Notify::new());
        let first = control
            .set_enabled_with(
                false,
                None,
                db_held_until(
                    release.clone(),
                    order.clone(),
                    "late failure",
                    Err("late failure".to_string()),
                ),
            )
            .await;
        assert!(first.db_timed_out);
        let second = control.set_enabled_with(false, None, db_ok()).await;
        assert!(second.fully_persisted());
        assert_eq!(control.persistence_warning(), None);
        release.notify_one();
        wait_until("the late failure has been observed", || {
            order.lock().unwrap().contains(&"late failure")
        })
        .await;
        let lagging: Vec<DbWrite> = std::mem::take(&mut *control.lagging_stop_db.lock().unwrap());
        for handle in lagging {
            // 見届け役は遅れた結果を返す (ここでは「失敗」)。完了だけを待つ。
            let _ = handle.await.unwrap();
        }
        assert_eq!(
            control.persistence_warning(),
            None,
            "an older late result must not overwrite a newer outcome"
        );
    }

    // --- 停止は、固まった再開の後ろで待たない (2026-10-10) -------------------

    /// 再開の DB 保存が返らない (DB が固まった) ときに停止が来た: 停止は
    /// 再開を待たずに、自分の DB 保存の制限時間 (再開の書き込みの完了待ちを
    /// 含む) で返る。書き込みは止まり、再開は割り込まれて返り、状態ファイルは
    /// 「停止」、再起動は停止で起動する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_does_not_wait_behind_a_resume_whose_db_hangs() {
        let dir = tempfile::tempdir().unwrap();
        let pool = init_db(dir.path().join("registry.sqlite3")).await.unwrap();
        let state_file = state_file_path(&dir.path().join("data"));
        persist_enabled(&pool, false, None).await.unwrap();
        write_state_file(&state_file, false, None).await.unwrap();
        let control = Arc::new(WriteControl::restore(
            load_startup_decision(&pool, &state_file).await,
            state_file.clone(),
        ));
        assert!(!control.is_enabled());

        let entered = Arc::new(tokio::sync::Notify::new());
        let resume = {
            let control = control.clone();
            let entered = entered.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, Some("admin"), async move {
                        entered.notify_one();
                        std::future::pending::<Result<(), String>>().await
                    })
                    .await
            })
        };
        entered.notified().await;
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::ResumePending,
            "precondition: the resume is inside its DB save"
        );

        let started = std::time::Instant::now();
        let stopped = tokio::time::timeout(
            STOP_DB_SAVE_TIMEOUT * 2,
            control.set_enabled_with(false, Some("admin"), db_ok()),
        )
        .await
        .expect("the stop must not wait behind the hung resume");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= STOP_DB_SAVE_TIMEOUT && elapsed < STOP_DB_SAVE_TIMEOUT * 2,
            "the stop's DB write waits for the hung resume write until its own limit: {elapsed:?}"
        );
        assert!(stopped.db_timed_out, "{stopped:?}");
        assert!(stopped.succeeded(), "saved to the file, so the stop holds");
        assert!(!control.is_enabled());

        let resumed = tokio::time::timeout(Duration::from_secs(1), resume)
            .await
            .expect("the interrupted resume has returned")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert!(!resumed.succeeded());
        assert!(!control.is_enabled());

        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
        let restart = load_startup_decision(&pool, &state_file).await;
        assert!(!restart.enabled, "the stop survives a restart");
    }

    /// 見捨てた再開の DB 書き込み (「有効」) は、割り込んだ停止の DB 書き込み
    /// (「停止」) より必ず先に届く。停止が制限時間で返った後に再開の書き込みが
    /// 完了しても、その後に停止の書き込みが届き、注意書きもそれで更新される。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_stop_db_write_lands_after_an_abandoned_resume_db_write() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let resume = {
            let control = control.clone();
            let entered = entered.clone();
            let held = db_held_until(release.clone(), order.clone(), "resume", Ok(()));
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        entered.notify_one();
                        held.await
                    })
                    .await
            })
        };
        entered.notified().await;

        let stop = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, async move {
                        order.lock().unwrap().push("stop");
                        Ok(())
                    })
                    .await
            })
        };
        let resumed = tokio::time::timeout(Duration::from_secs(2), resume)
            .await
            .expect("the resume stops waiting once the stop arrives")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");

        let stopped = stop.await.unwrap();
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");
        assert!(
            order.lock().unwrap().is_empty(),
            "the stop's DB write must not land before the abandoned resume write"
        );
        assert!(control
            .persistence_warning()
            .unwrap()
            .contains("タイムアウト"));
        assert!(!control.is_enabled());

        release.notify_one();
        wait_until("both DB writes land", || order.lock().unwrap().len() == 2).await;
        assert_eq!(*order.lock().unwrap(), vec!["resume", "stop"]);
        wait_until("the late stop result clears the warning", || {
            control.persistence_warning().is_none()
        })
        .await;
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 再開が遅れている停止の DB 書き込みを待っている間に停止が来た: 再開は
    /// その完了を待たずにすぐ返り、待っていた書き込みは失われない (次の再開が
    /// また待つ)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_interrupted_while_waiting_for_a_lagging_stop_keeps_it_for_the_next_resume() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        // 停止 A: DB 保存が制限時間を超えて遅れる。
        let stopped = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop A", Ok(())),
            )
            .await;
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");

        let resume_pushing = |label: &'static str| {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push(label);
                        Ok(())
                    })
                    .await
            })
        };

        // 再開 B: A の DB 書き込みの完了を待つ。
        let resume_b = resume_pushing("resume B");
        wait_until("resume B takes over the lagging stop write", || {
            control.lagging_stop_db.lock().unwrap().is_empty()
        })
        .await;
        assert!(!resume_b.is_finished());

        // 停止 C: B は A を待たずにすぐやめ、C はその場で保存する。
        let order_c = order.clone();
        let stopped = tokio::time::timeout(
            Duration::from_secs(2),
            control.set_enabled_with(false, None, async move {
                order_c.lock().unwrap().push("stop C");
                Ok(())
            }),
        )
        .await
        .expect("the stop must not wait for the lagging stop write behind the resume");
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        let resumed = tokio::time::timeout(Duration::from_secs(2), resume_b)
            .await
            .expect("the interrupted resume has returned")
            .unwrap();
        assert!(resumed.interrupted_by_stop, "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop C"]);

        // 再開 D: A の書き込みはまだ控えに残っているので、D はそれを待つ。
        let resume_d = resume_pushing("resume D");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !resume_d.is_finished(),
            "the lagging stop write must not be lost by the interrupted resume"
        );
        assert_eq!(*order.lock().unwrap(), vec!["stop C"]);

        release.notify_one();
        let resumed = resume_d.await.unwrap();
        assert!(resumed.succeeded(), "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop C", "stop A", "resume D"]);
        assert!(control.is_enabled());
    }

    // --- 呼び出し側が future を捨てても、書き込みを見失わない (2026-10-10) -----

    /// DB 保存の途中で再開の future が捨てられた (クライアントの切断): その
    /// DB 書き込みは追跡に残り、次の停止の DB 書き込みはその完了の後に届く。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_aborted_resume_leaves_its_db_write_for_the_next_stop() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let resume = {
            let control = control.clone();
            let entered = entered.clone();
            let held = db_held_until(release.clone(), order.clone(), "resume", Ok(()));
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        entered.notify_one();
                        held.await
                    })
                    .await
            })
        };
        entered.notified().await;
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::ResumePending
        );
        resume.abort();
        assert!(resume.await.unwrap_err().is_cancelled());
        assert!(!control.is_enabled());
        assert_eq!(
            control.lagging_resume_db.lock().unwrap().len(),
            1,
            "the abandoned resume write is tracked"
        );

        let stop = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, async move {
                        order.lock().unwrap().push("stop");
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            order.lock().unwrap().is_empty(),
            "the stop's DB write must wait for the abandoned resume write"
        );
        assert!(!stop.is_finished());

        release.notify_one();
        let stopped = stop.await.unwrap();
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert_eq!(*order.lock().unwrap(), vec!["resume", "stop"]);
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 遅れている停止の DB 書き込みを待っている間に再開の future が捨てられた:
    /// 待っていた書き込みは控えに戻り、後の再開がまた待つ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resume_dropped_while_waiting_for_a_lagging_stop_keeps_it_for_the_next_resume() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file_path(dir.path()),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());

        // 停止 A: DB 保存が制限時間を超えて遅れる。
        let stopped = control
            .set_enabled_with(
                false,
                None,
                db_held_until(release.clone(), order.clone(), "stop A", Ok(())),
            )
            .await;
        assert!(stopped.db_timed_out && stopped.succeeded(), "{stopped:?}");

        let resume_pushing = |label: &'static str| {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push(label);
                        Ok(())
                    })
                    .await
            })
        };

        // 再開 B: A を待っている間に捨てられる。
        let resume_b = resume_pushing("resume B");
        wait_until("resume B takes over the lagging stop write", || {
            control.lagging_stop_db.lock().unwrap().is_empty()
        })
        .await;
        assert!(!resume_b.is_finished());
        resume_b.abort();
        assert!(resume_b.await.unwrap_err().is_cancelled());
        assert_eq!(
            control.lagging_stop_db.lock().unwrap().len(),
            1,
            "the lagging stop write is put back"
        );

        // 再開 D: A をまた待つ。
        let resume_d = resume_pushing("resume D");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !resume_d.is_finished(),
            "the lagging stop write must not be lost by the dropped resume"
        );
        assert!(order.lock().unwrap().is_empty());

        release.notify_one();
        let resumed = resume_d.await.unwrap();
        assert!(resumed.succeeded(), "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop A", "resume D"]);
        assert!(control.is_enabled());
    }

    /// 停止の future が DB 保存の制限時間を待つ間に捨てられた: 停止の DB
    /// 書き込みは `lagging_stop_db` に残り、次の再開はその完了を待つ。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_dropped_while_its_db_write_runs_keeps_it_for_the_next_resume() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let stop = {
            let control = control.clone();
            let entered = entered.clone();
            let held = db_held_until(release.clone(), order.clone(), "stop", Ok(()));
            tokio::spawn(async move {
                control
                    .set_enabled_with(false, None, async move {
                        entered.notify_one();
                        held.await
                    })
                    .await
            })
        };
        entered.notified().await;
        stop.abort();
        assert!(stop.await.unwrap_err().is_cancelled());
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
        assert_eq!(
            control.lagging_stop_db.lock().unwrap().len(),
            1,
            "the stop's DB write is tracked"
        );

        let resume = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_with(true, None, async move {
                        order.lock().unwrap().push("resume");
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!resume.is_finished());
        assert!(order.lock().unwrap().is_empty());

        release.notify_one();
        let resumed = resume.await.unwrap();
        assert!(resumed.succeeded(), "{resumed:?}");
        assert_eq!(*order.lock().unwrap(), vec!["stop", "resume"]);
        assert!(control.is_enabled());
    }

    // --- 本番の呼び出しは別タスクで最後まで走らせる (2026-10-10 監査 P2-2) ---

    /// 呼び出し側 (REST・MCP のハンドラ) の future が再開の DB 保存中に捨て
    /// られても、再開は途中で止まらずに最後まで走る (状態ファイルの「有効」が
    /// `op_lock` の外で遅れて置き換わることは無い)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_detached_resume_runs_to_completion_when_the_caller_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let follow_ups: FollowUps = Arc::default();
        let entered = Arc::new(tokio::sync::Notify::new());

        let caller = {
            let control = control.clone();
            let entered = entered.clone();
            let held = db_held_until(release.clone(), order.clone(), "resume", Ok(()));
            let follow_up = counting_follow_up(&follow_ups);
            tokio::spawn(async move {
                control
                    .set_enabled_detached_with(
                        true,
                        None,
                        async move {
                            entered.notify_one();
                            held.await
                        },
                        follow_up,
                    )
                    .await
            })
        };
        entered.notified().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(
            control.lagging_resume_db.lock().unwrap().is_empty(),
            "the resume itself was not abandoned"
        );

        release.notify_one();
        wait_until("the resume completes", || control.is_enabled()).await;
        // ライブフラグは状態ファイルを「有効」にした後に立つ。
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Enabled
        );
        assert_eq!(control.persistence_warning(), None);
        // 呼び出し側の後始末 (監査) も、捨てられた後で 1 回だけ走る。
        wait_until("the follow-up runs", || {
            !follow_ups.lock().unwrap().is_empty()
        })
        .await;
        let seen = follow_ups.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].requested_enabled && seen[0].succeeded(), "{seen:?}");
    }

    /// 監査 P2-2 の例: 再開の途中で呼び出し側が切断し、すぐに停止が来る。
    /// 最後に残る永続値 (状態ファイル・DB の書き込み順) は停止になる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_after_a_dropped_detached_resume_is_what_persists() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: false,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let caller = {
            let control = control.clone();
            let entered = entered.clone();
            let held = db_held_until(release.clone(), order.clone(), "resume", Ok(()));
            tokio::spawn(async move {
                control
                    .set_enabled_detached_with(
                        true,
                        None,
                        async move {
                            entered.notify_one();
                            held.await
                        },
                        no_follow_up,
                    )
                    .await
            })
        };
        entered.notified().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());

        let before_stop = control.stop_state.lock().unwrap().generation;
        let stop = {
            let control = control.clone();
            let order = order.clone();
            tokio::spawn(async move {
                control
                    .set_enabled_detached_with(
                        false,
                        None,
                        async move {
                            order.lock().unwrap().push("stop");
                            Ok(())
                        },
                        no_follow_up,
                    )
                    .await
            })
        };
        wait_until("the stop has started", || {
            control.stop_state.lock().unwrap().generation != before_stop
        })
        .await;
        release.notify_one();
        let stopped = stop.await.unwrap();
        assert!(
            stopped.succeeded() && stopped.fully_persisted(),
            "{stopped:?}"
        );
        assert_eq!(*order.lock().unwrap(), vec!["resume", "stop"]);
        assert!(!control.is_enabled());
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
    }

    /// 停止の呼び出し側が `op_lock` を待つ間に捨てられても、停止は最後まで
    /// 走って保存する (以前は、ライブフラグが落ちるだけで何も保存しなかった)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_detached_stop_saves_even_if_the_caller_is_dropped_while_queued() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = state_file_path(dir.path());
        write_state_file(&state_file, true, None).await.unwrap();
        let control = Arc::new(WriteControl::restore(
            StartupDecision {
                enabled: true,
                warning: None,
            },
            state_file.clone(),
        ));
        let order: Order = Arc::default();
        let follow_ups: FollowUps = Arc::default();
        let held = control.op_lock.lock().await;

        let caller = {
            let control = control.clone();
            let order = order.clone();
            let follow_up = counting_follow_up(&follow_ups);
            tokio::spawn(async move {
                control
                    .set_enabled_detached_with(
                        false,
                        None,
                        async move {
                            order.lock().unwrap().push("stop");
                            Ok(())
                        },
                        follow_up,
                    )
                    .await
            })
        };
        wait_until("the stop is queued", || {
            control.stop_state.lock().unwrap().queued == 1
        })
        .await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(!control.is_enabled());
        drop(held);

        wait_until("the stop saves to the DB", || {
            order.lock().unwrap().as_slice() == ["stop"]
        })
        .await;
        assert_eq!(
            read_state_file(&state_file).await,
            FileStartupState::Disabled
        );
        assert_eq!(control.stop_state.lock().unwrap().queued, 0);
        wait_until("the follow-up runs", || {
            !follow_ups.lock().unwrap().is_empty()
        })
        .await;
        let seen = follow_ups.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(
            !seen[0].requested_enabled && seen[0].succeeded(),
            "{seen:?}"
        );
    }
}
