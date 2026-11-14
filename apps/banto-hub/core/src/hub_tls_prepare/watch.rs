//! サービスの Hub の、動いている間の周期の確かめ（#1121 PR2c。docs/hub-desktop-tls-design.md
//! §4.4.1「照合する時点」のサービスの Hub、§4.4.2 の場面 8・25・26、§7「PR2 で決めたこと」1〜4・
//! 「PR2c で決めたこと」）。
//!
//! 起動のときに [`super::load::load_default_identity`] が確かめて 127.0.0.1:8723 で待ち受けた
//! 証明書（拇印 = [`ServiceTlsWatch::new`] の `serving`）について、サービス本体（host の
//! `win_service.rs`）が [`SERVICE_TLS_RECHECK_INTERVAL`]（30 秒）ごとに [`ServiceTlsWatch::check`]
//! を呼ぶ。判定はここ（[`HubTlsOps`] を引数にした関数で、偽物で全 OS で確かめる）、8723 を止める
//! のは呼ぶ側（`RunningHub::tls_revoker`）。
//!
//! ## 1 周期の手順
//!
//! 1. **置き場の確かめをやり直す**（起動のときと同じ手順: ルート・`tls\`・鍵・証明書。`tls\` には
//!    書かない）。種類は共有 crate の [`PlaceFault`] で分ける:
//!    - **露出**（信頼する相手以外への許可）と**所有者の違い**は失効に入れる（オーナー決定 1）。
//!      入れる拇印は、いま待ち受けている証明書の拇印（必ず）と、ディスク上の証明書が形だけ
//!      読めて（[`peek_service_cert`]）別の拇印ならそれも。種類は service。
//!    - **それ以外**（無い・リンク・ハードリンク・場所の食い違い・一時的な失敗）は失効に入れない。
//!      いま待ち受けている証明書は起動のときにメモリ上で確かめ済みなので保ち、警告をログに出す
//!      （同じ理由が続く間は繰り返さない。理由が変わったら出す）。
//! 2. **記録に書く**（[`record_service_revocations`]）: 健全な記録には和で置き換え、無ければ
//!    置き換えずに作る。書けなかったら（オーナー決定 3）拇印をプロセスの中に覚えて使わず
//!    （8723 は止める）、周期ごとに書き直しを試み、毎周期ログに出す。
//! 3. **記録と照合する**（[`read_service_revocations`]）: いま待ち受けている証明書の拇印が記録に
//!    あれば止める。記録が使えないとき、置き場が通らない（Untrusted）・形が壊れている（Broken）は
//!    起動のときと同じく閉じる側（止める）。一時的（Transient）は保って毎周期警告する。
//!
//! 止めると決めた周期の結果は [`WatchRound::stop`]。止めたらそのプロセスでは再開しない
//! （[`ServiceTlsWatch`] は止めた印を持ち続ける）。止めた後も、書けていない拇印があれば書き直しを
//! 続け、書き終えたら [`WatchRound::finished`]（呼ぶ側は周期を終えてよい）。
//!
//! 起動のときに記録を作るだけの挙動・環境変数（`BANTO_HUB_TLS_*`）がある場合（周期の確かめは
//! しない）・コンソールとデスクトップの挙動は変えない。

use std::collections::BTreeSet;
use std::time::Duration;

use banto_hub_tls_trust::{
    check_cert_exposure, open_checked_file, open_place, peek_service_cert,
    read_service_revocations, record_service_revocations, CertThumbprint, ServiceRevocationFault,
    Unavailable, DEFAULT_TLS_PORT, PRODUCT_DIR,
};
use banto_safe_root::acl::exposure;
use banto_safe_root::guard::MachineEnvironment;

/// 周期の確かめが持つ拇印の型（host が共有 crate に直接依存しないための再公開）。
pub use banto_hub_tls_trust::CertThumbprint as ServingThumbprint;

use super::load::{from_record, from_trust, key_exposure, service_revoked, DefaultTlsUnavailable};
use super::{HubTlsOps, TlsFile, CERT_FILE, KEY_FILE, RENEW_FLAG};

/// 周期の確かめの間隔（ChronoGazer の見張りと同じ 30 秒。テストでは呼ぶ側が短くする）。
pub const SERVICE_TLS_RECHECK_INTERVAL: Duration = Duration::from_secs(30);

/// 1 周期のログの行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchLine {
    /// 知らせ（記録に書けた）。
    Info(String),
    /// 警告（保つ・止める・書けない）。
    Warn(String),
}

impl WatchLine {
    /// 行の文字列。
    pub fn text(&self) -> &str {
        match self {
            Self::Info(line) | Self::Warn(line) => line,
        }
    }
}

/// 1 周期の結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatchRound {
    /// この周期で 8723 の TLS を止めると決めた（呼ぶ側が止める。前の周期で止めていれば偽）。
    pub stop: bool,
    /// 止めた後で、書けていない拇印も無い（呼ぶ側は周期を終えてよい）。
    pub finished: bool,
    /// ログの行（サービスログへ）。
    pub lines: Vec<WatchLine>,
}

/// 置き場の確かめの結果（[`check_place`]）。
enum PlaceSeen {
    /// 通った。
    Fine,
    /// 露出・所有者の違い（失効に入れる）。
    Revokes(DefaultTlsUnavailable),
    /// それ以外で通らない（失効に入れず、保つ）。
    Keeps(String),
}

fn seen_of(found: Unavailable) -> PlaceSeen {
    let revokes = found.fault.revokes();
    let found = from_trust(found);
    if revokes {
        PlaceSeen::Revokes(found)
    } else {
        PlaceSeen::Keeps(found.reason)
    }
}

/// 起動のときと同じ手順で、置き場（ルート・`tls\`・鍵・証明書）を確かめ直す。中身は読まない
/// （待ち受けている証明書はメモリ上で確かめ済み）。書き込み・作成はしない。
fn check_place<O: HubTlsOps>(ops: &O) -> PlaceSeen {
    let place = match open_place(ops) {
        Ok(place) => place,
        Err(found) => return seen_of(found),
    };
    let tls_path = place.tls_path().to_path_buf();
    let key = match open_checked_file(ops, &place, KEY_FILE, "秘密鍵") {
        Ok(key) => key,
        Err(found) => return seen_of(found),
    };
    let cert = match open_checked_file(ops, &place, CERT_FILE, "証明書") {
        Ok(cert) => cert,
        Err(found) => return seen_of(found),
    };
    let (Some(key), Some(cert)) = (key, cert) else {
        return PlaceSeen::Keeps(format!(
            "{}（{}）に鍵（{KEY_FILE}）か証明書（{CERT_FILE}）がありません",
            banto_hub_tls_trust::TLS_LABEL,
            tls_path.display()
        ));
    };
    let key_open = exposure(&key.security());
    if !key_open.is_empty() {
        let (found, fault) = key_exposure(&tls_path.join(KEY_FILE), &key_open);
        return if fault.revokes() {
            PlaceSeen::Revokes(found)
        } else {
            PlaceSeen::Keeps(found.reason)
        };
    }
    match check_cert_exposure(&cert, &tls_path.join(CERT_FILE)) {
        Ok(()) => PlaceSeen::Fine,
        Err(found) => seen_of(found),
    }
}

/// 止めるときのログの行（理由と対処）。
fn stop_lines(reason: &str, remedy: &str) -> Vec<WatchLine> {
    vec![
        WatchLine::Warn(format!(
            "[WARN] banto-hub: 既定の TLS（https://127.0.0.1:{DEFAULT_TLS_PORT}）を止め、その証明書で張られていた接続を切ります（このプロセスでは再開しません）: {reason}"
        )),
        WatchLine::Warn(format!("[WARN] banto-hub: 対処: {remedy}")),
    ]
}

/// サービスの Hub の周期の確かめの状態（モジュール doc）。
#[derive(Debug)]
pub struct ServiceTlsWatch {
    /// いま 8723 で待ち受けている証明書の拇印。
    serving: CertThumbprint,
    /// 露出を見たのに、まだ記録に書けていない拇印（プロセスの中に覚えて使わない。オーナー決定 3）。
    unwritten: BTreeSet<CertThumbprint>,
    /// 失効で止めると決めた（そのプロセスでは再開しない）。
    stopped: bool,
    /// 最後に出した「保つ」の警告（同じ理由を繰り返さない）。
    last_keep: Option<String>,
}

impl ServiceTlsWatch {
    /// 起動のときに確かめて待ち受けた証明書の拇印から始める。
    pub fn new(serving: CertThumbprint) -> Self {
        Self {
            serving,
            unwritten: BTreeSet::new(),
            stopped: false,
            last_keep: None,
        }
    }

    /// いま待ち受けている証明書の拇印。
    pub fn serving(&self) -> CertThumbprint {
        self.serving
    }

    /// 失効で止めると決めたか。
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// まだ記録に書けていない拇印（昇順）。
    pub fn unwritten(&self) -> Vec<CertThumbprint> {
        self.unwritten.iter().copied().collect()
    }

    /// 1 周期の確かめ（モジュール doc「1 周期の手順」）。
    pub fn check<O: HubTlsOps>(&mut self, ops: &O) -> WatchRound {
        let mut lines = Vec::new();
        // (理由, 対処)。止めると決めたときだけ `Some`。
        let mut stop: Option<(String, String)> = None;

        // --- 1. 置き場 -------------------------------------------------------------------
        match check_place(ops) {
            PlaceSeen::Fine => self.last_keep = None,
            PlaceSeen::Keeps(reason) => {
                if !self.stopped && self.last_keep.as_deref() != Some(reason.as_str()) {
                    lines.push(WatchLine::Warn(format!(
                        "[WARN] banto-hub: 既定の TLS の置き場を確かめ直せませんでした。待ち受けている証明書（拇印 {}）は起動のときに確かめたものなので、使い続けます: {reason}",
                        self.serving
                    )));
                }
                self.last_keep = Some(reason);
            }
            PlaceSeen::Revokes(found) => {
                self.last_keep = None;
                self.unwritten.insert(self.serving);
                if let Some(disk) = peek_service_cert(ops) {
                    self.unwritten.insert(disk);
                }
                stop = Some((
                    format!(
                        "{}（露出を見た証明書は失効の記録に入れ、二度と使いません）",
                        found.reason
                    ),
                    found.remedy,
                ));
            }
        }

        // --- 2. 書けていない拇印を記録に書く ---------------------------------------------
        let mut written = None;
        if !self.unwritten.is_empty() {
            let entries: Vec<_> = self
                .unwritten
                .iter()
                .map(|thumbprint| service_revoked(ops, *thumbprint))
                .collect();
            let shown = self
                .unwritten
                .iter()
                .map(|thumbprint| thumbprint.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            match record_service_revocations(ops, &entries) {
                Ok(record) => {
                    lines.push(WatchLine::Info(format!(
                        "banto-hub: 露出を見た証明書（拇印 {shown}）をサービスの失効の記録に書きました"
                    )));
                    self.unwritten.clear();
                    written = Some(record);
                }
                Err(problem) => {
                    lines.push(WatchLine::Warn(format!(
                        "[WARN] banto-hub: 露出を見た証明書（拇印 {shown}）をサービスの失効の記録に書けませんでした。このプロセスの中に覚えて使わず、次の周期に書き直します: {}",
                        problem.reason
                    )));
                    // 書けていない拇印に待ち受けている証明書があれば、使わない（止める）。
                    if stop.is_none() && self.unwritten.contains(&self.serving) {
                        stop = Some((
                            format!(
                                "露出を見た証明書（拇印 {}）を失効の記録に書けていません",
                                self.serving
                            ),
                            renew_remedy_text(),
                        ));
                    }
                }
            }
        }

        // --- 3. 記録と照合する -----------------------------------------------------------
        if !self.stopped && stop.is_none() {
            let revoked = match written {
                Some(record) => Ok(record.contains(&self.serving)),
                None => read_service_revocations(ops).map(|found| found.is_revoked(&self.serving)),
            };
            match revoked {
                Ok(true) => {
                    stop = Some((
                        format!(
                            "証明書（拇印 {}）はサービスの失効の記録にあります（露出を見た証明書は二度と使いません）",
                            self.serving
                        ),
                        renew_remedy_text(),
                    ))
                }
                Ok(false) => {}
                Err(problem) if problem.fault == ServiceRevocationFault::Transient => {
                    lines.push(WatchLine::Warn(format!(
                        "[WARN] banto-hub: サービスの失効の記録を確かめられませんでした。待ち受けている証明書（拇印 {}）を使い続け、次の周期にやり直します: {}",
                        self.serving, problem.reason
                    )));
                }
                Err(problem) => {
                    // 対処の文言に記録の置き場のパスを出すため（取れなければ既定の書き方）。
                    let root = ops
                        .machine_env()
                        .program_data()
                        .map(|program_data| program_data.join(PRODUCT_DIR))
                        .unwrap_or_else(|_| std::path::PathBuf::from("%ProgramData%\\BantoHub"));
                    let found = from_record(problem, &root);
                    stop = Some((found.reason, found.remedy));
                }
            }
        }

        let newly = !self.stopped && stop.is_some();
        if let (true, Some((reason, remedy))) = (newly, stop) {
            self.stopped = true;
            lines.extend(stop_lines(&reason, &remedy));
        }
        WatchRound {
            stop: newly,
            finished: self.stopped && self.unwritten.is_empty(),
            lines,
        }
    }
}

/// 作り直しの対処（`--renew` → クライアントの pin の書き換え → 再起動）。
fn renew_remedy_text() -> String {
    let remedy = super::load::renew_remedy();
    debug_assert!(remedy.contains(RENEW_FLAG));
    remedy
}
