//! `banto-hub-elev prepare-hub-tls [--renew]`（#644 PR1a、2026-11-13 オーナー決定 1〜3）。
//!
//! banto-hub（平文 `http://127.0.0.1:8722`）が止まっている間に別のプロセスが 8722 を先に取ると、
//! クライアントは秘密（API キー）をその相手へ送ってしまう（#644）。Sink は #876 で証明書の pin
//! 付きの loopback 専用 TLS にしたが、Hub の TLS の待受（[`crate::tls_listener`]）は管理者が環境
//! 変数と OpenSSL で手作りする形だった。これを既定にするため、Hub の loopback TLS の鍵と
//! 証明書を、**作るのと同時に**保護された DACL を付けて
//! `<ProgramData>\BantoHub\tls\` に作る。サービスがここを読んで 127.0.0.1:8723 で待ち受けるのは
//! 次の PR（PR1b）で、この PR では Hub の起動は変えない。
//!
//! **ここは昇格の境界**（管理者として動き、一般の利用者が作れる場所 - ルート直下 - を扱う）。
//! [`crate::sink_config_prepare`]（`prepare-sink-config`）の型をそのまま使い、受け付けるものを
//! 固定し、迷ったら止める側に倒す。
//!
//! ## オーナー決定（2026-11-13）
//!
//! 1. 鍵と証明書は elev が Rust で作る（`rcgen`、ECDSA P-256・SHA-256、バックエンドは `ring`）。
//!    インストーラが `prepare-sink-config` と同じように呼ぶ。
//! 2. 置き場は `%ProgramData%\BantoHub\tls\`。フォルダは保護された DACL で SYSTEM・Administrators
//!    がフル、`BUILTIN\Users` が読み取りと実行（後の PR でクライアントが証明書を pin として
//!    読むため）、所有者 Administrators。秘密鍵（[`KEY_FILE`]）は SYSTEM・Administrators だけ。
//!    証明書（[`CERT_FILE`]）は SYSTEM・Administrators にフル＋Users に読み取り。どれも作るのと
//!    同時に DACL を付ける（作ってから絞るまでの間に広い権限で見える時間を作らない）。
//! 3. 有効期限は 10 年。無いときだけ作る（再インストールでは既存を保つ）。壊れている・期限
//!    切れ・片方だけ・ACL が露出（一般の利用者が書ける・鍵を読める）なら理由の行を出して断る
//!    （終了コード 1）。作り直しは管理者が `--renew` を明示する。
//!
//! ## 設計意図
//!
//! - **引数は固定**（無しか `--renew` だけ。[`parse_args`]）。ProgramData は既知のフォルダ
//!   （`SHGetKnownFolderPath`）から取り、`BANTO_HUB_ROOT`・このプロセスの環境変数には追従しない。
//! - **確かめ終わるまで副作用なし**（[`prepare_hub_tls`]）: 純粋な確かめ（ProgramData・既知の
//!   フォルダ）→ ルートを辿らずに開いて確かめる（本当のパス・reparse point でないフォルダ・
//!   所有者。無ければ親を確かめてから親のハンドル基準で作る）→ `tls\` を確かめ済みのルートの
//!   ハンドル基準で開く（無ければ上の DACL を作るのと同時に付けて作る）→ 本当のパス・所有者
//!   （SYSTEM・Administrators・昇格した本人以外なら**何も変えずに断る** - 一般の利用者が先に
//!   作った `tls\` の乗っ取りを拒む）・ACL（[`banto_safe_root::acl::exposure_beyond_users_read`]。
//!   Users の読み取りより広い許可・ほかの相手があれば断る。**`--renew` でも断る**）→ 中の鍵・
//!   証明書・一時名を置き場のハンドル基準で辿らずに開いて確かめる（フォルダ・リンクは断る）→
//!   既存の鍵と証明書を確かめる（[`inspect_identity`]）か、新しく作る（[`generate_identity`]）
//!   → **ここまで通ってから**書く。無いルート・`tls\` を作るのは確かめの途中で行う
//!   （`prepare-sink-config` と同じ。作るものは継承のルートと保護された `tls\` だけ）。
//! - **既存の確かめ**: 鍵は [`banto_safe_root::acl::exposure`]（SYSTEM・Administrators だけ。
//!   ハードリンク・ほかの所有者も断る）、証明書は `exposure_beyond_users_read`。中身は PEM が
//!   読める・証明書がちょうど 1 つで秘密鍵を含まない・鍵が PKCS#8 の ECDSA P-256・鍵と証明書の
//!   公開鍵が一致・期限内・Basic Constraints が CA:FALSE・SAN に IP 127.0.0.1・EKU があれば
//!   serverAuth を含む。どれかが通らなければ理由を出して断り、`--renew` を案内する。
//! - **`--renew`**: 置き場は上と同じく確かめる（露出していれば断る）。既存の鍵・証明書は中身も
//!   ACL も確かめない（露出した鍵の作り直しに使えるように。フォルダ・リンクは断る）。
//! - **書くこと**（この順。作るときと `--renew` で同じ）: 前回の途中で残った一時名
//!   （[`KEY_TEMP`]・[`CERT_TEMP`]）を消す → 新しい鍵を [`KEY_TEMP`] に（SYSTEM・Administrators
//!   だけ）、証明書を [`CERT_TEMP`] に（Users に読み取り）、作るのと同時に DACL を付けて書いて
//!   書き出す → （`--renew` なら）旧い証明書・鍵を消す → [`KEY_TEMP`] → [`KEY_FILE`]、
//!   [`CERT_TEMP`] → [`CERT_FILE`] の順に、辿らず・置き換えずに付け替える
//!   （`banto_safe_root::ManagedDir::rename_no_replace`）。途中で落ちて一時名が残っても、次の
//!   実行が確かめの後に消す（鍵が付け替わって証明書が一時名のまま残ったときは「片方だけ」で
//!   断るので、`--renew` で作り直す）。
//! - **直列化**（Codex P2）: 実行全体（preflight の後、ルートを開く前から最後まで）をプロセスを
//!   またぐ名前付き mutex（[`LOCK_NAME`]）で直列化する。重なると、片方が他方の作りかけの一時名を
//!   「前回の残り」として消し、鍵と証明書が別々の実行のものになり得るため。持たれていれば何も
//!   変えずに断る。
//! - ルートの DACL は書かない（`prepare-sink-config` と同じ。`profiles\` を巻き込まない）。
//!   既にある `tls\` の DACL も書き直さない（確かめて、合わなければ断る）。
//!
//! ## 出力・終了コード
//!
//! 行は `banto-hub-elev: ` で始まる。成功したときは証明書の絶対パス・SHA-256 の指紋（DER
//! 全体）・notAfter（UTC）を出す。**秘密鍵の中身は出さない**（PEM の読み込みの失敗の理由にも
//! 入力を含めない）。断ったときは理由の行を出して [`Refused`] を返し、呼ぶ側
//! （`crate::service_elevated`）が終了コード 1 にする。
//!
//! ## テスト
//!
//! OS を触る操作はすべて [`HubTlsOps`]（Windows の実装は `win::WindowsHubTlsOps`）を通すので、
//! 確かめの順序・確かめ終わる前に副作用が無いこと・断る分岐ごとの出力は偽物で全 OS で確かめる
//! （`tests.rs`。作った鍵と証明書を [`crate::tls_listener`] の読み込みで読み、
//! `banto_tagclient::PinnedHubTls` で pin したクライアントと握手することも）。Windows の実物
//! （一時フォルダ）は `win/tests.rs`（所有者を Administrators にするので、書くところまで進む
//! ものは管理者でしか走らない - CI は管理者）。

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    exposure, exposure_beyond_users_read, Exposure, FileSecurity, PinnedDirInfo,
};
use banto_safe_root::guard::{self, DataAclRefusal, DirState, KnownFolders, MachineEnvironment};
use banto_safe_root::InstallUserSid;
use banto_service_control::ConsoleLine;
use zeroize::Zeroizing;

use crate::profile_grant::{root_parent_problem, well_known_text, Out, Refused, LABEL};
use crate::sink_config_prepare::{
    dir_refusal_text, exposure_text, real_path_text, DirLevel, FileOpenError, PRODUCT_DIR,
};

#[cfg(windows)]
pub mod win;

/// 置き場のフォルダ名。
pub const TLS_DIR: &str = "tls";

/// 実行全体を直列化する名前付き mutex の名前。
pub const LOCK_NAME: &str = r"Global\BantoHub.prepare-hub-tls";

/// 証明書（PEM。利用者は読み取りだけ）。
pub const CERT_FILE: &str = "hub-cert.pem";

/// 秘密鍵（PKCS#8 の PEM。SYSTEM・Administrators だけ）。
pub const KEY_FILE: &str = "hub-key.pem";

/// 作りかけの証明書（書き終えてから [`CERT_FILE`] へ付け替える）。
pub const CERT_TEMP: &str = "hub-cert.pem.new";

/// 作りかけの秘密鍵（書き終えてから [`KEY_FILE`] へ付け替える）。
pub const KEY_TEMP: &str = "hub-key.pem.new";

/// 作り直しの引数（これだけを受け付ける）。
pub const RENEW_FLAG: &str = "--renew";

/// 読む PEM の大きさの上限。
pub const MAX_PEM_BYTES: usize = 64 * 1024;

/// 証明書の CN。
pub const COMMON_NAME: &str = "BantoHub loopback";

/// 有効期限の長さ（暦の 10 年。オーナー決定 3。[`not_after_for`]）。
pub const VALIDITY_YEARS: i32 = 10;

/// notBefore を今より前にずらす長さ（時計のずれで「まだ有効でない」にならないよう 1 時間）。
pub const BACKDATE_SECS: i64 = 3600;

fn out_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stdout(format!("{LABEL}: {text}"))
}

fn err_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stderr(format!("{LABEL}: {text}"))
}

/// 引数（action の後ろ）を読む（純関数）。無しなら `Ok(false)`、`--renew` ちょうど 1 つなら
/// `Ok(true)`。それ以外は理由の文言（呼ぶ側が `ElevatedError::InvalidArgs` にする）。
pub fn parse_args(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [only] if only == RENEW_FLAG => Ok(true),
        _ => Err(format!(
            "'prepare-hub-tls' の引数は無しか {RENEW_FLAG} だけですが、{} 個（{}）受け取りました",
            args.len(),
            args.join(" ")
        )),
    }
}

// --- 鍵と証明書 --------------------------------------------------------------------

/// 作った鍵と証明書（PEM）。鍵は落とすときに消す。
pub struct Identity {
    /// 証明書の PEM（公開してよい）。
    pub cert_pem: String,
    /// 秘密鍵の PKCS#8 の PEM（出力・ログに出さない）。
    pub key_pem: Zeroizing<String>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("cert_pem", &self.cert_pem)
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

/// 確かめた証明書について出すこと。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityInfo {
    /// DER 全体の SHA-256（大文字の 16 進を `:` で区切る）。
    pub fingerprint: String,
    /// notAfter（Unix 秒、UTC）。
    pub not_after_unix: i64,
}

/// `now_unix` の暦の [`VALIDITY_YEARS`] 年後（同じ月日・時刻。2 月 29 日は 2 月 28 日にする。
/// 純関数）。
pub fn not_after_for(now_unix: i64) -> Result<i64, String> {
    let at = time::OffsetDateTime::from_unix_timestamp(now_unix)
        .map_err(|_| format!("時刻（{now_unix}）を証明書の期限にできません"))?;
    let year = at.year() + VALIDITY_YEARS;
    at.replace_year(year)
        .or_else(|_| at.replace_day(28).and_then(|day| day.replace_year(year)))
        .map(|later| later.unix_timestamp())
        .map_err(|_| format!("時刻（{now_unix}）の {VALIDITY_YEARS} 年後を求められません"))
}

/// 鍵（ECDSA P-256）と自己署名の証明書を作る（純関数。乱数は `ring`）。CN は [`COMMON_NAME`]、
/// SAN は IP 127.0.0.1 だけ、Basic Constraints は CA:FALSE を明示（rustls は CA:TRUE の
/// end-entity を断る）、Key Usage は digitalSignature、EKU は serverAuth、notBefore は
/// `now_unix` の [`BACKDATE_SECS`] 前、notAfter は [`not_after_for`]（暦の 10 年後）。
pub fn generate_identity(now_unix: i64) -> Result<Identity, String> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
    };
    let time = |unix: i64| {
        time::OffsetDateTime::from_unix_timestamp(unix)
            .map_err(|_| format!("時刻（{unix}）を証明書の期限にできません"))
    };
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|err| format!("鍵を作れませんでした: {err}"))?;
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, COMMON_NAME);
    params.distinguished_name = name;
    params.subject_alt_names = vec![SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))];
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = time(now_unix - BACKDATE_SECS)?;
    params.not_after = time(not_after_for(now_unix)?)?;
    let cert = params
        .self_signed(&key)
        .map_err(|err| format!("証明書を作れませんでした: {err}"))?;
    Ok(Identity {
        cert_pem: cert.pem(),
        key_pem: Zeroizing::new(key.serialize_pem()),
    })
}

/// DER 全体の SHA-256 の指紋（大文字の 16 進を `:` で区切る。純関数）。
pub fn fingerprint_text(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Unix 秒を UTC の `YYYY-MM-DD HH:MM:SS UTC` にする（純関数）。
pub fn utc_text(unix: i64) -> String {
    match chrono::DateTime::<chrono::Utc>::from_timestamp(unix, 0) {
        Some(at) => at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => format!("（Unix 秒 {unix}）"),
    }
}

/// 既にある鍵と証明書を確かめる（純関数。モジュール doc「既存の確かめ」）。通らなければ理由の
/// 文言（入力の中身は含めない）。
pub fn inspect_identity(
    cert_pem: &[u8],
    key_pem: &[u8],
    now_unix: i64,
) -> Result<IdentityInfo, String> {
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use x509_cert::der::Decode;
    use x509_cert::ext::pkix::name::GeneralName;
    use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, SubjectAltName};

    if cert_pem
        .windows(b"PRIVATE KEY".len())
        .any(|window| window == b"PRIVATE KEY")
    {
        return Err("証明書のファイルに秘密鍵が入っています".to_string());
    }
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|_| "証明書の PEM を読めません".to_string())?;
    let [cert_der] = certs.as_slice() else {
        return Err(format!(
            "証明書の PEM に証明書がちょうど 1 つではありません（{} 個）",
            certs.len()
        ));
    };
    let key = PrivateKeyDer::from_pem_slice(key_pem)
        .map_err(|_| "秘密鍵の PEM を読めません".to_string())?;
    let PrivateKeyDer::Pkcs8(pkcs8) = &key else {
        return Err("秘密鍵が PKCS#8 ではありません".to_string());
    };
    let pair = rcgen::KeyPair::try_from(pkcs8)
        .map_err(|_| "秘密鍵を読めません（ECDSA P-256 の PKCS#8 ではありません）".to_string())?;
    if !pair.is_compatible(&rcgen::PKCS_ECDSA_P256_SHA256) {
        return Err("秘密鍵が ECDSA P-256 ではありません".to_string());
    }
    let cert = x509_cert::Certificate::from_der(cert_der.as_ref())
        .map_err(|_| "証明書（DER）を読めません".to_string())?;
    let tbs = &cert.tbs_certificate;
    if tbs.subject_public_key_info.subject_public_key.raw_bytes() != pair.public_key_raw() {
        return Err("秘密鍵と証明書が対になっていません".to_string());
    }
    let secs = |at: x509_cert::time::Time| i64::try_from(at.to_unix_duration().as_secs());
    let (Ok(not_before), Ok(not_after)) =
        (secs(tbs.validity.not_before), secs(tbs.validity.not_after))
    else {
        return Err("証明書の有効期間を読めません".to_string());
    };
    if now_unix < not_before {
        return Err(format!(
            "証明書はまだ有効になっていません（notBefore {}）",
            utc_text(not_before)
        ));
    }
    if now_unix > not_after {
        return Err(format!(
            "証明書の期限が切れています（notAfter {}）",
            utc_text(not_after)
        ));
    }
    match tbs.get::<BasicConstraints>() {
        Ok(Some((_, constraints))) if !constraints.ca => {}
        Ok(Some(_)) => return Err("証明書が CA の証明書です（CA:TRUE）".to_string()),
        Ok(None) => return Err("証明書に Basic Constraints（CA:FALSE）がありません".to_string()),
        Err(_) => return Err("証明書の Basic Constraints を読めません".to_string()),
    }
    let loopback = match tbs.get::<SubjectAltName>() {
        Ok(Some((_, names))) => names.0.iter().any(
            |name| matches!(name, GeneralName::IpAddress(ip) if ip.as_bytes() == [127, 0, 0, 1]),
        ),
        Ok(None) => false,
        Err(_) => return Err("証明書の SAN を読めません".to_string()),
    };
    if !loopback {
        return Err("証明書の SAN に IP 127.0.0.1 がありません".to_string());
    }
    let server_auth = x509_cert::der::asn1::ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1");
    match tbs.get::<ExtendedKeyUsage>() {
        Ok(Some((_, usage))) if !usage.0.contains(&server_auth) => {
            return Err("証明書の用途（EKU）に serverAuth がありません".to_string())
        }
        Ok(_) => {}
        Err(_) => return Err("証明書の用途（EKU）を読めません".to_string()),
    }
    Ok(IdentityInfo {
        fingerprint: fingerprint_text(cert_der.as_ref()),
        not_after_unix: not_after,
    })
}

// --- OS の操作 ---------------------------------------------------------------------

/// 辿らずに開いたフォルダ（Windows は `banto_safe_root::acl::PinnedDir` を
/// `banto_safe_root::ManagedDir` に包んだもの）。
pub trait TlsDir {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから読んだ属性・所有者・DACL。
    fn security(&self) -> Result<FileSecurity, String>;
}

/// 置き場のハンドル基準に、読み取りの権利で辿らずに開いたファイル（Windows は
/// `banto_safe_root::acl::PinnedFile`）。持っている間はほかの誰も書き込みに開く・消す・名前を
/// 変えることができない（共有は読み取りだけ）。
pub trait TlsFile {
    /// 開いた時点でハンドルから読んだ属性・リンク数・所有者・DACL。
    fn security(&self) -> FileSecurity;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから、`max_len` バイトまで読む（超えたら断る）。
    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, String>;
}

/// フォルダの作り方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirHow {
    /// 親から継承する普通のフォルダ（ルート）。
    Inherit,
    /// SYSTEM・Administrators にフル、Users に読み取り、所有者 Administrators（`tls\`）。
    UsersRead,
}

/// ファイルに作るのと同時に付ける DACL。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAcl {
    /// SYSTEM・Administrators だけ（秘密鍵）。
    ServiceOnly,
    /// SYSTEM・Administrators にフル、Users に読み取り（証明書）。
    UsersRead,
}

/// `prepare-hub-tls` が OS に対してする操作（Windows の実装は `win::WindowsHubTlsOps`。テストは
/// 偽物）。
pub trait HubTlsOps {
    /// マシンの環境の読み取り（ProgramData・既知のフォルダ。マシンの環境変数は使わない）。
    type Env: MachineEnvironment;
    /// [`HubTlsOps::pin_dir`] などが返すもの。
    type Dir: TlsDir;
    /// [`HubTlsOps::open_child_file`] が返すもの。
    type File: TlsFile;
    /// [`HubTlsOps::hold_lock`] が返す、持っている間だけ有効なもの（drop で離す）。
    type Lock;

    /// [`LOCK_NAME`] の名前付き mutex を取る（既に誰かが持っていれば `Ok(None)`）。
    fn hold_lock(&self) -> Result<Option<Self::Lock>, String>;
    fn machine_env(&self) -> &Self::Env;
    /// このプロセスのトークンの利用者の SID（所有者として許す）。
    fn current_user_sid(&self) -> Result<InstallUserSid, String>;
    /// 今の時刻（Unix 秒、UTC）。
    fn now_unix(&self) -> i64;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素のフォルダを作る（既にあれば
    /// `Ok(None)`）。`UsersRead` なら作るのと同時にその DACL を付ける。
    fn create_child_dir(
        &self,
        parent: &Self::Dir,
        name: &str,
        how: DirHow,
    ) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素の子を辿らずに開く。
    fn open_child_dir(&self, parent: &Self::Dir, name: &str) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素の子を読み取りで辿らずに開く（共有は
    /// 読み取りだけ）。
    fn open_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError>;
    /// 確かめ済みの `parent` のハンドル基準に、`acl` を作るのと同時に付けた新しいファイルを作って
    /// 書き、書き出す。既にあれば `Ok(false)`（何もしない）。
    fn create_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
        contents: &[u8],
        acl: FileAcl,
    ) -> Result<bool, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素のファイルを辿らずに消す（無ければ
    /// `Ok(false)`。リンク・フォルダは消さずに断る）。
    fn remove_child(&self, parent: &Self::Dir, name: &str) -> Result<bool, String>;
    /// 確かめ済みの `parent` のハンドル基準に、`from` を `to` へ辿らず・置き換えずに付け替える
    /// （`from` が無ければ `Ok(false)`、`to` があれば失敗）。
    fn rename_no_replace(&self, parent: &Self::Dir, from: &str, to: &str) -> Result<bool, String>;
}

// --- 手順 --------------------------------------------------------------------------

/// 純粋な確かめの結果。
struct Plan {
    root: PathBuf,
    known: KnownFolders,
    elevated_user: Option<InstallUserSid>,
}

/// 純粋な確かめ（ファイルシステムには何もしない）: ProgramData → ルートの形と既知のフォルダ →
/// 昇格した本人。
fn preflight<O: HubTlsOps>(ops: &O) -> Result<Plan, String> {
    let program_data = ops.machine_env().program_data().map_err(|error| {
        format!("ProgramData の場所を取れませんでした: {error}。何も変えていません")
    })?;
    let root = program_data.join(PRODUCT_DIR);
    if !root.is_absolute() {
        return Err(format!(
            "ルート（{}）が絶対パスではありません。何も変えていません",
            root.display()
        ));
    }
    let known = ops.machine_env().known_folders().map_err(|error| {
        format!("既知のフォルダの場所を取れませんでした: {error}。何も変えていません")
    })?;
    if let Err(refusal) = guard::reject_well_known_root(&root, &known) {
        return Err(format!(
            "{}。何も変えていません",
            well_known_text(&root, &refusal)
        ));
    }
    Ok(Plan {
        root,
        known,
        elevated_user: ops.current_user_sid().ok(),
    })
}

/// 自分で作ったフォルダ（断ったときに残っていることを知らせる）。
#[derive(Default)]
struct Created(Vec<(&'static str, PathBuf)>);

impl Created {
    fn notice(&self, out: Out<'_>) {
        for (label, dir) in &self.0 {
            out(err_line(&format!(
                "{label}（{}）は作りましたが、確かめが通らなかったので残っています",
                dir.display()
            )));
        }
    }
}

/// 作るか、既にあれば親のハンドル基準で開き直す（パスで開き直さない）。
fn create_or_open<O: HubTlsOps>(
    ops: &O,
    parent: &O::Dir,
    name: &str,
    how: DirHow,
) -> Result<(O::Dir, bool), String> {
    match ops.create_child_dir(parent, name, how)? {
        Some(dir) => Ok((dir, true)),
        None => match ops.open_child_dir(parent, name)? {
            Some(dir) => Ok((dir, false)),
            None => Err("作ろうとしたら既にあり、開き直すと無くなっていました".to_string()),
        },
    }
}

const ROOT_LABEL: &str = "Hub のルート";
const TLS_LABEL: &str = "Hub の TLS の置き場";

/// `tls\` の所有者・種類の断りの文言（純関数）。所有者で断ったときは取り直さず、中身を確かめて
/// 消してから再実行を案内する（`prepare-sink-config` の `sink\` と同じ理由: 一般の利用者が先に
/// 作った `tls\` は、中身まで相手が用意したものでありうる）。
fn tls_dir_refusal_text(dir: &Path, refusal: &DataAclRefusal) -> String {
    let reason = match refusal {
        DataAclRefusal::ReparsePoint => "ジャンクションかシンボリックリンク（reparse point）です。リンクの先へ書かないよう、止めます（リンクを外してから、もう一度実行してください）".to_string(),
        DataAclRefusal::NotADirectory => "フォルダではありません。止めます".to_string(),
        DataAclRefusal::ForeignOwner(owner) => format!(
            "所有者（{owner}）が SYSTEM・Administrators・実行した管理者のどれでもありません。一般の利用者が先に作った（乗っ取りの準備の）恐れがあるため、何も変えずに止めます（所有者は取り直さず、中身を確かめてから、このフォルダを消して、もう一度実行してください。中の鍵・証明書は使わず、作り直されます）"
        ),
    };
    format!("{TLS_LABEL}（{}）は{reason}", dir.display())
}

/// 置き場の中で見つけたファイル（確かめた属性と、読んだときは中身）。
struct Found {
    security: FileSecurity,
    contents: Option<Zeroizing<Vec<u8>>>,
}

/// 置き場の中のファイル 1 つを開いて確かめる（無ければ `None`）。フォルダ・reparse point・
/// 本当の場所の食い違いは断る。`read` なら中身も読む（ハンドルは返す前に閉じる）。
fn inspect_child<O: HubTlsOps>(
    ops: &O,
    tls: &O::Dir,
    tls_path: &Path,
    tls_real: &Path,
    name: &str,
    label: &str,
    read: bool,
) -> Result<Option<Found>, String> {
    let shown = tls_path.join(name);
    let mut file = match ops.open_child_file(tls, name) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(FileOpenError::InUse) => {
            return Err(format!(
                "{label}（{}）は別のプロセスで書き込みに開かれています。閉じてから、もう一度実行してください",
                shown.display()
            ))
        }
        Err(FileOpenError::Other(error)) => {
            return Err(format!(
                "{label}（{}）を開けませんでした: {error}",
                shown.display()
            ))
        }
    };
    let security = file.security();
    let problem = if security.is_dir {
        Some("フォルダです".to_string())
    } else if security.is_reparse_point {
        Some("シンボリックリンクなどの reparse point です".to_string())
    } else if !guard::paths_match(&file.final_path(), &tls_real.join(name)) {
        Some(format!(
            "本当の場所（{}）が期待と違います",
            file.final_path().display()
        ))
    } else {
        None
    };
    if let Some(problem) = problem {
        return Err(format!(
            "{label}（{}）は{problem}。何も変えずに止めます（中身を確かめ、消してから、もう一度実行してください）",
            shown.display()
        ));
    }
    let contents = if read {
        Some(Zeroizing::new(file.read_all(MAX_PEM_BYTES).map_err(
            |error| format!("{label}（{}）を読めませんでした: {error}", shown.display()),
        )?))
    } else {
        None
    };
    Ok(Some(Found { security, contents }))
}

fn reasons_text(found: &[Exposure]) -> String {
    found
        .iter()
        .map(exposure_text)
        .collect::<Vec<_>>()
        .join("・")
}

/// 作り直しの案内（断りの文言の後ろに付ける）。
fn renew_hint() -> String {
    format!(
        "管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls {RENEW_FLAG} を実行して作り直し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてください"
    )
}

/// 確かめた後にすること。
enum Action {
    /// 既にある鍵と証明書を保つ。
    Keep(IdentityInfo),
    /// 無いので作る。
    Create(Identity, IdentityInfo),
    /// `--renew`: 作り直す。
    Renew(Identity, IdentityInfo),
}

/// `prepare-hub-tls` の本体（モジュール doc「設計意図」）。
pub fn prepare_hub_tls<O: HubTlsOps>(ops: &O, renew: bool, out: Out<'_>) -> Result<(), Refused> {
    let mut created = Created::default();
    match run(ops, renew, out, &mut created) {
        Ok(()) => Ok(()),
        Err(text) => {
            out(err_line(&text));
            created.notice(out);
            Err(Refused)
        }
    }
}

fn run<O: HubTlsOps>(
    ops: &O,
    renew: bool,
    out: Out<'_>,
    created: &mut Created,
) -> Result<(), String> {
    let plan = preflight(ops)?;
    // 実行全体を直列化する。最後まで持つ（ローカルの drop は宣言の逆順なので、下で開く
    // フォルダのハンドルを閉じた後に離す）。
    let _lock = match ops.hold_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return Err(format!(
                "別の prepare-hub-tls が実行中です（{LOCK_NAME}）。終わってから、もう一度実行してください。何も変えていません"
            ))
        }
        Err(error) => {
            return Err(format!(
                "{LOCK_NAME} を取れませんでした: {error}。何も変えていません"
            ))
        }
    };
    let tls_path = plan.root.join(TLS_DIR);

    // --- ルート（辿らずに開く。無ければ親を確かめてから、親のハンドル基準で作る） ------
    let mut root_parent: Option<O::Dir> = None;
    let root = match ops.pin_dir(&plan.root) {
        Ok(Some(root)) => root,
        Err(error) => {
            return Err(format!(
                "{ROOT_LABEL}（{}）を開けませんでした: {error}",
                plan.root.display()
            ))
        }
        Ok(None) => {
            let parent_dir = plan.root.parent().unwrap_or(&plan.root).to_path_buf();
            let parent = match ops.pin_dir(&parent_dir) {
                Ok(Some(parent)) => parent,
                Ok(None) => {
                    return Err(format!(
                        "ProgramData（{}）がありません",
                        parent_dir.display()
                    ))
                }
                Err(error) => {
                    return Err(format!(
                        "ProgramData（{}）を開けませんでした: {error}",
                        parent_dir.display()
                    ))
                }
            };
            if let Some(problem) = root_parent_problem(
                &parent.final_path(),
                &parent_dir,
                &parent.info(),
                &plan.known,
            ) {
                return Err(format!(
                    "{ROOT_LABEL}（{}）の親（{}）は{problem}。何も作らずに止めます",
                    plan.root.display(),
                    parent_dir.display()
                ));
            }
            let made = create_or_open(ops, &parent, PRODUCT_DIR, DirHow::Inherit);
            root_parent = Some(parent);
            let (root, was_created) =
                made.map_err(|error| format!("{ROOT_LABEL}（{}）: {error}", plan.root.display()))?;
            if was_created {
                created.0.push((ROOT_LABEL, plan.root.clone()));
            }
            root
        }
    };
    let root_real = root.final_path();
    if let Err(refusal) = guard::check_real_path(&root_real, &plan.root, &plan.known) {
        return Err(real_path_text(ROOT_LABEL, &plan.root, &root_real, refusal));
    }
    let elevated = plan.elevated_user.as_ref().map(InstallUserSid::as_str);
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(root.info()), None, elevated) {
        return Err(dir_refusal_text(DirLevel::Root, &plan.root, &refusal));
    }

    // --- 置き場（確かめ済みのルートのハンドル基準。無ければ DACL を付けて作る） ---------
    let opened = match ops.open_child_dir(&root, TLS_DIR) {
        Ok(Some(tls)) => Ok((tls, false)),
        Ok(None) => create_or_open(ops, &root, TLS_DIR, DirHow::UsersRead),
        Err(error) => Err(format!("開けませんでした: {error}")),
    };
    let (tls, tls_created) =
        opened.map_err(|error| format!("{TLS_LABEL}（{}）: {error}", tls_path.display()))?;
    if tls_created {
        created.0.push((TLS_LABEL, tls_path.clone()));
    }
    let tls_real = tls.final_path();
    if let Err(refusal) = guard::check_real_path(&tls_real, &root_real.join(TLS_DIR), &plan.known) {
        return Err(real_path_text(TLS_LABEL, &tls_path, &tls_real, refusal));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(tls.info()), None, elevated) {
        return Err(tls_dir_refusal_text(&tls_path, &refusal));
    }
    let tls_security = tls.security().map_err(|error| {
        format!(
            "{TLS_LABEL}（{}）の ACL を読めませんでした: {error}",
            tls_path.display()
        )
    })?;
    let tls_open = exposure_beyond_users_read(&tls_security);
    if !tls_open.is_empty() {
        return Err(format!(
            "{TLS_LABEL}（{}）は保護されていません（{}）。ほかの利用者が鍵・証明書を書き換えた・読んだ恐れがあるため、何も変えずに止めます（{RENEW_FLAG} でも作り直しません）。中身を確かめ、このフォルダを消してから、もう一度 prepare-hub-tls を実行し、この証明書を pin しているクライアントを新しい証明書に書き換えてください",
            tls_path.display(),
            reasons_text(&tls_open)
        ));
    }

    // --- 中のファイル（置き場のハンドル基準。辿らない） --------------------------------
    let read = !renew;
    let key = inspect_child(ops, &tls, &tls_path, &tls_real, KEY_FILE, "秘密鍵", read)?;
    let cert = inspect_child(ops, &tls, &tls_path, &tls_real, CERT_FILE, "証明書", read)?;
    let key_temp = inspect_child(
        ops,
        &tls,
        &tls_path,
        &tls_real,
        KEY_TEMP,
        "作りかけの秘密鍵",
        false,
    )?;
    let cert_temp = inspect_child(
        ops,
        &tls,
        &tls_path,
        &tls_real,
        CERT_TEMP,
        "作りかけの証明書",
        false,
    )?;
    let key_path = tls_path.join(KEY_FILE);
    let cert_path = tls_path.join(CERT_FILE);
    let now = ops.now_unix();

    let new_identity = || -> Result<(Identity, IdentityInfo), String> {
        let identity = generate_identity(now)?;
        // 作ったものを既存と同じ確かめに通す（自分の確かめ）。
        let info = inspect_identity(
            identity.cert_pem.as_bytes(),
            identity.key_pem.as_bytes(),
            now,
        )
        .map_err(|error| format!("作った鍵と証明書が確かめを通りません（{error}）"))?;
        Ok((identity, info))
    };
    let action = if renew {
        let (identity, info) = new_identity()?;
        Action::Renew(identity, info)
    } else {
        match (&key, &cert) {
            (None, None) => {
                let (identity, info) = new_identity()?;
                Action::Create(identity, info)
            }
            (Some(_), None) | (None, Some(_)) => {
                let (have, missing) = if key.is_some() {
                    (&key_path, &cert_path)
                } else {
                    (&cert_path, &key_path)
                };
                return Err(format!(
                    "{} はありますが {} がありません（片方だけ）。何も変えずに止めます。{}",
                    have.display(),
                    missing.display(),
                    renew_hint()
                ));
            }
            (Some(key), Some(cert)) => {
                let key_open = exposure(&key.security);
                if !key_open.is_empty() {
                    return Err(format!(
                        "秘密鍵（{}）は保護されていません（{}）。ほかの利用者に読まれた・すり替えられた恐れがあるため、使わずに止めます。{}",
                        key_path.display(),
                        reasons_text(&key_open),
                        renew_hint()
                    ));
                }
                let cert_open = exposure_beyond_users_read(&cert.security);
                if !cert_open.is_empty() {
                    return Err(format!(
                        "証明書（{}）は保護されていません（{}）。ほかの利用者に書き換えられた恐れがあるため、使わずに止めます。{}",
                        cert_path.display(),
                        reasons_text(&cert_open),
                        renew_hint()
                    ));
                }
                let (Some(key_pem), Some(cert_pem)) = (&key.contents, &cert.contents) else {
                    return Err("鍵と証明書の中身を読んでいません".to_string());
                };
                let info = inspect_identity(cert_pem, key_pem, now).map_err(|error| {
                    format!(
                        "既にある鍵と証明書（{}）は使えません: {error}。何も変えずに止めます。{}",
                        tls_path.display(),
                        renew_hint()
                    )
                })?;
                Action::Keep(info)
            }
        }
    };
    let had_key = key.is_some();
    let had_cert = cert.is_some();
    drop((key, cert));

    // --- ここから書く ------------------------------------------------------------------
    // 作ったフォルダは、ここからは確かめを通ったものとして残す。
    created.0.clear();
    if tls_created {
        out(out_line(&format!(
            "{TLS_LABEL}（{}）を作りました（SYSTEM・Administrators にフル、利用者は読み取りだけ、所有者は Administrators、親から継承しない）",
            tls_path.display()
        )));
    }
    for (name, found) in [(KEY_TEMP, &key_temp), (CERT_TEMP, &cert_temp)] {
        if found.is_none() {
            continue;
        }
        match ops.remove_child(&tls, name) {
            Ok(_) => out(out_line(&format!(
                "前回の途中で残った {} を消しました",
                tls_path.join(name).display()
            ))),
            Err(error) => {
                return Err(format!(
                    "前回の途中で残った {} を消せませんでした: {error}",
                    tls_path.join(name).display()
                ))
            }
        }
    }

    let (identity, info, verb) = match action {
        Action::Keep(info) => {
            out(out_line(&format!(
                "Hub の TLS の鍵と証明書は既にあり、確かめて保ちました（作り直すときは {RENEW_FLAG}）"
            )));
            print_identity(out, &cert_path, &info);
            drop(tls);
            drop(root);
            drop(root_parent);
            return Ok(());
        }
        Action::Create(identity, info) => (identity, info, "作りました"),
        Action::Renew(identity, info) => (identity, info, "作り直しました"),
    };

    let leftover = |what: &str| {
        format!(
            "{what}。作りかけ（{KEY_TEMP}・{CERT_TEMP}）が残っていれば、次の実行で消します。もう一度実行してください"
        )
    };
    match ops.create_child_file(
        &tls,
        KEY_TEMP,
        identity.key_pem.as_bytes(),
        FileAcl::ServiceOnly,
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Err(leftover(&format!(
                "{} を作ろうとしたら既にありました（誰かが作った）",
                tls_path.join(KEY_TEMP).display()
            )))
        }
        Err(error) => {
            return Err(leftover(&format!(
                "{} を作れませんでした: {error}",
                tls_path.join(KEY_TEMP).display()
            )))
        }
    }
    let cert_made = ops.create_child_file(
        &tls,
        CERT_TEMP,
        identity.cert_pem.as_bytes(),
        FileAcl::UsersRead,
    );
    drop(identity);
    if !matches!(cert_made, Ok(true)) {
        // 作った鍵の一時名は残さない（消せなければ次の実行で消す）。
        let _ = ops.remove_child(&tls, KEY_TEMP);
        let what = match cert_made {
            Ok(_) => format!(
                "{} を作ろうとしたら既にありました（誰かが作った）",
                tls_path.join(CERT_TEMP).display()
            ),
            Err(error) => format!(
                "{} を作れませんでした: {error}",
                tls_path.join(CERT_TEMP).display()
            ),
        };
        return Err(leftover(&what));
    }
    if renew {
        for (name, present) in [(CERT_FILE, had_cert), (KEY_FILE, had_key)] {
            if !present {
                continue;
            }
            if let Err(error) = ops.remove_child(&tls, name) {
                return Err(leftover(&format!(
                    "旧い {} を消せませんでした: {error}（{RENEW_FLAG} でもう一度作り直してください）",
                    tls_path.join(name).display()
                )));
            }
        }
    }
    for (from, to) in [(KEY_TEMP, KEY_FILE), (CERT_TEMP, CERT_FILE)] {
        match ops.rename_no_replace(&tls, from, to) {
            Ok(true) => {}
            Ok(false) => {
                return Err(format!(
                    "{} を {to} に付け替えようとしたら無くなっていました。{}",
                    tls_path.join(from).display(),
                    renew_hint()
                ))
            }
            Err(error) => {
                return Err(format!(
                    "{} を {to} に付け替えられませんでした: {error}。{}",
                    tls_path.join(from).display(),
                    renew_hint()
                ))
            }
        }
    }
    out(out_line(&format!(
        "Hub の TLS の鍵（{}。SYSTEM・Administrators だけ）と証明書（利用者は読み取りだけ）を{verb}（ECDSA P-256、有効期限 10 年）",
        key_path.display()
    )));
    print_identity(out, &cert_path, &info);
    if renew {
        out(err_line(
            "警告: 証明書を作り直したので、旧い証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）は、新しい証明書に書き換えるまで Hub に繋がりません。Hub と Sink のサービスも再起動してください",
        ));
    }
    // 開いた順の逆に閉じる。
    drop(tls);
    drop(root);
    drop(root_parent);
    Ok(())
}

/// 成功したときの、証明書の行（絶対パス・SHA-256 の指紋・notAfter）。
fn print_identity(out: Out<'_>, cert_path: &Path, info: &IdentityInfo) {
    out(out_line(&format!("証明書: {}", cert_path.display())));
    out(out_line(&format!(
        "証明書の SHA-256 指紋（DER 全体）: {}",
        info.fingerprint
    )));
    out(out_line(&format!(
        "証明書の有効期限（notAfter）: {}",
        utc_text(info.not_after_unix)
    )));
}

#[cfg(test)]
mod tests;
