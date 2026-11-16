//! #644 PR3b・#1121 PR4a: ChronoGazer が Hub と話すときの TLS の決まり（[`HubTlsPolicy`]）。
//!
//! # 何を塞ぐか
//!
//! 同じ PC の Hub が止まっている間に、その loopback のポートを別のプロセスが先に取ると、
//! 平文の http ではその相手に試運転の発行の要求や API キーが届いてしまう。そこで
//! **接続先が loopback のときは、Hub が自分の置き場に置いた証明書を pin した https でだけ
//! 話す**（2026-11-13 オーナー決定）。
//!
//! | 接続先                                   | 本番（[`HubTlsPolicy::production`]・[`HubTlsPolicy::service_only`]） |
//! | ---------------------------------------- | -------------------------------------------------------- |
//! | loopback（127.0.0.0/8・`::1`・`localhost`）の http | 断る（[`LOOPBACK_HTTP_REFUSED`] の案内。何も送らない）   |
//! | loopback の https                        | 置き場の証明書を確かめて読み、その 1〜2 枚だけを信じる   |
//! | それ以外（LAN）の http                   | 今までどおり pin なし（#644 の範囲外）                   |
//! | それ以外の https                         | 今までどおり断る（pin が無いので bootstrap が `InvalidEndpoint`） |
//!
//! IPv6 の loopback（`::1`、IPv4-mapped の `::ffff:127.x.x.x`）も loopback として扱う（Hub は `::1` では
//! 待ち受けないので https は繋がらないだけ）。それ以外の IPv6 は LAN 扱い。
//!
//! # 2 つの置き場（#1121 PR4a、docs/hub-desktop-tls-design.md §4.4）
//!
//! Hub は動かし方で証明書の置き場が違う。サービスは `%ProgramData%\BantoHub\tls\`、
//! デスクトップ版は利用者ごとの `%LOCALAPPDATA%\BantoHub\tls\`。どちらも 127.0.0.1:8723 で
//! 待ち受け、利用者は両者を行き来するので、ChronoGazer デスクトップは**両方の置き場を読み**、
//! 確かめに通って失効していない証明書（1〜2 枚）を pin にする。相手がどちらかを当てには
//! しない（別のアカウントのプロセスはどちらの鍵も持たないので、両方を pin にしても通らない）。
//!
//! | 置き場の状態             | pin      | 警告                              |
//! | ------------------------ | -------- | --------------------------------- |
//! | 無い                     | 外す     | 出さない（片方だけの PC は普通）  |
//! | あるが確かめに通らない   | 入れない | **接続できても**出す              |
//! | 失効の記録にある・露出を見た | 入れない | **接続できても**出す          |
//! | 通って失効していない     | 入れる   | 出さない                          |
//!
//! 両方とも使えなければ [`BootstrapErrorKind::HubTls`] で断る（何も送らない）。警告は
//! [`HubTlsPolicy::warnings_for`] で取り、`crate::hub` が `HubView::tls_warnings` に載せる。
//!
//! **失効の照合**（設計 §4.4.1「照合する時点」）: サービスの証明書は、サービスの記録と利用者の
//! 記録（A・B の和。形が読めた分）の両方と、利用者の証明書は利用者の記録と照合する。露出・
//! 所有者の違いを見た証明書は、ACL が直っても二度と使わない: 拇印を利用者の記録に書き足し
//! （サービスの証明書の露出も利用者の記録に書く。サービスの記録には書かない）、書けなければ
//! プロセスの中に覚えて使わず、周期ごとに書き直しを試みる。
//!
//! **利用者の記録を整える手順の結果**で、利用者の証明書を使ってよいかが決まる（§4.4.1
//! 「手順の結果は 3 つ」）:
//!
//! | 最後の結果                 | まだ pin に使っていない利用者の証明書 | すでに pin に使っている利用者の証明書 |
//! | -------------------------- | ------------------------------------- | ------------------------------------- |
//! | 整っている                 | 使い始める                            | 使い続ける                            |
//! | 通らない                   | 使わない                              | すぐ外す                              |
//! | この回はできなかった       | 使わない                              | 保つ（警告）。`INCOMPLETE_LIMIT`（3）周期続いたら外す |
//! | まだ 1 回も走らせていない  | 使わない                              | （起きない）                          |
//!
//! サービスの証明書は、この結果で止めない（利用者の記録を失っても、サービスの証明書は
//! 使い続けて警告を出す）。
//!
//! `chronogazer-server`（[`HubTlsPolicy::service_only`]）は LocalSystem で動き、利用者の置き場を
//! 持たない。サービスの置き場とサービスの記録だけを読み、利用者の記録は読まず書かない。露出を
//! 見た拇印はプロセスの間だけ覚えて使わない（§4.4.2 の場面 10）。
//!
//! # いつ読むか
//!
//! - **1 回の操作ごと**（`banto_hub_bootstrap::HubTlsSource::pins_for` と、購読の世代の目印
//!   [`HubTlsPolicy::marker_for`]）: 置き場の確かめ（所有者・ACL・露出）と証明書の読み直し、
//!   失効の記録のロック無しの読みを、**そのたびに**やり直す。管理者が `prepare-hub-tls --renew`
//!   で作り直しても、Hub のデスクトップ版がトレイで作り直しても、ChronoGazer の再起動は
//!   要らない。前の呼び出しで読んだ証明書は覚えない。
//! - **見張りの周期と起動の初回**（[`HubTlsPolicy::refresh`]・[`HubTlsPolicy::ensure_started`]。
//!   `crate::hub` の `supervise_once` と操作の入口が呼ぶ）: 利用者の記録を整える手順と、露出を
//!   見た拇印の書き足し。起動の初回は、最後の結果が「整っている」になるまで、操作の入口ごとに
//!   走らせ直す。どちらも記録のロックを上限付き（5 秒）で待つので、操作ごとの判定
//!   からは呼ばず、blocking のスレッドで走らせる。起動の初回にその回で消失を直したら、続けて
//!   もう 1 回走らせる（直した回の結果は「通らない」で、次の回に「整っている」になるため。
//!   設計 §7「PR2 で決めたこと」2）。保存済み・入力された接続先が loopback の https のときだけ
//!   走らせる（Hub を使わない利用者の `%LOCALAPPDATA%` に記録を作らない）。
//!
//! 購読は目印が変わったときに張り直す（`crate::hub` の `reconcile_generation` と、pin した
//! 世代の目印を毎周期確かめる `supervise_once`）。目印は**置き場ごとの値の組**（サービス・
//! 利用者それぞれ: 無い／使える証明書の拇印／通らない／失効）の hash で、PEM の連結の hash に
//! しない（順序や有無の変化を取り違えないため）。失効の記録だけが足された・ACL だけが
//! 変わったときも、PEM が同じまま目印が変わる。
//!
//! # 覚えるもの
//!
//! 決まりは `Clone` の間で 1 つの状態を共有する（`State`）: 最後の整える手順の結果、
//! 「この回はできなかった」の連続数、いま pin に使っている利用者の証明書の拇印、露出を見た
//! 拇印（使わない）と、そのうち記録に書けていないもの、いまの警告。**証明書そのものと
//! 目印は覚えない**（別の接続先や失敗の呼び出しで上書きされた値と比べないため）。
//!
//! # 保存済みの loopback の http
//!
//! 以前の版で保存した `http://127.0.0.1:8722` などは**書き換えない**（オーナー決定）。
//! 状態の確認・購読の開始はこの決まりで断られ、画面に [`LOOPBACK_HTTP_REFUSED`] の案内が
//! 出る。https は port も違うので keyring の口座が別になり、接続し直すと新しいキーが
//! 発行される（それでよい）。
//!
//! # 案内の文言
//!
//! 直し方は Hub の動かし方ごとに分ける。利用者の置き場は「Hub のデスクトップ版を起動する／
//! トレイの『TLS の証明書を作り直す』」、サービスの置き場は「管理者の PowerShell で
//! `banto-hub-elev.exe prepare-hub-tls`（`--renew`）」。置き場がどちらも無いときは、ChronoGazer
//! デスクトップではデスクトップ版を主に案内する。ChronoGazer デスクトップだけがサービスの
//! 証明書の露出を見たときは、管理者に `prepare-hub-tls --renew` を頼むよう案内する（サービスと
//! ほかの利用者へは伝わらないため。設計 §4.4.1「保証の範囲」）。
//!
//! # テストと開発用
//!
//! [`HubTlsPolicy::plain_loopback_http_for_dev`] は loopback の http を pin なしで受ける。
//! 模擬 Hub（127.0.0.1 の http）を相手にするテスト・実機確認のハーネス・開発用の
//! `banto-serve`（配布しない。E2E が `BANTO_HUB_LOOPBACK_HTTP=1` で選ぶ）だけが使う。
//! 製品の host は固定の決まりを渡し（デスクトップは [`HubTlsPolicy::production`]、
//! `chronogazer-server` は [`HubTlsPolicy::service_only`]）、設定や環境変数で切り替える口を
//! 持たない。置き場を読む口（`places::HubTlsPlaces`）はテストで偽物に差し替える。

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};

use banto_hub_bootstrap::{
    Error as BootstrapError, ErrorKind as BootstrapErrorKind, HubTlsSource, Url,
};
use banto_hub_tls_trust::{CertThumbprint, ReconcileOutcome, RevokedCertPlace};
use banto_tagclient::PinnedHubTls;

#[cfg(test)]
pub(crate) mod fake;
mod places;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use places::HubCertReader;
use places::{CertRead, HubTlsPlaces, ServiceRecordRead};

/// 同じ PC の Hub の接続先。画面の例と案内に使う。
pub const LOOPBACK_HUB_ENDPOINT: &str = "https://127.0.0.1:8723";

/// 保存済み・入力された loopback の http を断るときの案内（ChronoGazer デスクトップ。キーや
/// 秘密は含まない）。
pub const LOOPBACK_HTTP_REFUSED: &str = "同じ PC の Hub へは http では接続しません（API キーを守るため）。https://127.0.0.1:8723 で接続し直してください（Hub のデスクトップ版を起動してあるか、Hub をサービスで動かして管理者で prepare-hub-tls を済ませてあること）。";

/// [`LOOPBACK_HTTP_REFUSED`] の `chronogazer-server` 向け（サービスの Hub だけが相手）。
pub const LOOPBACK_HTTP_REFUSED_SERVICE: &str = "同じ PC の Hub へは http では接続しません（API キーを守るため）。https://127.0.0.1:8723 で接続し直してください（Hub をサービスで動かし、管理者で prepare-hub-tls を済ませてあること）。";

/// 「この回はできなかった」がこの周期数続いたら、pin に使っている利用者の証明書を外す
/// （数えるのは見張りの周期だけ。操作の入口の走らせ直しでは進めない。設計 §4.4.1。Hub のデスクトップ版の見張りと同じ 3 周期 = 約 90 秒）。
pub(crate) const INCOMPLETE_LIMIT: u8 = 3;

const ELEV_RENEW: &str = "banto-hub-elev.exe prepare-hub-tls --renew";

/// 案内の文言の向き（モジュール doc「案内の文言」）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Host {
    /// ChronoGazer デスクトップ（Hub のデスクトップ版を主に案内する）。
    Desktop,
    /// `chronogazer-server`（Hub のサービスだけを案内する）。
    Service,
}

/// 目印に入れる、置き場ごとの値（モジュール doc「いつ読むか」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    /// この決まりでは読まない置き場（`chronogazer-server` の利用者の置き場）。
    NotRead,
    /// 無い。
    Absent,
    /// 確かめに通って失効しておらず、pin に使う証明書。
    Usable(CertThumbprint),
    /// あるが使わない（確かめに通らない・記録を確かめられない・整える手順の結果で止めている）。
    Unusable,
    /// 失効した（記録にある・露出を見た）。
    Revoked,
}

/// 最後に走らせた、利用者の記録を整える手順。
struct ReconcileSeen {
    outcome: ReconcileOutcome,
    /// その回の警告（起動のときに 2 回走らせたら、両方の分）。
    warnings: Vec<String>,
}

/// `Clone` の間で共有する状態（モジュール doc「覚えるもの」）。
#[derive(Default)]
struct State {
    /// 最後の整える手順（まだ走らせていなければ `None`）。
    reconcile: Option<ReconcileSeen>,
    /// 「この回はできなかった」の連続数。
    incomplete_streak: u8,
    /// いま pin に使っている利用者の証明書。
    user_in_use: Option<CertThumbprint>,
    /// 露出・所有者の違いを見た証明書（プロセスの間は使わない）。
    remembered: BTreeSet<CertThumbprint>,
    /// そのうち、まだ利用者の記録に書けていないもの（周期ごとに書き直しを試みる）。
    unwritten: BTreeMap<CertThumbprint, RevokedCertPlace>,
    /// いまの警告（最後の判定のもの）。
    warnings: Vec<String>,
    /// ログに出した警告（同じ理由が続く間は繰り返さない）。
    logged: BTreeSet<String>,
}

/// loopback を pin した https だけにする決まりの中身。
struct Pinned {
    places: Arc<dyn HubTlsPlaces>,
    /// 利用者の置き場と利用者の記録を読み書きするか（`chronogazer-server` はしない）。
    reads_user: bool,
    host: Host,
    state: Mutex<State>,
}

/// 1 回の判定の結果。
struct Evaluation {
    /// pin にする証明書の PEM（0〜2 枚）。
    pems: Vec<String>,
    /// 置き場ごとの値の組。
    slots: (Slot, Slot),
    /// 使える証明書が無いときに断る理由。
    refusal: Option<String>,
}

impl Evaluation {
    fn marker(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.slots.hash(&mut hasher);
        hasher.finish()
    }
}

/// 置き場 1 つの判定の途中経過。
struct Judged {
    slot: Slot,
    pem: Option<String>,
    warning: Option<String>,
}

impl Judged {
    fn absent() -> Self {
        Self {
            slot: Slot::Absent,
            pem: None,
            warning: None,
        }
    }

    fn unused(slot: Slot, warning: String) -> Self {
        Self {
            slot,
            pem: None,
            warning: Some(warning),
        }
    }
}

fn sentence(reason: &str, remedy: &str) -> String {
    if remedy.is_empty() {
        reason.to_owned()
    } else {
        format!("{reason}。{remedy}")
    }
}

/// 確かめに通った PEM を、pin にできる形（UTF-8・証明書がちょうど 1 つ・秘密鍵なし）かまで
/// 見て、拇印を付けて返す。
fn pinnable(pem: Vec<u8>) -> Option<(String, CertThumbprint)> {
    let thumbprint = CertThumbprint::of_pem(&pem).ok()?;
    let text = String::from_utf8(pem).ok()?;
    PinnedHubTls::from_pem(std::slice::from_ref(&text)).ok()?;
    Some((text, thumbprint))
}

impl Pinned {
    fn new(places: Arc<dyn HubTlsPlaces>, reads_user: bool, host: Host) -> Arc<Self> {
        Arc::new(Self {
            places,
            reads_user,
            host,
            state: Mutex::new(State::default()),
        })
    }

    /// 状態のロック。握ったまま置き場を読まない（読むのは必ずロックの外）。毒は無視する
    /// （状態は足すだけ・上書きするだけの値で、途中で止まっても閉じる側にしか倒れない）。
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn http_refused(&self) -> &'static str {
        match self.host {
            Host::Desktop => LOOPBACK_HTTP_REFUSED,
            Host::Service => LOOPBACK_HTTP_REFUSED_SERVICE,
        }
    }

    /// 露出・所有者の違いを見た拇印を覚える。利用者の記録を持つ決まりでは、まだ記録に無ければ
    /// 書き足しの待ちにも入れる。
    fn remember(
        &self,
        state: &mut State,
        thumbprint: CertThumbprint,
        place: RevokedCertPlace,
        recorded: &BTreeSet<CertThumbprint>,
    ) {
        state.remembered.insert(thumbprint);
        if self.reads_user && !recorded.contains(&thumbprint) {
            state.unwritten.entry(thumbprint).or_insert(place);
        }
    }

    /// 判定の本体。置き場をこの呼び出しで読み直し（ロックを待つ操作はしない）、状態と
    /// 合わせて pin・目印・警告を決める。
    fn evaluate(&self) -> Evaluation {
        // 置き場を読む（状態のロックの外）。
        let service_cert = self.places.service_cert();
        let service_record =
            matches!(service_cert, CertRead::Usable(_)).then(|| self.places.service_revocations());
        let (user_cert, user_record) = if self.reads_user {
            (
                Some(self.places.user_cert()),
                self.places.user_revocations(),
            )
        } else {
            (None, BTreeSet::new())
        };

        let mut state = self.state();
        // 記録に書けていた拇印は、書き足しの待ちから外す（ほかのプロセスが書いた場合を含む）。
        state
            .unwritten
            .retain(|thumbprint, _| !user_record.contains(thumbprint));

        let service = self.judge_service(&mut state, service_cert, service_record, &user_record);
        let user = match user_cert {
            Some(cert) => self.judge_user(&mut state, cert, &user_record),
            None => Judged {
                slot: Slot::NotRead,
                pem: None,
                warning: None,
            },
        };

        let mut warnings: Vec<String> = [&service, &user]
            .into_iter()
            .filter_map(|judged| judged.warning.clone())
            .collect();
        if self.reads_user {
            warnings.extend(reconcile_warnings(&state));
            if !state.unwritten.is_empty() {
                warnings.push(format!(
                    "露出・所有者の異常を見た証明書（拇印 {}）を、失効の記録に書けていません。この ChronoGazer が動いている間は使いませんが、アクセス許可を直すだけでは漏れた鍵は無効になりません。その証明書の Hub を止め、鍵と証明書を作り直してください",
                    state
                        .unwritten
                        .keys()
                        .map(CertThumbprint::to_string)
                        .collect::<Vec<_>>()
                        .join("・")
                ));
            }
        }

        // 同じ証明書が両方の置き場にあっても 1 枚（pin の枠は 2）。
        let mut pems: Vec<String> = Vec::new();
        for pem in [&service.pem, &user.pem].into_iter().flatten() {
            if !pems.contains(pem) {
                pems.push(pem.clone());
            }
        }
        let refusal = pems
            .is_empty()
            .then(|| self.refusal_text(service.slot, user.slot, &warnings));

        // 警告はログにも出す（同じ理由が続く間は繰り返さない）。
        for warning in &warnings {
            if !state.logged.contains(warning) {
                eprintln!("banto: Hub の TLS: {warning}");
            }
        }
        state.logged = warnings.iter().cloned().collect();
        state.warnings = warnings;

        Evaluation {
            pems,
            slots: (service.slot, user.slot),
            refusal,
        }
    }

    fn judge_service(
        &self,
        state: &mut State,
        cert: CertRead,
        record: Option<ServiceRecordRead>,
        user_record: &BTreeSet<CertThumbprint>,
    ) -> Judged {
        const PLACE: &str = "Hub のサービスの証明書";
        let pem = match cert {
            CertRead::Absent => return Judged::absent(),
            CertRead::Unusable(found) => {
                let Some(thumbprint) = found.revoke else {
                    return Judged::unused(
                        Slot::Unusable,
                        format!(
                            "{PLACE}を使っていません: {}",
                            sentence(&found.reason, &found.remedy)
                        ),
                    );
                };
                self.remember(state, thumbprint, RevokedCertPlace::Service, user_record);
                return Judged::unused(
                    Slot::Revoked,
                    format!(
                        "{PLACE}（拇印 {thumbprint}）に露出・所有者の異常を見たため、今後は使いません: {}。{}",
                        found.reason,
                        self.service_exposure_remedy()
                    ),
                );
            }
            CertRead::Usable(pem) => pem,
        };
        let Some((text, thumbprint)) = pinnable(pem) else {
            return Judged::unused(
                Slot::Unusable,
                format!(
                    "{PLACE}（PEM）を pin にできないため使っていません。管理者の PowerShell で {ELEV_RENEW} を実行して作り直し、Hub のサービスを再起動してください"
                ),
            );
        };
        let (in_service_record, record_problem) = match record {
            Some(ServiceRecordRead::Revoked(revoked)) => (revoked.contains(&thumbprint), None),
            Some(ServiceRecordRead::Unusable { reason, salvaged }) => {
                (salvaged.contains(&thumbprint), Some(reason))
            }
            // 使える証明書があるときは必ず記録を読んでいる。読んでいなければ閉じる側。
            None => (
                false,
                Some("サービスの失効の記録を読んでいません".to_owned()),
            ),
        };
        if in_service_record {
            return Judged::unused(
                Slot::Revoked,
                format!(
                    "{PLACE}（拇印 {thumbprint}）はサービスの失効の記録にあるため使いません。管理者の PowerShell で {ELEV_RENEW} を実行して作り直し、Hub のサービスを再起動してください"
                ),
            );
        }
        if state.remembered.contains(&thumbprint) || user_record.contains(&thumbprint) {
            return Judged::unused(
                Slot::Revoked,
                format!(
                    "{PLACE}（拇印 {thumbprint}）は、露出・所有者の異常を見た証明書なので使いません。{}",
                    self.service_exposure_remedy()
                ),
            );
        }
        if let Some(reason) = record_problem {
            return Judged::unused(
                Slot::Unusable,
                format!(
                    "サービスの失効の記録を確かめられないため、{PLACE}を使っていません: {reason}。続くときは、管理者が置き場の所有者・アクセス許可を直してから、管理者の PowerShell で {ELEV_RENEW} を実行して記録と証明書を作り直し、Hub のサービスを再起動してください"
                ),
            );
        }
        Judged {
            slot: Slot::Usable(thumbprint),
            pem: Some(text),
            warning: None,
        }
    }

    /// サービスの証明書の露出を、サービスより先に ChronoGazer が見たときの案内。ChronoGazer
    /// デスクトップの記録は、サービスとほかの利用者へ伝わらない（設計 §4.4.1「保証の範囲」）。
    fn service_exposure_remedy(&self) -> String {
        match self.host {
            Host::Desktop => format!(
                "アクセス許可を直すだけでは漏れた鍵は無効になりません。この PC の管理者に、管理者の PowerShell で {ELEV_RENEW} を実行して作り直し、Hub のサービスを再起動するよう頼んでください"
            ),
            Host::Service => format!(
                "アクセス許可を直すだけでは漏れた鍵は無効になりません。管理者の PowerShell で {ELEV_RENEW} を実行して作り直し、Hub のサービスを再起動してください"
            ),
        }
    }

    fn judge_user(
        &self,
        state: &mut State,
        cert: CertRead,
        user_record: &BTreeSet<CertThumbprint>,
    ) -> Judged {
        const PLACE: &str = "Hub のデスクトップ版の証明書";
        const RENEW: &str = "Hub のデスクトップ版で、TLS フォルダと鍵・証明書の所有者・アクセス許可を確かめ、異常があれば直してから、トレイの「TLS の証明書を作り直す」で作り直してください（失効の記録は消さないでください）";
        let judged = self.judge_user_cert(state, cert, user_record, PLACE, RENEW);
        // pin に使わなかった回は「使っている証明書」を忘れる。戻すときは「使い始める」の
        // 条件（整える手順の結果が「整っている」）をもう一度通す。
        state.user_in_use = match judged.slot {
            Slot::Usable(thumbprint) => Some(thumbprint),
            _ => None,
        };
        judged
    }

    fn judge_user_cert(
        &self,
        state: &mut State,
        cert: CertRead,
        user_record: &BTreeSet<CertThumbprint>,
        place: &str,
        renew: &str,
    ) -> Judged {
        let pem = match cert {
            CertRead::Absent => return Judged::absent(),
            CertRead::Unusable(found) => {
                let Some(thumbprint) = found.revoke else {
                    return Judged::unused(
                        Slot::Unusable,
                        format!(
                            "{place}を使っていません: {}",
                            sentence(&found.reason, &found.remedy)
                        ),
                    );
                };
                self.remember(state, thumbprint, RevokedCertPlace::User, user_record);
                return Judged::unused(
                    Slot::Revoked,
                    format!(
                        "{place}（拇印 {thumbprint}）に露出・所有者の異常を見たため、今後は使いません: {}。アクセス許可を直すだけでは漏れた鍵は無効になりません。{renew}",
                        found.reason
                    ),
                );
            }
            CertRead::Usable(pem) => pem,
        };
        let Some((text, thumbprint)) = pinnable(pem) else {
            return Judged::unused(
                Slot::Unusable,
                format!("{place}（PEM）を pin にできないため使っていません。{renew}"),
            );
        };
        if state.remembered.contains(&thumbprint) || user_record.contains(&thumbprint) {
            return Judged::unused(
                Slot::Revoked,
                format!("{place}（拇印 {thumbprint}）は失効しているため使いません。{renew}"),
            );
        }
        let held = state.user_in_use == Some(thumbprint);
        let withheld = |why: &str| {
            Judged::unused(
                Slot::Unusable,
                format!("利用者の失効の記録{why}ため、{place}を使っていません"),
            )
        };
        match state.reconcile.as_ref().map(|seen| seen.outcome) {
            Some(ReconcileOutcome::Consistent) => {}
            Some(ReconcileOutcome::Incomplete)
                if held && state.incomplete_streak < INCOMPLETE_LIMIT => {}
            Some(ReconcileOutcome::Incomplete) if state.incomplete_streak >= INCOMPLETE_LIMIT => {
                return withheld(&format!(
                    "を {INCOMPLETE_LIMIT} 周期続けて確かめられなかった"
                ))
            }
            Some(ReconcileOutcome::Incomplete) => {
                return withheld("をまだ確かめられていない（整うまで使い始めません）")
            }
            Some(ReconcileOutcome::Refused) => {
                return withheld("が確かめに通らない（消された・壊れた・置き場が通らない）")
            }
            None => return withheld("をまだ確かめていない（確かめが済むまで使い始めません）"),
        }
        Judged {
            slot: Slot::Usable(thumbprint),
            pem: Some(text),
            warning: None,
        }
    }

    /// 使える証明書が無いときの理由と直し方。
    fn refusal_text(&self, service: Slot, user: Slot, warnings: &[String]) -> String {
        let service_absent = service == Slot::Absent;
        let user_absent = matches!(user, Slot::Absent | Slot::NotRead);
        if service_absent && user_absent {
            return match self.host {
                Host::Desktop => "同じ PC の Hub の証明書がまだ無いため接続しません。Hub のデスクトップ版を起動してください（起動のときに証明書が作られます）。Hub をサービスで動かしている PC では、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行してから、Hub のサービスを再起動してください".to_owned(),
                Host::Service => "同じ PC の Hub の証明書がまだ無いため接続しません。Hub をサービスで動かし、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行してから、Hub のサービスを再起動してください".to_owned(),
            };
        }
        format!(
            "同じ PC の Hub の証明書を使えないため接続しません: {}",
            warnings.join(" / ")
        )
    }

    /// 利用者の記録を整え、露出を見た拇印を書き足す（モジュール doc「いつ読むか」）。記録の
    /// ロックを上限付きで待つので、blocking のスレッドから呼ぶ。`counts_cycle` は見張りの
    /// 周期の回か: 「できなかった」の連続数（`INCOMPLETE_LIMIT`）を進めるのは周期の回だけ
    /// （操作の入口の走らせ直しは増やさず、「できなかった」以外なら 0 に戻す）。
    fn refresh_blocking(&self, startup: bool, counts_cycle: bool) {
        let mut report = self.places.reconcile_user_revocations();
        let mut warnings = std::mem::take(&mut report.warnings);
        if startup && report.saw_loss {
            // 消失を直した回の結果は「通らない」。直っていれば、次の回が「整っている」になる。
            report = self.places.reconcile_user_revocations();
            warnings.append(&mut report.warnings);
        }
        {
            let mut state = self.state();
            state.incomplete_streak = if report.outcome != ReconcileOutcome::Incomplete {
                0
            } else if counts_cycle {
                state.incomplete_streak.saturating_add(1)
            } else {
                state.incomplete_streak
            };
            state.reconcile = Some(ReconcileSeen {
                outcome: report.outcome,
                warnings,
            });
        }
        // 置き場の確かめ → 記録との照合（露出を見た拇印はここで待ちに入る）。
        self.evaluate();
        let pending = self.state().unwritten.clone();
        if pending.is_empty() {
            return;
        }
        for (thumbprint, place) in pending {
            let written = self
                .places
                .record_user_revocation(thumbprint, place)
                .revoked
                .contains(&thumbprint);
            if written {
                self.state().unwritten.remove(&thumbprint);
            }
        }
        // 書けた・書けなかったを警告に反映する。
        self.evaluate();
    }
}

/// 整える手順の結果から出す警告（結果が「整っている」で警告も無ければ空）。
fn reconcile_warnings(state: &State) -> Vec<String> {
    let Some(seen) = &state.reconcile else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    match seen.outcome {
        ReconcileOutcome::Consistent => {}
        ReconcileOutcome::Refused => warnings.push(
            "利用者の失効の記録が確かめに通りません（消された・壊れた・置き場が通らない）。直るまで、Hub のデスクトップ版の証明書は使いません（Hub のサービスの証明書の照合は続けます）".to_owned(),
        ),
        ReconcileOutcome::Incomplete if state.incomplete_streak < INCOMPLETE_LIMIT => {
            warnings.push(format!(
                "利用者の失効の記録の確かめが終わりませんでした（{} 周期続いています。{INCOMPLETE_LIMIT} 周期続くと、Hub のデスクトップ版の証明書を pin から外します）",
                state.incomplete_streak
            ))
        }
        // 続く間は同じ文言にする（周期ごとにログへ繰り返さない）。
        ReconcileOutcome::Incomplete => warnings.push(format!(
            "利用者の失効の記録の確かめが、{INCOMPLETE_LIMIT} 周期以上続けて終わっていません（終わるまで、Hub のデスクトップ版の証明書は使いません）"
        )),
    }
    warnings.extend(seen.warnings.iter().cloned());
    warnings
}

#[derive(Clone)]
enum Loopback {
    /// 本番: loopback は pin した https だけ。
    Pinned(Arc<Pinned>),
    /// テスト・開発用: loopback の http を pin なしで受ける（https の loopback は pin が
    /// 無いので bootstrap の規則で断られる）。
    PlainHttp,
}

/// ChronoGazer の Hub 接続の TLS の決まり（モジュール doc）。`Clone` は同じ決まりで、状態
/// （モジュール doc「覚えるもの」）を共有する。
#[derive(Clone)]
pub struct HubTlsPolicy {
    loopback: Loopback,
}

impl HubTlsPolicy {
    /// ChronoGazer デスクトップの決まり: loopback は、Hub のサービスの置き場
    /// （`%ProgramData%\BantoHub\tls\`）と今の利用者の置き場（`%LOCALAPPDATA%\BantoHub\tls\`）の
    /// 証明書を pin した https だけ、それ以外は今までどおり。
    pub fn production() -> Self {
        Self::pinned(Arc::new(places::SystemPlaces), true, Host::Desktop)
    }

    /// `chronogazer-server` の決まり: loopback は、Hub のサービスの置き場の証明書を pin した
    /// https だけ。利用者の置き場と利用者の失効の記録は読まず、書かない（LocalSystem には
    /// 「今の利用者の Hub のデスクトップ版」が無い。設計 §4.4.1「照合する時点」）。
    pub fn service_only() -> Self {
        Self::pinned(Arc::new(places::SystemPlaces), false, Host::Service)
    }

    /// **テスト・開発用**（モジュール doc「テストと開発用」）: loopback の http も pin なしで
    /// 受ける。製品の host（デスクトップ・`chronogazer-server`）から呼ばない。
    pub fn plain_loopback_http_for_dev() -> Self {
        Self {
            loopback: Loopback::PlainHttp,
        }
    }

    /// サービスの置き場の証明書の読み方だけを差し替える（テスト用。#644 PR3b の形）。
    /// 利用者の置き場は読まず、失効の記録は空。
    #[cfg(test)]
    pub(crate) fn with_reader(reader: Arc<dyn HubCertReader>) -> Self {
        Self::pinned(Arc::new(places::ReaderPlaces(reader)), false, Host::Desktop)
    }

    /// [`Self::production`] と同じ決まりで、置き場を読む口だけを差し替える（テスト用）。
    #[cfg(test)]
    pub(crate) fn two_places_with(places: Arc<dyn HubTlsPlaces>) -> Self {
        Self::pinned(places, true, Host::Desktop)
    }

    /// [`Self::service_only`] と同じ決まりで、置き場を読む口だけを差し替える（テスト用）。
    #[cfg(test)]
    pub(crate) fn service_only_with(places: Arc<dyn HubTlsPlaces>) -> Self {
        Self::pinned(places, false, Host::Service)
    }

    fn pinned(places: Arc<dyn HubTlsPlaces>, reads_user: bool, host: Host) -> Self {
        Self {
            loopback: Loopback::Pinned(Pinned::new(places, reads_user, host)),
        }
    }

    /// `endpoint` が、置き場の証明書を pin にする接続先（loopback の https）なら、その決まりの
    /// 中身。
    fn pinned_for(&self, endpoint: &str) -> Option<&Arc<Pinned>> {
        let Loopback::Pinned(pinned) = &self.loopback else {
            return None;
        };
        let base = Url::parse(endpoint.trim()).ok()?;
        (is_loopback(&base) && base.scheme() == "https").then_some(pinned)
    }

    /// `endpoint` に今使う pin の目印（置き場ごとの値の組の hash。モジュール doc「いつ
    /// 読むか」）を、**この呼び出しで置き場を確かめ直して**返す。pin を使わない接続先（LAN の
    /// http・開発用の決まり）は `Ok(None)`、断る接続先・使える証明書が無いときは
    /// [`HubTlsSource::pins_for`] と同じ `Err`。
    ///
    /// 購読の世代の同一性に混ぜ、証明書が作り直された・失効した・確かめに通らなくなったら
    /// 張り直す（`crate::hub` の `reconcile_generation`・`supervise_once`）。前の呼び出しの
    /// 結果を覚えて共有する作りにはしない - 別の接続先や失敗の呼び出しで上書きされた値と
    /// 比べてしまうため。
    ///
    /// 解釈できない綴りは `Ok(None)`（その接続先は bootstrap が `InvalidEndpoint` で断る）。
    pub(crate) fn marker_for(&self, endpoint: &str) -> Result<Option<u64>, BootstrapError> {
        let Ok(base) = Url::parse(endpoint.trim()) else {
            return Ok(None);
        };
        Ok(self.decide(&base)?.map(|(_, marker)| marker))
    }

    /// `endpoint` への接続について、いま出ている警告（片方の置き場が通らない・失効した・
    /// 失効の記録が消された・直された・確かめが終わらない）。**接続できている間も**出す
    /// （片方が良いとき、もう片方の問題が誰にも見えなくなるのを防ぐ。設計 §4.4）。最後の
    /// 判定（操作・見張りの周期）のもので、この呼び出しでは置き場を読まない。置き場の証明書を
    /// pin にしない接続先では空。
    pub(crate) fn warnings_for(&self, endpoint: &str) -> Vec<String> {
        self.pinned_for(endpoint)
            .map(|pinned| pinned.state().warnings.clone())
            .unwrap_or_default()
    }

    /// 起動の初回の整える手順（モジュール doc「いつ読むか」）。`endpoint` が loopback の https
    /// で、利用者の記録を持つ決まりで、まだ 1 回も走らせていないか、最後の結果が「整っている」
    /// でないか、露出を見た拇印の書き足しが残っている（`unwritten`）ときだけ走らせる
    /// （「できなかった」「通らない」で終わった回・書けなかった失効は、次の操作でやり直す。
    /// やり直しは「できなかった」の連続数を進めない: 3 周期は見張りの周期で数える）。操作の
    /// 入口（`crate::hub` の `begin_operation`・`connect`・`adopt_manual_key`）が、Hub と話す
    /// 前に呼ぶ - 走らせる前の判定は、利用者の証明書を使い始めない。
    pub(crate) async fn ensure_started(&self, endpoint: &str) {
        self.run_refresh(endpoint, true).await;
    }

    /// 見張りの周期の整える手順と、露出を見た拇印の書き足し（`crate::hub` の
    /// `supervise_once`）。まだ 1 回も走らせていなければ、起動の初回として走らせる。
    pub(crate) async fn refresh(&self, endpoint: &str) {
        self.run_refresh(endpoint, false).await;
    }

    async fn run_refresh(&self, endpoint: &str, only_first: bool) {
        let Some(pinned) = self.pinned_for(endpoint) else {
            return;
        };
        if !pinned.reads_user {
            return;
        }
        let (started, settled) = {
            let state = pinned.state();
            (
                state.reconcile.is_some(),
                state
                    .reconcile
                    .as_ref()
                    .is_some_and(|seen| seen.outcome == ReconcileOutcome::Consistent)
                    && state.unwritten.is_empty(),
            )
        };
        // 起動の初回は「整っている」かつ書き足しが残っていない状態になるまで走らせ直す
        // （保存済みの接続が無い PC では見張りが `refresh` を呼ばないので、ロックの待ち切れで
        // 終わった回や、記録に書けなかった失効を再起動まで引きずらない）。
        if only_first && settled {
            return;
        }
        let pinned = Arc::clone(pinned);
        // 記録のロックを最大 5 秒待つので、tokio のワーカーを塞がない。
        let ran =
            tokio::task::spawn_blocking(move || pinned.refresh_blocking(!started, !only_first))
                .await;
        if let Err(error) = ran {
            // 状態は書き換わっていない（1 回も走っていなければ、利用者の証明書は使い始めない）。
            eprintln!(
                "banto: Hub の TLS: 利用者の失効の記録の確かめが途中で止まりました（次の周期でやり直します）: {error}"
            );
        }
    }

    /// 判定の入口（置き場を読むのは loopback の https のときだけ）。
    fn decide(&self, base: &Url) -> Result<Option<(PinnedHubTls, u64)>, BootstrapError> {
        if !is_loopback(base) {
            // LAN: http は pin なし（範囲外）。https は pin が無いので bootstrap が断る。
            return Ok(None);
        }
        let pinned = match &self.loopback {
            Loopback::PlainHttp => return Ok(None),
            Loopback::Pinned(pinned) => pinned,
        };
        if base.scheme() != "https" {
            return Err(refused(pinned.http_refused()));
        }
        let evaluation = pinned.evaluate();
        if let Some(reason) = evaluation.refusal.clone() {
            return Err(refused(reason));
        }
        // 1 枚ずつは pin にできると確かめてある（`pinnable`）。
        let pins = PinnedHubTls::from_pem(&evaluation.pems).map_err(|_| {
            refused("同じ PC の Hub の証明書（PEM）を pin にできなかったため接続しません")
        })?;
        Ok(Some((pins, evaluation.marker())))
    }
}

impl HubTlsSource for HubTlsPolicy {
    fn pins_for(&self, base: &Url) -> banto_hub_bootstrap::Result<Option<PinnedHubTls>> {
        Ok(self.decide(base)?.map(|(pins, _)| pins))
    }
}

fn refused(reason: impl Into<String>) -> BootstrapError {
    BootstrapError::with_detail(BootstrapErrorKind::HubTls, reason)
}

/// 接続先が loopback か（127.0.0.0/8、`::1`、IPv4-mapped の `::ffff:127.x.x.x`、
/// `localhost`・`*.localhost`、末尾の `.` は無視）。`Url` は `127.1` のような書き方も
/// `127.0.0.1` に正規化してから渡ってくる。
fn is_loopback(base: &Url) -> bool {
    let Some(host) = base.host_str() else {
        return false;
    };
    // IPv6 は `host_str` が角括弧つきで返す（`url::Host::Ipv6` と同じ判定になる）。
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().is_ok_and(|ip| {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        });
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_loopback();
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}
