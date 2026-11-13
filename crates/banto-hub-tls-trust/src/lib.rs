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
//! ## テスト
//!
//! OS を触る操作はすべて [`TlsTrustOps`]（Windows の実装は `win::WindowsTlsTrustOps`）を通す
//! ので、確かめの順序と断る分岐は偽物で全 OS で確かめる（`tests.rs`）。Hub 側の同じ手順の
//! 表は banto-hub-core の `hub_tls_prepare/tests/load.rs` にある。

mod dacl;
mod ops;
mod place;
pub mod text;
#[cfg(windows)]
pub mod win;

pub use dacl::{root_dacl_problems, ROOT_TAKEOVER_MASK};
pub use ops::{FileOpenError, TlsDir, TlsFile, TlsTrustOps};
pub use place::{
    check_cert_exposure, load_pinned_hub_cert, open_checked_file, open_place,
    parse_single_certificate, read_checked, CheckedPlace, HubCertPin, Remedy, Unavailable,
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
