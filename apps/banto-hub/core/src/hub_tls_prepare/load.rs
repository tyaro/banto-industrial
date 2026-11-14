//! Hub の Windows サービスが既定で使う loopback TLS の鍵と証明書を読む（#644 PR1b、
//! 2026-11-13 オーナー決定 A・B）。
//!
//! `banto-hub-elev prepare-hub-tls`（[`super`]）が `<ProgramData>\BantoHub\tls\` に作った
//! [`KEY_FILE`]・[`CERT_FILE`] を、**`tls\` には書き込み・作成を一切せずに**、置き場ごと確かめて
//! から読む。書くのはサービスの失効の記録だけ（下の手順 6 と「露出を見たとき」）。
//! サービス（`HubHostKind::Service`）は、Hub の TLS の環境変数（`BANTO_HUB_TLS_PORT/CERT/KEY`）
//! がどれも無いときにここを呼び、読めたら 127.0.0.1:[`DEFAULT_TLS_PORT`] で TLS を待ち受ける
//! （`crate::runtime::HubConfig::default_tls`）。読めなければ（無い・片方だけ・壊れている・
//! 期限切れ・ACL が露出・ルートが保護されていない）、TLS 無しで Hub を起動し、サービスログに
//! 理由と対処を出す（[`DefaultTlsUnavailable::log_lines`]。オーナー決定 A）。
//!
//! ## 確かめの順序（[`load_default_identity`]）
//!
//! 1. ProgramData は既知のフォルダ（`SHGetKnownFolderPath`）から取る（`BANTO_HUB_ROOT`・
//!    環境変数には追従しない）。ルート `<ProgramData>\BantoHub` が絶対パスで、既知のフォルダ
//!    そのものでないこと。
//! 2. ルートを辿らずに開き、本当のパス・reparse point でないフォルダ・所有者
//!    （SYSTEM・Administrators・TrustedInstaller）・DACL（[`root_dacl_problems`](super::root_dacl_problems)。実行した管理者
//!    本人は無いので `None`）を確かめる。
//! 3. `tls\` をルートのハンドル基準で辿らずに開き、本当のパス・種類・所有者・
//!    [`exposure_beyond_users_read`](banto_safe_root::acl::exposure_beyond_users_read) を確かめる。
//! 4. 鍵・証明書を `tls\` のハンドル基準で辿らずに読み取りで開く（共有は読み取りだけ。握って
//!    いる間は誰も書き換えられない）。フォルダ・reparse point・本当の場所の食い違いは断る。
//!    両方あること（片方だけは断る）。鍵は [`exposure`]（SYSTEM・Administrators だけ）、証明書は
//!    [`exposure_beyond_users_read`](banto_safe_root::acl::exposure_beyond_users_read)。
//! 5. 確かめてから、握ったハンドルから [`MAX_PEM_BYTES`](super::MAX_PEM_BYTES) まで読み、[`inspect_identity`] で中身を
//!    確かめる。
//! 6. **サービスの失効の記録**（#1121 PR2b。docs/hub-desktop-tls-design.md §4.4.1「サービスの
//!    記録」、場面 25・26）: 確かめたルートのハンドル基準で `tls-revocations\` を確かめ（無ければ
//!    `tls\` と同じ保護された DACL を付けて作る）、記録を読む（無ければ空で作る。#1116 で作った
//!    証明書だけがある PC のため。置き換えずに作るので、同時に elev が作った記録を上書きしない）。
//!    証明書の拇印が記録にあれば使わない。置き場が確かめに通らない・形が壊れている・作れない
//!    （I/O）も使わない。どれも TLS 無しで起動し、理由と対処をログに出す（閉じる側）。読み書きは
//!    共有 crate の `banto_hub_tls_trust::ensure_service_revocations`。
//!
//! **露出を見たとき**（#1121 PR2c、場面 8。2026-11-14 オーナー決定 1）: 手順 2〜4 で露出（信頼する
//! 相手以外への許可）か所有者の違い（共有 crate の `PlaceFault::revokes`）を見たら、ディスク上の
//! 証明書を形だけ読み（`banto_hub_tls_trust::peek_service_cert`。ルートが通るときだけ）、その拇印を
//! サービスの記録に書いてから断る（`record_service_revocations`。証明書が読めなければ書けるものが
//! 無いので書かない）。書けた・書けなかったは理由の文に足す。リンク・ハードリンク・壊れ・無い・
//! 一時的な失敗は書かない。動いている間の確かめは [`super::watch`]。
//!
//! 手順 1〜3 と、4 のファイルの開き方・証明書の ACL・5 の読み方は、クライアント（ChronoGazer、
//! #644 PR3b）と同じコードを使うため共有 crate `banto_hub_tls_trust` にある（[`open_place`]・
//! [`open_checked_file`]・[`check_cert_exposure`]・[`read_checked`]。#644 PR3a）。ここに残るのは、
//! 鍵の確かめ（両方あること・[`exposure`]）・[`inspect_identity`]・対処の文言（[`Remedy`] から）。
//! 判定・順序・文言は移す前と同じ。
//!
//! 読んだ PEM のバイト列をそのまま TLS に渡す（[`DefaultHubTls`]。確かめた後にパスで開き直さ
//! ない）。鍵は [`Zeroizing`] で持ち、`Debug`・ログに出さない。

use std::path::{Path, PathBuf};

use banto_hub_tls_trust::{
    check_cert_exposure, ensure_service_revocations, exposure_fault, open_checked_file, open_place,
    peek_service_cert, read_checked, record_service_revocations, CertThumbprint, PlaceFault,
    Remedy, RevokedCert, RevokedCertPlace, ServiceRevocationFault, ServiceRevocationProblem,
    Unavailable, SERVICE_REVOCATIONS_DIR, SERVICE_REVOCATIONS_LABEL, TLS_LABEL,
};
use banto_safe_root::acl::{exposure, Exposure};
use zeroize::Zeroizing;

use super::{
    inspect_identity, reasons_text, utc_text, HubTlsOps, IdentityInfo, TlsFile, CERT_FILE,
    KEY_FILE, RENEW_FLAG,
};

/// サービスが既定で TLS を待ち受けるポート（127.0.0.1 だけ。#644 PR3a で共有 crate へ）。
pub use banto_hub_tls_trust::DEFAULT_TLS_PORT;

/// 確かめて読んだ、既定の TLS の鍵と証明書（PEM のバイト列）とポート。
#[derive(Clone)]
pub struct DefaultHubTls {
    port: u16,
    cert_pem: Vec<u8>,
    key_pem: Zeroizing<Vec<u8>>,
    cert_path: PathBuf,
    info: IdentityInfo,
}

impl std::fmt::Debug for DefaultHubTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultHubTls")
            .field("port", &self.port)
            .field("cert_path", &self.cert_path)
            .field("info", &self.info)
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

impl DefaultHubTls {
    /// PEM を [`inspect_identity`] で確かめてから持つ（`now_unix` で期限を見る）。テスト・
    /// 呼ぶ側の組み立て用で、置き場の確かめはしない（本番は [`load_default_identity`]）。
    pub fn from_checked_pem(
        port: u16,
        cert_pem: Vec<u8>,
        key_pem: impl Into<Zeroizing<Vec<u8>>>,
        cert_path: PathBuf,
        now_unix: i64,
    ) -> Result<Self, String> {
        let key_pem = key_pem.into();
        let info = inspect_identity(&cert_pem, &key_pem, now_unix)?;
        Ok(Self {
            port,
            cert_pem,
            key_pem,
            cert_path,
            info,
        })
    }

    /// 待ち受けるポート（本番は [`DEFAULT_TLS_PORT`]）。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// ポートを差し替える（テスト用。本番は [`DEFAULT_TLS_PORT`] のまま）。
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// 証明書の PEM。
    pub fn cert_pem(&self) -> &[u8] {
        &self.cert_pem
    }

    /// 証明書のパス（ログ用。読み直しには使わない）。
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// 証明書の SHA-256 指紋と notAfter。
    pub fn info(&self) -> &IdentityInfo {
        &self.info
    }

    /// TLS の待受に渡す（証明書、鍵）。鍵を複製しないよう取り出し、後には空を残す（ログの行の
    /// ための証明書のパス・指紋・期限は残る）。
    pub(crate) fn take_pem(&mut self) -> (Vec<u8>, Zeroizing<Vec<u8>>) {
        (
            std::mem::take(&mut self.cert_pem),
            std::mem::take(&mut self.key_pem),
        )
    }

    /// 待ち受けたときのログの 1 行。
    pub fn listening_line(&self, local: std::net::SocketAddr) -> String {
        format!(
            "banto-hub: 既定の TLS を https://{local} で待ち受けています（証明書 {}、SHA-256 指紋 {}、notAfter {}）",
            self.cert_path.display(),
            self.info.fingerprint,
            utc_text(self.info.not_after_unix)
        )
    }
}

/// 既定の TLS を使えない理由と対処（鍵の中身は含めない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultTlsUnavailable {
    /// 何が合わなかったか。
    pub reason: String,
    /// 管理者がすること。
    pub remedy: String,
}

impl DefaultTlsUnavailable {
    /// サービスログに出す行（警告として出す）。
    pub fn log_lines(&self) -> Vec<String> {
        vec![
            format!(
                "[WARN] banto-hub: 既定の TLS（https://127.0.0.1:{DEFAULT_TLS_PORT}）を待ち受けずに、TLS 無しで起動します: {}",
                self.reason
            ),
            format!("[WARN] banto-hub: 対処: {}", self.remedy),
        ]
    }
}

/// bind できなかったとき（オーナー決定 B）の、サービスログに出す行。
pub fn bind_failure_lines(port: u16, error: &str) -> Vec<String> {
    vec![
        format!(
            "[WARN] banto-hub: 既定の TLS（https://127.0.0.1:{port}）を待ち受けられないので、TLS 無しで起動します: {error}"
        ),
        format!(
            "[WARN] banto-hub: 対処: 127.0.0.1:{port} を使っている別のプロセス（netstat -ano | findstr :{port} で PID を確かめる）を止めてから、Hub のサービスを再起動してください。それまで、証明書を pin したクライアント（banto-hub-sink など）は Hub に繋がりません"
        ),
    ]
}

const ELEV: &str = "banto-hub-elev.exe prepare-hub-tls";

fn unavailable(reason: String, remedy: String) -> DefaultTlsUnavailable {
    DefaultTlsUnavailable { reason, remedy }
}

/// 無い・片方だけのときの対処。
fn prepare_remedy() -> String {
    format!("管理者の PowerShell で {ELEV} を実行してから、Hub のサービスを再起動してください")
}

/// 壊れている・期限切れ・鍵や証明書の露出のときの対処。
pub(super) fn renew_remedy() -> String {
    format!(
        "管理者の PowerShell で {ELEV} {RENEW_FLAG} を実行して作り直し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてから、Hub と Sink のサービスを再起動してください"
    )
}

/// `tls\` が露出・乗っ取りの恐れがあるときの対処。
fn recreate_dir_remedy(tls_path: &Path) -> String {
    format!(
        "{} の中身を確かめ、このフォルダを消してから、管理者の PowerShell で {ELEV} を実行し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてから、Hub と Sink のサービスを再起動してください",
        tls_path.display()
    )
}

/// 開けない・読めないなど、一時的かもしれないときの対処。
fn retry_remedy() -> String {
    "原因を取り除いてから Hub のサービスを再起動してください。続くときは、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して置き場を確かめてください".to_string()
}

/// ルートが保護されていないときの対処。
fn fix_root_remedy() -> String {
    "所有者を Administrators にし（takeown /a）、SYSTEM・Administrators 以外への削除・子の削除・ACL と所有者の変更・書き込み・フルの許可を外してから、Hub のサービスを再起動してください".to_string()
}

/// サービスの失効の記録の置き場が確かめに通らないときの対処（場面 26）。
fn fix_record_place_remedy(dir: &Path) -> String {
    format!(
        "{} の中身を確かめ、所有者を Administrators にし、SYSTEM・Administrators 以外への書き込みの許可を外して（または、このフォルダを消して）から、管理者の PowerShell で {ELEV} {RENEW_FLAG} を実行して記録と証明書を作り直し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてから、Hub と Sink のサービスを再起動してください",
        dir.display()
    )
}

/// サービスの失効の記録を使えないときの、理由と対処（#1121 PR2b）。
pub(super) fn from_record(
    found: ServiceRevocationProblem,
    root_path: &Path,
) -> DefaultTlsUnavailable {
    let remedy = match found.fault {
        ServiceRevocationFault::Untrusted => {
            fix_record_place_remedy(&root_path.join(SERVICE_REVOCATIONS_DIR))
        }
        ServiceRevocationFault::Broken => renew_remedy(),
        ServiceRevocationFault::Transient => retry_remedy(),
    };
    unavailable(
        format!(
            "{SERVICE_REVOCATIONS_LABEL}を使えないので、サービスの証明書を使いません: {}",
            found.reason
        ),
        remedy,
    )
}

/// 共有 crate の確かめの失敗を、サービスログの理由と対処にする（対処の文言は移す前と同じ）。
pub(super) fn from_trust(found: Unavailable) -> DefaultTlsUnavailable {
    let remedy = match &found.remedy {
        Remedy::Retry => retry_remedy(),
        Remedy::Prepare => prepare_remedy(),
        Remedy::FixRoot => fix_root_remedy(),
        Remedy::RecreateDir(tls_path) => recreate_dir_remedy(tls_path),
        Remedy::Renew => renew_remedy(),
    };
    unavailable(found.reason, remedy)
}

/// 秘密鍵の露出（[`exposure`] が空でない）の理由と対処。種類は [`exposure_fault`]（ハードリンク
/// だけなら失効に入れない）。
pub(super) fn key_exposure(
    key_path: &Path,
    key_open: &[Exposure],
) -> (DefaultTlsUnavailable, PlaceFault) {
    (
        unavailable(
            format!(
                "秘密鍵（{}）は保護されていません（{}）。ほかの利用者に読まれた・すり替えられた恐れがあります",
                key_path.display(),
                reasons_text(key_open)
            ),
            renew_remedy(),
        ),
        exposure_fault(key_open),
    )
}

/// 失効の記録に入れる、サービスの証明書の 1 件（種類は service、時刻は今）。
pub(super) fn service_revoked<O: HubTlsOps>(ops: &O, thumbprint: CertThumbprint) -> RevokedCert {
    RevokedCert {
        thumbprint,
        place: RevokedCertPlace::Service,
        recorded_at: u64::try_from(ops.now_unix()).unwrap_or(0),
    }
}

/// 起動のときに露出・所有者の違い（[`PlaceFault::revokes`]）を見た（#1121 PR2c、場面 8）: ディスク
/// 上の証明書の拇印を形だけ読めたら（[`peek_service_cert`]）、サービスの記録に書いてから断る。読め
/// なければ（ルートが通らない・証明書が無い・形が壊れている）書けるものが無いので何もしない。
/// 書けたか・書けなかったかは理由の文に足す（書けなくても TLS 無しで起動するのは同じ）。
///
/// `held` は、確かめに使った証明書のハンドルから取った拇印（ハンドルを開けていたら `Some`。読めな
/// ければ `Some(None)`）。開けていたなら必ずそれを使い、開き直さない（開き直す間に差し替えられた
/// 中身を記録に入れて、確かめた元の証明書を残さないため。Codex CLI P1）。開けなかった・無い・
/// ファイルを開く前の段階の露出（`None`）だけ、ディスクを形だけ読む（元を見ていないので取れる分）。
fn record_exposed_at_start<O: HubTlsOps>(
    ops: &O,
    mut found: DefaultTlsUnavailable,
    held: HeldThumbprint,
) -> DefaultTlsUnavailable {
    let Some(thumbprint) = held.unwrap_or_else(|| peek_service_cert(ops)) else {
        return found;
    };
    match record_service_revocations(ops, &[service_revoked(ops, thumbprint)]) {
        Ok(_) => found.reason.push_str(&format!(
            "。この証明書（拇印 {thumbprint}）はサービスの失効の記録に書きました（二度と使いません）"
        )),
        Err(problem) => found.reason.push_str(&format!(
            "。この証明書（拇印 {thumbprint}）をサービスの失効の記録に書けませんでした: {}",
            problem.reason
        )),
    }
    found
}

/// 確かめに使った証明書のハンドルから取った拇印（[`record_exposed_at_start`] の `held`）。
pub(super) type HeldThumbprint = Option<Option<CertThumbprint>>;

/// 握った証明書のハンドルから読んで拇印を取る（読めない・形が壊れていれば `None`）。
pub(super) fn held_thumbprint<F: TlsFile>(
    cert: &mut F,
    cert_path: &Path,
) -> Option<CertThumbprint> {
    let pem = read_checked(cert, "証明書", cert_path).ok()?;
    CertThumbprint::of_pem(&pem).ok()
}

/// 鍵・証明書それぞれの確かめの結果（[`load_default_identity`] の手順 4）。
enum Verdict {
    Fine,
    Missing,
    /// 開けなかった（理由, 失効に入れる種類か）。
    OpenFailed(DefaultTlsUnavailable, bool),
    /// 露出・所有者の違い（理由, 失効に入れる種類か）。
    Exposed(DefaultTlsUnavailable, bool),
}

impl Verdict {
    fn open_failed(found: Unavailable) -> Self {
        let revokes = found.fault.revokes();
        Verdict::OpenFailed(from_trust(found), revokes)
    }

    fn revokes(&self) -> bool {
        matches!(
            self,
            Verdict::OpenFailed(_, true) | Verdict::Exposed(_, true)
        )
    }
}

/// 鍵と証明書のどちらかに問題があったときの断り方。失効に入れる種類がどちらかにあれば、それを
/// 優先して記録に書いてから断る。無ければ、理由の優先は 開けない → 無い（片方だけ）→ 露出（鍵、
/// 証明書）。
fn refuse_pair<O: HubTlsOps>(
    ops: &O,
    verdicts: [Verdict; 2],
    key_path: &Path,
    cert_path: &Path,
    tls_path: &Path,
    held: HeldThumbprint,
) -> DefaultTlsUnavailable {
    let [key_verdict, cert_verdict] = verdicts;
    let key_missing = matches!(key_verdict, Verdict::Missing);
    let cert_missing = matches!(cert_verdict, Verdict::Missing);
    let mut verdicts = [key_verdict, cert_verdict];
    if let Some(i) = verdicts.iter().position(Verdict::revokes) {
        if let Verdict::OpenFailed(found, _) | Verdict::Exposed(found, _) =
            std::mem::replace(&mut verdicts[i], Verdict::Fine)
        {
            return record_exposed_at_start(ops, found, held);
        }
    }
    let [key_verdict, cert_verdict] = verdicts;
    let (mut opens, mut exposed) = (Vec::new(), Vec::new());
    for verdict in [key_verdict, cert_verdict] {
        match verdict {
            Verdict::OpenFailed(found, _) => opens.push(found),
            Verdict::Exposed(found, _) => exposed.push(found),
            Verdict::Fine | Verdict::Missing => {}
        }
    }
    if !opens.is_empty() {
        return opens.remove(0);
    }
    if key_missing && cert_missing {
        return unavailable(
            format!(
                "{TLS_LABEL}（{}）に鍵（{KEY_FILE}）と証明書（{CERT_FILE}）がありません",
                tls_path.display()
            ),
            prepare_remedy(),
        );
    }
    if key_missing || cert_missing {
        let (have, missing) = if cert_missing {
            (key_path, cert_path)
        } else {
            (cert_path, key_path)
        };
        return unavailable(
            format!(
                "{} はありますが {} がありません（片方だけ）",
                have.display(),
                missing.display()
            ),
            renew_remedy(),
        );
    }
    exposed.remove(0)
}

/// 既定の TLS の鍵と証明書を、置き場ごと確かめてから読み、サービスの失効の記録と照合する
/// （モジュール doc「確かめの順序」）。`tls\` には書き込み・作成をしない。記録が無ければ空で作る。
/// 露出・所有者の違いを見たら、ディスク上の証明書の拇印を記録に書いてから断る（#1121 PR2c）。
pub fn load_default_identity<O: HubTlsOps>(
    ops: &O,
) -> Result<DefaultHubTls, DefaultTlsUnavailable> {
    // --- 1〜3. ルートと置き場（共有 crate。モジュール doc「確かめの順序」） ----------
    // 露出・所有者の違いなら、記録に書いてから断る（#1121 PR2c）。
    let refuse = |found: Unavailable| {
        let revokes = found.fault.revokes();
        let found = from_trust(found);
        if revokes {
            record_exposed_at_start(ops, found, None)
        } else {
            found
        }
    };
    let place = open_place(ops).map_err(refuse)?;
    let tls_path = place.tls_path().to_path_buf();

    // --- 4. 鍵と証明書（開いて握り、ACL を確かめる） ---------------------------------
    let key_path = tls_path.join(KEY_FILE);
    let cert_path = tls_path.join(CERT_FILE);
    // 鍵と証明書は独立に確かめる（片方の失効に入れない問題・無い・開けない、が、もう片方の
    // 失効に入れる露出を隠さない）。握るのは、問題が無かったときだけ。
    let (key, key_verdict) = match open_checked_file(ops, &place, KEY_FILE, "秘密鍵") {
        Ok(Some(key)) => {
            let key_open = exposure(&key.security());
            if key_open.is_empty() {
                (Some(key), Verdict::Fine)
            } else {
                let (found, fault) = key_exposure(&key_path, &key_open);
                (None, Verdict::Exposed(found, fault.revokes()))
            }
        }
        Ok(None) => (None, Verdict::Missing),
        Err(found) => (None, Verdict::open_failed(found)),
    };
    // 開けた証明書のハンドルから取った拇印（露出していたとき。鍵のほうが露出のときは下で取る）。
    let mut held: HeldThumbprint = None;
    let (cert, cert_verdict) = match open_checked_file(ops, &place, CERT_FILE, "証明書") {
        Ok(Some(mut cert)) => match check_cert_exposure(&cert, &cert_path) {
            Ok(()) => (Some(cert), Verdict::Fine),
            Err(found) => {
                let revokes = found.fault.revokes();
                if revokes {
                    held = Some(held_thumbprint(&mut cert, &cert_path));
                }
                (None, Verdict::Exposed(from_trust(found), revokes))
            }
        },
        Ok(None) => (None, Verdict::Missing),
        Err(found) => (None, Verdict::open_failed(found)),
    };
    let (mut key, mut cert) = match (key, cert) {
        (Some(key), Some(cert)) => (key, cert),
        (key, mut cert) => {
            // 握った証明書のハンドルから拇印を取ってから放し、記録に書く。
            if held.is_none() {
                if let Some(cert) = cert.as_mut() {
                    held = Some(held_thumbprint(cert, &cert_path));
                }
            }
            drop((key, cert, place));
            return Err(refuse_pair(
                ops,
                [key_verdict, cert_verdict],
                &key_path,
                &cert_path,
                &tls_path,
                held,
            ));
        }
    };

    // --- 5. 握ったハンドルから読んで、中身を確かめる --------------------------------
    let key_pem = read_checked(&mut key, "秘密鍵", &key_path)
        .map(Zeroizing::new)
        .map_err(from_trust)?;
    let cert_pem = read_checked(&mut cert, "証明書", &cert_path).map_err(from_trust)?;
    drop((key, cert));
    let now = ops.now_unix();
    let loaded = DefaultHubTls::from_checked_pem(
        DEFAULT_TLS_PORT,
        cert_pem,
        key_pem,
        cert_path.clone(),
        now,
    )
    .map_err(|error| {
        unavailable(
            format!(
                "{TLS_LABEL}（{}）の鍵と証明書は使えません: {error}",
                tls_path.display()
            ),
            renew_remedy(),
        )
    })?;

    // --- 6. サービスの失効の記録（#1121 PR2b。場面 25・26） ------------------------------
    // `inspect_identity` を通った PEM なので、拇印は取れる（取れなければ使わない側に倒す）。
    let thumbprint = CertThumbprint::of_pem(loaded.cert_pem()).map_err(|error| {
        unavailable(
            format!(
                "証明書（{}）の拇印を取れませんでした: {error}",
                cert_path.display()
            ),
            renew_remedy(),
        )
    })?;
    let (record, _created) = ensure_service_revocations(ops, &place)
        .map_err(|found| from_record(found, place.root_path()))?;
    if record.contains(&thumbprint) {
        return Err(unavailable(
            format!(
                "証明書（{}、拇印 {thumbprint}）は{SERVICE_REVOCATIONS_LABEL}（{}）の失効の記録にあります（露出を見た証明書は二度と使いません）",
                cert_path.display(),
                place.root_path().join(SERVICE_REVOCATIONS_DIR).display()
            ),
            renew_remedy(),
        ));
    }
    Ok(loaded)
}
