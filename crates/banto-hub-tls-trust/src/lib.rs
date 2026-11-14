//! `banto-hub-tls-trust`（#644 PR3a、2026-11-13 オーナー決定）: banto-hub の loopback TLS の
//! 置き場 `<ProgramData>\BantoHub\tls\` を、**書き込み・作成を一切せずに**確かめてから読む部品。
//!
//! banto-hub（平文 `http://127.0.0.1:8722`）が止まっている間に別のプロセスが 127.0.0.1 の
//! ポートを先に取ると、クライアントは秘密（API キー）をその相手へ送ってしまう（#644）。Hub の
//! サービスは既定で、`banto-hub-elev prepare-hub-tls` が作った [`KEY_FILE`]・[`CERT_FILE`] で
//! 127.0.0.1:[`DEFAULT_TLS_PORT`] の TLS を待ち受ける（banto-hub-core の
//! `hub_tls_prepare::load`）。クライアント（ChronoGazer など、#644 PR3b）は同じ [`CERT_FILE`] を
//! pin として読み、その証明書を出す相手にだけ秘密を送る。
//!
//! もとは banto-hub-core の `hub_tls_prepare`（`load.rs` の確かめの手順 1〜3 と、それに要る OS の
//! 操作・文言）にあったもので、クライアントも**同じ確かめ**をしてから証明書を信じる必要がある
//! ため、重複させずにここへ移した（オーナー決定: 確かめは共有 crate に切り出し、Hub と
//! ChronoGazer で同じコードを使う）。Hub の判定・順序・文言は移す前と変えない。
//!
//! ## 何を信じるか
//!
//! 一般の利用者は ProgramData に `BantoHub` を先に作って、偽の証明書を置ける。そこで、置き場の
//! 中身を信じるのは、置き場そのものが「SYSTEM・Administrators だけが書き換えられる」と確かめ
//! られたときだけにする（[`open_place`]、手順は下の「確かめの順序」）:
//!
//! 1. ProgramData は既知のフォルダ（`SHGetKnownFolderPath`）から取る（`BANTO_HUB_ROOT`・
//!    環境変数には追従しない）。ルート `<ProgramData>\BantoHub` が絶対パスで、既知のフォルダ
//!    そのものでないこと。
//! 2. ルートを辿らずに開き、本当のパス・reparse point でないフォルダ・所有者
//!    （SYSTEM・Administrators・TrustedInstaller）・DACL（[`root_dacl_problems`]）を確かめる。
//! 3. `tls\` をルートのハンドル基準で辿らずに開き、本当のパス・種類・所有者・
//!    `exposure_beyond_users_read`（Users の読み取りより広い許可・ほかの相手が無い）を確かめる。
//!
//! その後のファイルは `tls\` のハンドル基準で辿らずに読み取りで開き（[`open_checked_file`]。
//! 共有は読み取りだけで、握っている間は誰も書き換えられない）、フォルダ・reparse point・本当の
//! 場所の食い違いを断ってから、握ったハンドルから [`MAX_PEM_BYTES`] まで読む
//! （[`read_checked`]）。確かめた後にパスで開き直さない。
//!
//! クライアントの入口は [`load_pinned_hub_cert`]: 上の後、証明書だけを開いて
//! `exposure_beyond_users_read` を確かめ、PEM に証明書がちょうど 1 つで DER として解析できる
//! ことを確かめて、PEM のバイト列と本当のパスを返す（[`HubCertPin`]）。
//! クライアントの Windows の実装（`win::WindowsTlsTrustOps`）はフォルダを**読み取り専用**の pin
//! （`WRITE_DAC | WRITE_OWNER` を求めない）で開く。一般利用者（昇格なし）でも動くため。
//!
//! ## なぜ鍵を読まないか
//!
//! 秘密鍵（[`KEY_FILE`]）は SYSTEM・Administrators だけが読める（`prepare-hub-tls` が作るのと
//! 同時にそう付ける）。クライアントが要るのは pin の根になる証明書だけで、鍵を開こうとすると
//! 一般の利用者では必ず拒否される。鍵の確かめ（`exposure`）と、鍵と証明書の対の確かめは Hub
//! （サービス）の側に残す。
//!
//! ## 所有者に「実行した本人」を含めない
//!
//! `prepare-hub-tls`（昇格した管理者）は、ルートを継承で作ると作った本人が所有者になるので、
//! ルートの DACL の確かめで本人を信頼する（`root_dacl_problems(.., Some(本人))`）。読む側
//! （LocalSystem のサービス・一般利用者のクライアント）は本人を知らず、本人が一般の利用者なら
//! それこそが偽物を置いた相手なので、ここでは常に `None` で確かめる（所有者は SYSTEM・
//! Administrators・TrustedInstaller だけ）。
//!
//! ## 失敗の形
//!
//! 断ったときは [`Unavailable`]（表示用の日本語の理由と、対処の種類 [`Remedy`]）を返す。
//! 対処の文言は呼ぶ側が決める（Hub は「Hub のサービスを再起動」、クライアントは自分の言い方）。
//! 理由の文言は Hub のサービスログの文言そのもの（移す前と同じ）。
//!
//! ## 利用者ごとの置き場（#1121 PR1a、2026-11-14 オーナー決定）
//!
//! Hub のデスクトップ版は LocalSystem でも管理者でもないので、`%ProgramData%\BantoHub\tls\` を
//! 作れない。デスクトップ版は**利用者ごとの置き場**の鍵と証明書で 8723 を待ち受け、ChronoGazer
//! はサービスの置き場と利用者の置き場の両方を pin として読む（docs/hub-desktop-tls-design.md
//! §4.1・§4.4）。その置き場と確かめが `user` モジュール:
//!
//! - **差し替えの口は 1 つ**（[`UserTlsPlace`]）。4 つの置き場（[`UserPlaceKind`]: `tls\`・
//!   失効の記録 A・B・記録のロックファイル）それぞれについて、パス・期待する所有者・確かめの
//!   方針（[`UserPlaceSpec`]）を返す。パスを決め打ちするのは既定の実装
//!   [`DefaultUserTlsPlace`]（`%LOCALAPPDATA%\BantoHub\tls\`・`…\BantoHub\revocations\`・
//!   `%LOCALAPPDATA%\BantoHubRevocations\`・`%LOCALAPPDATA%\BantoHubRevocations.lock`）だけ。
//!   Store 版のフォーク（MSIX では `%LOCALAPPDATA%` がパッケージごとの場所へ振り向けられ、
//!   publisher cache folder は所有者・ACL が違いうる）はこの trait を差し替える。
//! - **`%LOCALAPPDATA%` は環境変数ではなく既知のフォルダ**（`FOLDERID_LocalAppData`）から取る
//!   （[`UserTlsOps::local_app_data`]）。理由は ProgramData と同じで、環境変数で別の場所へ
//!   向けられないようにするため。
//! - **信頼する相手は 3 者**: 今のプロセスのトークンの利用者（[`UserPlaceSpec::owners`]）・
//!   SYSTEM・Administrators。サービスの置き場と違い、利用者本人を信頼する（置き場は本人の
//!   もの。守る相手は**別のアカウント**）。
//! - **確かめ**（[`open_user_place`]）: 起点（`%LOCALAPPDATA%`）を辿らずに開き、その下は親の
//!   ハンドル基準で 1 要素ずつ開いて、起点から置き場までの**すべてのフォルダ**で、reparse
//!   point でないこと・本当のパスが期待と同じ場所であること・所有者が 3 者のどれかであること・
//!   3 者以外に書き込み・削除・`WRITE_DAC`・`WRITE_OWNER` の許可が無いこと
//!   （[`user_place_problems`]）を見る。起点は、既知のフォルダの値と一致することと、本当の
//!   パスがローカルの固定ドライブにあること（フォルダの転送を断る）も見る。`%LOCALAPPDATA%` の
//!   既定の ACL（利用者・SYSTEM・Administrators だけ）なら、何もしなくても通る。
//! - **AppContainer の SID への許可は問題にしない**（2026-11-14 オーナー決定。
//!   [`is_app_container_sid`]: パッケージ SID `S-1-15-2-…`・capability SID `S-1-15-3-…` への
//!   普通の許可。鍵でも同じ）。AppContainer のトークンのアクセスは、利用者・グループの SID の
//!   確かめとパッケージ・capability の SID の確かめの両方を通った分の積で、通常のトークンは
//!   これらの SID を持たない。この ACE だけでは別のアカウントに権限は付かず、付くのは同じ
//!   利用者（信頼する相手）の AppContainer のプロセスだけ。サンドボックスの道具が
//!   `%LOCALAPPDATA%` に capability SID のフルを足した PC でも、既定の置き場が通る。サービスの
//!   置き場（`%ProgramData%`）の確かめは変えない。
//! - **「無い」と「あるが通らない」を分ける**: デスクトップ版を動かしていない利用者では置き場が
//!   無いのが普通なので、無いのは [`UserPlaceState::Absent`]・[`UserCertPin::Absent`]、通らない
//!   のは [`UserPlaceProblem`]（理由と対処の種類 [`UserRemedy`]）で返す。
//! - **読む入口**は [`load_pinned_user_cert`]（ChronoGazer 用）。サービスの置き場と違って
//!   **鍵の露出も確かめる**（鍵は同じ利用者のものなので開ける。3 者以外が読めれば通さない。
//!   中身は読まない）。
//! - **鍵と証明書は書かない**。作る・作り直すのは Hub の core（PR2）で、[`open_user_place`] が
//!   返す確かめ済みのハンドル（[`CheckedUserPlace`]・[`MissingUserPlace`]）を基準に書く。この
//!   crate が書くのは、下の失効の記録とそのロックファイルだけ。
//!
//! ## 利用者の失効の記録（#1121 PR1b、2026-11-14 オーナー決定）
//!
//! 鍵がほかのアカウントに読める状態だった証明書は、ACL が直っても二度と使わない（その間に
//! 鍵をコピーされたかもしれない）。露出を見た側（デスクトップ版の Hub・ChronoGazer
//! デスクトップ）が証明書の拇印（[`CertThumbprint`]。DER の SHA-256）を**失効の記録**に書き
//! 足し、拇印が記録にある証明書を pin にも待ち受けにも使わない（設計 §4.4.1・§4.4.2）。Hub と
//! ChronoGazer が同じ決まりで読み書きする必要があるので、`revocation` モジュールに置く:
//!
//! - **記録は 2 つ（A・B）で、和で照合する**。別のフォルダに置き（[`UserPlaceKind`]）、片方の
//!   置き場だけに書ける相手が、片方を消す・壊す・古い内容に戻しても、もう片方が残る。拇印は
//!   増えるだけで、減らさない。
//! - **形**（[`RevocationRecord`]）: フォルダの中の [`REVOCATIONS_FILE`] に、ASCII の行で
//!   「形の名前と版・状態（`準備中`／`確定`）・拇印の並び（拇印・どの置き場の証明書か・書いた
//!   時刻）・件数」。読むときは形を厳しく取り、少しでも違えば「壊れている」。依存を増やさない
//!   ため、JSON にはしない。
//! - **照合の読み口**（[`read_user_revocations`]）はロック無しで、書き込みをしない。確かめに
//!   通らない置き場の記録も、形が読めれば和に入れる（和は安全側にしか動かない）。
//! - **整える手順は 1 つ**（[`reconcile_user_revocations`]）。同じ利用者のどのプロセスが何度
//!   走らせても同じ結果になる。A・B の状態の表（未初期化・初期化の途中・整っている）に無い形は
//!   **消失**で、その場で「今ある利用者の証明書を失効に入れて、記録を直す」。記録のファイルを
//!   新しく作るときも、利用者の証明書がすでにあれば失効に入れる（正しい順序では、記録が 2 つ
//!   とも `確定` になる前に証明書は無いので、あれば記録を消されたとみなす）。
//! - **結果は 3 つ**（[`ReconcileOutcome`]）: 整っている・通らない（消失を見た、置き場かロックが
//!   通らない）・この回はできなかった（ロックが上限までに取れない、I/O の誤り）。「状態を確かに
//!   見た」と「操作が終わらなかった」を分ける。警告の理由（[`RevocationWarning`]）も返す。
//!   「この回はできなかった」が何周期続いたら止めるかは、呼ぶ側が数える。
//! - **失効を書き足す口**（[`record_user_revocation`]）は同じ手順で、和に拇印を足して両方に
//!   書く。露出を見た証明書の拇印は [`load_pinned_user_cert`] の「あるが通らない」
//!   （[`UnusableUserCert::exposed_thumbprint`]）から取る。
//! - **書き方**（[`RevocationOps`]。Windows の実装は `win::WindowsTlsTrustOps`、部品は
//!   `banto_safe_root`）: 置き場を [`open_user_place`] の確かめに通した直後に、**握ったフォルダの
//!   ハンドル基準**でだけ書く（パスで開き直さない）。記録は「同じフォルダの一時ファイル →
//!   書き切る → 置き換え」で、途中で止まっても前か新しい中身のどちらかが残る。フォルダが
//!   無ければ作り、作ったら起点から確かめ直す。確かめに通らない置き場には書かない。
//! - **直列化**は、A・B の外のロックファイルの排他の open（共有なしで開く。上限付きで待つ。既定
//!   [`DEFAULT_LOCK_WAIT`]）。名前付き mutex は使わない（別のアカウントが先に作って握れる）。
//!   ロックファイルも、置き場のフォルダと、開いた後のハンドルで確かめる。取れない・通らない
//!   ときは何も書かない。握ったプロセスが落ちれば OS がハンドルを閉じるので、残らない。
//!
//! ## テスト
//!
//! OS を触る操作はすべて [`TlsTrustOps`]・[`UserTlsOps`]・[`RevocationOps`]（Windows の実装は
//! `win::WindowsTlsTrustOps`）を通すので、確かめの順序と断る分岐は偽物で全 OS で確かめる
//! （`tests.rs`、利用者ごとの置き場は `tests/user.rs`、失効の記録は `tests/revocation.rs` で、
//! 設計 §4.4.2 の場面の番号を試験の名前に入れてある）。Hub 側の同じ手順の表は banto-hub-core の
//! `hub_tls_prepare/tests/load.rs` にある。

mod dacl;
mod ops;
mod place;
mod revocation;
pub mod text;
mod user;
#[cfg(windows)]
pub mod win;

pub use dacl::{root_dacl_problems, ROOT_TAKEOVER_MASK};
pub use ops::{FileOpenError, ReadError, TlsDir, TlsFile, TlsTrustOps};
pub use place::{
    check_cert_exposure, load_pinned_hub_cert, open_checked_file, open_place,
    parse_single_certificate, read_checked, CheckedPlace, HubCertPin, Remedy, Unavailable,
};
pub use revocation::{
    read_user_revocations, reconcile_user_revocations, record_user_revocation, CertThumbprint,
    ReconcileOptions, ReconcileOutcome, ReconcileReport, RecordState, RevocationOps,
    RevocationRecord, RevocationSide, RevocationWarning, RevokedCert, RevokedCertPlace,
    SideContent, UserRevocations, DEFAULT_LOCK_POLL, DEFAULT_LOCK_WAIT, MAX_REVOCATIONS_BYTES,
    REVOCATIONS_FILE, REVOCATIONS_HEADER,
};
pub use user::{
    check_place_layout, check_spec, is_app_container_sid, load_pinned_user_cert, open_user_file,
    open_user_place, peek_user_cert, user_place_problems, CheckedUserPlace, DefaultUserTlsPlace,
    MissingUserPlace, SeenUserCert, UncheckedUserCert, UnusableUserCert, UserCertPin, UserFault,
    UserFileRole, UserPlaceKind, UserPlacePolicy, UserPlaceProblem, UserPlaceSpec, UserPlaceState,
    UserRemedy, UserTlsOps, UserTlsPlace, USER_ANCHOR_LABEL, USER_PLACE_OTHERS_READ_MASK,
    USER_REVOCATIONS_A_DIR, USER_REVOCATIONS_B_DIR, USER_REVOCATIONS_LOCK_FILE,
};

/// ProgramData の下の製品のフォルダ（Hub のルート）。
pub const PRODUCT_DIR: &str = "BantoHub";

/// ルートの下の TLS の置き場のフォルダ名。
pub const TLS_DIR: &str = "tls";

/// 証明書（PEM。利用者は読み取りだけ）。
pub const CERT_FILE: &str = "hub-cert.pem";

/// 秘密鍵（PKCS#8 の PEM。SYSTEM・Administrators だけ）。
pub const KEY_FILE: &str = "hub-key.pem";

/// 読む PEM の大きさの上限。
pub const MAX_PEM_BYTES: usize = 64 * 1024;

/// Hub のサービスが既定で TLS を待ち受けるポート（127.0.0.1 だけ）。
pub const DEFAULT_TLS_PORT: u16 = 8723;

/// 文言の中のルートの呼び名。
pub const ROOT_LABEL: &str = "Hub のルート";

/// 文言の中の `tls\` の呼び名。
pub const TLS_LABEL: &str = "Hub の TLS の置き場";

#[cfg(test)]
mod tests;
