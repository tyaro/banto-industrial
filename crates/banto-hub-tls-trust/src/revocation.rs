//! 利用者の失効の記録（#1121 PR1b。docs/hub-desktop-tls-design.md §4.4.1・§4.4.2）: 記録の形
//! （[`RevocationRecord`]）、ロック無しの照合の読み口（[`read_user_revocations`]）、整える手順
//! （[`reconcile_user_revocations`]）、失効を書き足す口（[`record_user_revocation`]）。Hub の
//! デスクトップ版と ChronoGazer デスクトップが同じコードを使う。
//!
//! 考え方は設計の 3 つ: (1) 整える手順は 1 つだけで、同じ利用者のどのプロセスが何度走らせても
//! 同じ結果になる。(2) 拇印は増えるだけで、2 つの記録（A・B）は和で照合する。(3) 記録が欠けて
//! いたら、その場で「今ある利用者の証明書を失効に入れて、記録を直す」。
//!
//! ## この module が書くとき
//!
//! - 書くのは、置き場が PR1a の確かめ（[`open_user_place`]）に通った直後に、**その確かめで
//!   握ったフォルダのハンドル基準**でだけ（パスで開き直さない）。フォルダが無ければ、確かめ
//!   済みのいちばん深いフォルダのハンドル基準で 1 つ作り、**起点から確かめ直す**。
//! - 記録は「同じフォルダの一時ファイル → 書き切る → 置き換え」（[`RevocationOps::write_atomic`]）。
//!   残った一時ファイルは読まずに片付ける（ロックを握っている間だけ。握っていない書き手の
//!   一時ファイルを消さないため）。
//! - 判定と書き込みは、A・B の外のロックファイルの**排他の open**
//!   （[`RevocationOps::open_child_file_exclusive`]）を握っている間だけ。上限付きで待つ
//!   （[`ReconcileOptions`]）。名前付き mutex は使わない。
//! - 確かめに通らない置き場には書かない。ロックが取れない・通らないなら何も書かない。
//!
//! ## 3 周期の扱いは呼ぶ側
//!
//! ここが返すのは 1 回の手順の結果（[`ReconcileOutcome`]）と警告の理由だけ。「この回は
//! できなかった」が何周期続いたら止めるか（設計では 3 周期）は呼ぶ側（PR2 の Hub、PR4 の
//! ChronoGazer）が数える。

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use banto_safe_root::guard;

use crate::user::{fault_of, read_unchecked, user_reasons_text, UncheckedRead};
use crate::{
    check_place_layout, load_pinned_user_cert, open_user_file, open_user_place,
    parse_single_certificate, user_place_problems, CheckedUserPlace, FileOpenError, ReadError,
    TlsFile, UncheckedUserCert, UserCertPin, UserFault, UserFileRole, UserPlaceKind,
    UserPlaceProblem, UserPlaceState, UserRemedy, UserTlsOps, UserTlsPlace,
};

/// 失効の記録のファイル名（A・B のフォルダの中で同じ名前）。
pub const REVOCATIONS_FILE: &str = "revocations.txt";

/// 記録の 1 行目（形の名前と版）。
pub const REVOCATIONS_HEADER: &str = "banto-hub-revocations 1";

/// 記録の大きさの上限（読むとき・書くとき）。1 件は 90 バイトほどなので、約 2900 件。
pub const MAX_REVOCATIONS_BYTES: usize = 256 * 1024;

/// ロックを待つ上限の既定（§4.4.1「ロックは上限付きで待つ」）。
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(5);

/// ロックを試し直す間隔の既定。
pub const DEFAULT_LOCK_POLL: Duration = Duration::from_millis(50);

/// 文言の中の記録の呼び名。
const RECORD_LABEL: &str = "失効の記録";

// --- 拇印 -----------------------------------------------------------------------------

/// 証明書の拇印（DER の SHA-256）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CertThumbprint([u8; 32]);

impl CertThumbprint {
    /// 証明書の DER から。
    pub fn of_der(der: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        Self(Sha256::digest(der).into())
    }

    /// 証明書がちょうど 1 つ入った PEM から（[`parse_single_certificate`] と同じ読み方）。
    pub fn of_pem(cert_pem: &[u8]) -> Result<Self, String> {
        parse_single_certificate(cert_pem).map(|der| Self::of_der(&der))
    }

    /// 32 バイトそのまま。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 小文字の 16 進 64 文字。
    pub fn to_hex(&self) -> String {
        self.to_string()
    }

    /// 小文字の 16 進ちょうど 64 文字から（大文字・長さ違い・ほかの文字は断る）。
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.as_bytes();
        if hex.len() != 64 {
            return None;
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = nibble(hex[index * 2])? << 4 | nibble(hex[index * 2 + 1])?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for CertThumbprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for CertThumbprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertThumbprint({self})")
    }
}

// --- 記録の形 -------------------------------------------------------------------------

/// 失効した証明書がどの置き場のものか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokedCertPlace {
    /// 利用者の `tls\`（デスクトップ版の Hub の証明書）。
    User,
    /// サービスの `tls\`（その利用者の ChronoGazer が露出を見たサービスの証明書）。
    Service,
}

impl RevokedCertPlace {
    fn word(self) -> &'static str {
        match self {
            RevokedCertPlace::User => "user",
            RevokedCertPlace::Service => "service",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "user" => Some(RevokedCertPlace::User),
            "service" => Some(RevokedCertPlace::Service),
            _ => None,
        }
    }
}

/// 失効した証明書 1 件（拇印・どの置き場の証明書か・書いた時刻）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokedCert {
    /// 拇印。
    pub thumbprint: CertThumbprint,
    /// どの置き場の証明書か。
    pub place: RevokedCertPlace,
    /// 記録に入れた時刻（UNIX 秒。表示用で、判定には使わない）。
    pub recorded_at: u64,
}

/// 記録の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// 準備中（初期化の途中。なるのは A だけ）。
    Pending,
    /// 確定。
    Committed,
}

/// 失効の記録 1 つ（A か B のファイルの中身）。
///
/// ファイルは ASCII の行（区切りは `\n` だけ、最後の行も `\n` で終わる）:
///
/// ```text
/// banto-hub-revocations 1
/// state committed
/// revoked <拇印: 小文字の 16 進 64 文字> <user|service> <UNIX 秒>
/// end <revoked の行の数>
/// ```
///
/// `state` は `pending` か `committed`。`revoked` の行は拇印の昇順で、同じ拇印は 1 行だけ。
/// 最後の `end` の行が、途中で切れた中身を読み違えないための印。読むときは形を厳しく取り
/// （[`RevocationRecord::parse`]）、少しでも違えば「形が壊れている」として扱う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationRecord {
    state: RecordState,
    entries: Vec<RevokedCert>,
}

impl RevocationRecord {
    /// `entries` を拇印の昇順に並べて作る（同じ拇印は最初の 1 件だけ残す）。
    pub fn new(state: RecordState, entries: impl IntoIterator<Item = RevokedCert>) -> Self {
        let mut by_thumbprint = BTreeMap::new();
        for entry in entries {
            by_thumbprint.entry(entry.thumbprint).or_insert(entry);
        }
        Self {
            state,
            entries: by_thumbprint.into_values().collect(),
        }
    }

    /// 状態。
    pub fn state(&self) -> RecordState {
        self.state
    }

    /// 失効した証明書（拇印の昇順）。
    pub fn entries(&self) -> &[RevokedCert] {
        &self.entries
    }

    /// 拇印が入っているか。
    pub fn contains(&self, thumbprint: &CertThumbprint) -> bool {
        self.entries
            .binary_search_by(|entry| entry.thumbprint.cmp(thumbprint))
            .is_ok()
    }

    /// ファイルの中身にする。
    pub fn encode(&self) -> Vec<u8> {
        let state = match self.state {
            RecordState::Pending => "pending",
            RecordState::Committed => "committed",
        };
        let mut text = format!("{REVOCATIONS_HEADER}\nstate {state}\n");
        for entry in &self.entries {
            text.push_str(&format!(
                "revoked {} {} {}\n",
                entry.thumbprint,
                entry.place.word(),
                entry.recorded_at
            ));
        }
        text.push_str(&format!("end {}\n", self.entries.len()));
        text.into_bytes()
    }

    /// ファイルの中身から（形が少しでも違えば理由を返す）。
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        // 数字だけ（符号・空白・先頭の 0 の飾りを受けない）。
        fn number(word: &str) -> Option<u64> {
            let plain = !word.is_empty()
                && word.bytes().all(|b| b.is_ascii_digit())
                && (word == "0" || !word.starts_with('0'));
            plain.then(|| word.parse().ok()).flatten()
        }

        if bytes.len() > MAX_REVOCATIONS_BYTES {
            return Err("大きすぎます".to_string());
        }
        let text = std::str::from_utf8(bytes)
            .ok()
            .filter(|text| text.is_ascii())
            .ok_or("ASCII の文字だけではありません")?;
        let body = text
            .strip_suffix('\n')
            .ok_or("最後の行が改行で終わっていません")?;
        let mut lines = body.split('\n');
        if lines.next() != Some(REVOCATIONS_HEADER) {
            return Err("1 行目が形の名前と版ではありません".to_string());
        }
        let state = match lines.next() {
            Some("state pending") => RecordState::Pending,
            Some("state committed") => RecordState::Committed,
            _ => return Err("2 行目が状態ではありません".to_string()),
        };
        let mut entries: Vec<RevokedCert> = Vec::new();
        let mut ended = false;
        for line in lines {
            if ended {
                return Err("end の行の後に続きがあります".to_string());
            }
            let words: Vec<&str> = line.split(' ').collect();
            match words.as_slice() {
                ["revoked", thumbprint, place, recorded_at] => {
                    let entry = RevokedCert {
                        thumbprint: CertThumbprint::from_hex(thumbprint)
                            .ok_or("拇印が小文字の 16 進 64 文字ではありません")?,
                        place: RevokedCertPlace::from_word(place)
                            .ok_or("置き場の種類が user・service ではありません")?,
                        recorded_at: number(recorded_at).ok_or("時刻が数字ではありません")?,
                    };
                    if entries
                        .last()
                        .is_some_and(|last| last.thumbprint >= entry.thumbprint)
                    {
                        return Err("拇印が昇順に並んでいません".to_string());
                    }
                    entries.push(entry);
                }
                ["end", count] => {
                    if number(count) != Some(entries.len() as u64) {
                        return Err("end の行の件数が合いません".to_string());
                    }
                    ended = true;
                }
                _ => return Err("読めない行があります".to_string()),
            }
        }
        if !ended {
            return Err("end の行がありません（途中で切れています）".to_string());
        }
        Ok(Self { state, entries })
    }
}

// --- OS の操作 ------------------------------------------------------------------------

/// 失効の記録を書くために OS に対してする操作（[`UserTlsOps`] に足すもの）。どれも、
/// [`open_user_place`] が確かめて握ったフォルダの**ハンドル基準**に、名前 1 要素で辿らずに行う
/// （パスで開き直さない）。Windows の実装は `crate::win::WindowsTlsTrustOps`（部品は
/// `banto_safe_root`）、テストは偽物。
pub trait RevocationOps: UserTlsOps {
    /// `parent` の直下にフォルダ `name` を作る（記述子は親から継承。既にあれば何もせず `Ok`）。
    /// 作った後の確かめは呼ぶ側（起点から [`open_user_place`] をやり直す）。
    fn create_child_dir(&self, parent: &Self::Dir, name: &str) -> Result<(), String>;

    /// `dir` の中の `name` を、中身ごと原子的に置き換える: 同じフォルダの一時ファイルに書く →
    /// 書き切る（flush）→ 置き換え。途中で止まっても、前の中身か新しい中身のどちらかが残る。
    fn write_atomic(&self, dir: &Self::Dir, name: &str, contents: &[u8]) -> Result<(), String>;

    /// `dir` の中に残った、`name` の一時ファイルを読まずに消す。
    fn remove_stale_temps(&self, dir: &Self::Dir, name: &str) -> Result<(), String>;

    /// `dir` の直下に空のファイル `name` を作る（記述子は親から継承。既にあれば何もせず `Ok`）。
    fn create_file_if_absent(&self, dir: &Self::Dir, name: &str) -> Result<(), String>;

    /// `dir` の中の `name` を**排他**に辿らずに開く（無ければ `Ok(None)`）。返したものを持って
    /// いる間は、ほかの誰もこの口で開けない（[`FileOpenError::InUse`]）。drop で放す。
    fn open_child_file_exclusive(
        &self,
        dir: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError>;

    /// ロックを試し直す前に待つ。
    fn sleep(&self, duration: Duration);

    /// 今の時刻（UNIX 秒。記録に入れる「書いた時刻」）。
    fn now_unix_seconds(&self) -> u64;
}

// --- 照合の読み口 ----------------------------------------------------------------------

/// 利用者の記録の 2 つの側。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationSide {
    /// 記録 A（既定は `%LOCALAPPDATA%\BantoHub\revocations\`）。
    A,
    /// 記録 B（既定は `%LOCALAPPDATA%\BantoHubRevocations\`）。
    B,
}

impl RevocationSide {
    /// 両方。
    pub const BOTH: [RevocationSide; 2] = [RevocationSide::A, RevocationSide::B];

    /// 置き場の種類。
    pub fn kind(self) -> UserPlaceKind {
        match self {
            RevocationSide::A => UserPlaceKind::RevocationsA,
            RevocationSide::B => UserPlaceKind::RevocationsB,
        }
    }

    fn name(self) -> &'static str {
        match self {
            RevocationSide::A => "A",
            RevocationSide::B => "B",
        }
    }
}

/// 片方の記録を読んだ結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideContent {
    /// 置き場は確かめに通り、記録が無い（置き場のフォルダがまだ無い場合を含む）。
    Missing,
    /// 置き場もファイルも確かめに通り、形が読めた。
    Record(RevocationRecord),
    /// 欠けている: 置き場かファイルが確かめに通らない、または形が壊れている。
    Lost {
        /// 理由（表示用）。
        reason: String,
        /// 確かめを通さずに形だけ読めた拇印（和に入れる。安全側にしか使わない）。
        salvaged: Vec<RevokedCert>,
        /// 置き場のフォルダは確かめに通っているか（通っていれば、置き換えで書き直せる。
        /// 通っていなければ、通るようになるまで書かない）。
        place_trusted: bool,
    },
    /// 読めなかった（開けない・読めないなどの I/O の誤り。状態を見たのではない）。
    Unread {
        /// 理由（表示用）。
        reason: String,
    },
}

/// 利用者の記録 A・B を読んだもの（ロック無しで読める。和で照合する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRevocations {
    a: SideContent,
    b: SideContent,
    union: BTreeMap<CertThumbprint, RevokedCert>,
}

impl UserRevocations {
    fn new(a: SideContent, b: SideContent) -> Self {
        let mut union = BTreeMap::new();
        for side in [&a, &b] {
            for entry in side_entries(side) {
                union.entry(entry.thumbprint).or_insert(*entry);
            }
        }
        Self { a, b, union }
    }

    /// 拇印が失効しているか（A・B の和。確かめに通らない置き場から形だけ読めた拇印も含む）。
    pub fn is_revoked(&self, thumbprint: &CertThumbprint) -> bool {
        self.union.contains_key(thumbprint)
    }

    /// 失効した証明書（A・B の和。拇印の昇順）。
    pub fn entries(&self) -> impl Iterator<Item = &RevokedCert> {
        self.union.values()
    }

    /// 片方の記録を読んだ結果。
    pub fn side(&self, side: RevocationSide) -> &SideContent {
        match side {
            RevocationSide::A => &self.a,
            RevocationSide::B => &self.b,
        }
    }

    /// 両方とも確かめに通って `確定` で、同じ拇印の並びか（[`reconcile_user_revocations`] の
    /// 「整っている」の形）。
    pub fn is_settled(&self) -> bool {
        match (&self.a, &self.b) {
            (SideContent::Record(a), SideContent::Record(b)) => {
                a.state() == RecordState::Committed
                    && b.state() == RecordState::Committed
                    && same_thumbprints(a.entries(), &self.union)
                    && same_thumbprints(b.entries(), &self.union)
            }
            _ => false,
        }
    }
}

fn side_entries(side: &SideContent) -> &[RevokedCert] {
    match side {
        SideContent::Record(record) => record.entries(),
        SideContent::Lost { salvaged, .. } => salvaged,
        SideContent::Missing | SideContent::Unread { .. } => &[],
    }
}

fn same_thumbprints(
    entries: &[RevokedCert],
    union: &BTreeMap<CertThumbprint, RevokedCert>,
) -> bool {
    entries.len() == union.len()
        && entries
            .iter()
            .all(|entry| union.contains_key(&entry.thumbprint))
}

/// 確かめを通さずに、記録の形だけ読めた拇印（読めなければ空）。
fn salvage<O: UserTlsOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    side: RevocationSide,
) -> Vec<RevokedCert> {
    let spec = place.spec(side.kind());
    match read_unchecked(ops, &spec, REVOCATIONS_FILE, MAX_REVOCATIONS_BYTES) {
        UncheckedRead::Bytes(bytes) => RevocationRecord::parse(&bytes)
            .map(|record| record.entries)
            .unwrap_or_default(),
        UncheckedRead::Absent | UncheckedRead::Unreadable => Vec::new(),
    }
}

/// 片方の記録を、置き場の確かめ（[`open_user_place`]）とファイルの確かめ（[`open_user_file`]）
/// に通して読む。`on_checked` は、置き場が確かめに通ったときに、握ったフォルダで呼ぶ（ロックを
/// 握っている呼び手が、残った一時ファイルを片付けるため）。
fn read_side<O: UserTlsOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    side: RevocationSide,
    on_checked: &dyn Fn(&O::Dir),
) -> SideContent {
    let lost = |problem: UserPlaceProblem, place_trusted: bool| {
        if problem.fault == UserFault::Transient {
            SideContent::Unread {
                reason: problem.reason,
            }
        } else {
            SideContent::Lost {
                reason: problem.reason,
                salvaged: salvage(ops, place, side),
                place_trusted,
            }
        }
    };
    let checked = match open_user_place(ops, place, side.kind()) {
        Ok(UserPlaceState::Present(checked)) => checked,
        Ok(UserPlaceState::Absent(_)) => return SideContent::Missing,
        Err(problem) => return lost(problem, false),
    };
    on_checked(checked.dir());
    let label = format!("{RECORD_LABEL} {}", side.name());
    let mut file = match open_user_file(
        ops,
        &checked,
        REVOCATIONS_FILE,
        &label,
        UserFileRole::Public,
    ) {
        Ok(Some(file)) => file,
        Ok(None) => return SideContent::Missing,
        Err(problem) => return lost(problem, true),
    };
    let shown = checked.dir_path().join(REVOCATIONS_FILE);
    let bytes = match file.read_all(MAX_REVOCATIONS_BYTES) {
        Ok(bytes) => bytes,
        // 上限超えは形が壊れているのと同じ恒久的な破損（消失として直す）。
        Err(ReadError::TooLarge) => {
            return SideContent::Lost {
                reason: format!(
                    "{label}（{}）が大きすぎます（上限 {} KiB）",
                    shown.display(),
                    MAX_REVOCATIONS_BYTES / 1024
                ),
                salvaged: Vec::new(),
                place_trusted: true,
            }
        }
        Err(error) => {
            return SideContent::Unread {
                reason: format!("{label}（{}）を読めませんでした: {error}", shown.display()),
            }
        }
    };
    match RevocationRecord::parse(&bytes) {
        Ok(record) => SideContent::Record(record),
        Err(error) => SideContent::Lost {
            reason: format!("{label}（{}）の形が壊れています: {error}", shown.display()),
            salvaged: Vec::new(),
            place_trusted: true,
        },
    }
}

/// 照合の読み口: **ロック無しで** A・B を読み、和を取る（§4.4.1「照合のために読むだけなら
/// ロックは要らない」。記録は置き換えで書くので、読めるのは常に揃った中身）。書き込み・作成は
/// しない。確かめに通らない置き場の記録も、形が読めれば和に入れる（和は増えるだけなので、
/// 安全側にしか動かない。場面 27 で、サービスの証明書の照合を続けるため）。
pub fn read_user_revocations<O: UserTlsOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
) -> UserRevocations {
    UserRevocations::new(
        read_side(ops, place, RevocationSide::A, &|_| {}),
        read_side(ops, place, RevocationSide::B, &|_| {}),
    )
}

// --- 整える手順 ------------------------------------------------------------------------

/// 整える手順の設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileOptions {
    /// ロックを待つ上限（既定 [`DEFAULT_LOCK_WAIT`]）。
    pub lock_wait: Duration,
    /// ロックを試し直す間隔（既定 [`DEFAULT_LOCK_POLL`]。0 は 1 ミリ秒として扱う）。
    pub lock_poll: Duration,
}

impl Default for ReconcileOptions {
    fn default() -> Self {
        Self {
            lock_wait: DEFAULT_LOCK_WAIT,
            lock_poll: DEFAULT_LOCK_POLL,
        }
    }
}

/// 手順の結果（§4.4.1「手順の結果は 3 つ」。「状態を確かに見た」と「操作が終わらなかった」を
/// 分ける）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// **整っている**: A・B とも `確定` で同じ中身（初期化とその続きを仕上げた場合を含む）。
    /// 利用者の証明書を作る・使い始めてよいのはこのときだけ（そのうえで、証明書の拇印が
    /// [`ReconcileReport::revocations`] に無いこと）。
    Consistent,
    /// **通らない**（確かに見た）: 消失と判定した（その回に直せたかどうかに依らない）、または
    /// 記録の置き場・ロックファイルが確かめに通らない。
    Refused,
    /// **この回はできなかった**: ロックが上限までに取れない、読み書きが I/O の誤りで終わら
    /// なかった。記録の状態を見たのではなく、操作が終わらなかっただけ。
    Incomplete,
}

/// 手順が出す警告の理由（画面とログに出す文言は呼ぶ側が決める）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevocationWarning {
    /// 置き場の指定（[`UserTlsPlace`]）の並びが使えない。
    Layout(String),
    /// ロックファイルの置き場かファイルが確かめに通らない（何も書かなかった）。
    LockRefused(UserPlaceProblem),
    /// ロックが上限までに取れなかった（待った時間）。
    LockTimeout(Duration),
    /// **消失を見た**: この側の記録が無い・壊れている・置き場が確かめに通らない・あるはずの
    /// ない状態（B が `準備中`）。「失効の記録が消された・壊れた」を画面とログに出す。
    Lost {
        /// どちらの側か。
        side: RevocationSide,
        /// 理由（表示用）。
        reason: String,
    },
    /// 記録を新しく作る・直すので、利用者の `tls\` に**すでにあった証明書を失効に入れる**
    /// （その証明書は使えない。作り直しで戻る）。記録に書けたかどうかは
    /// [`ReconcileReport::revocations`] で分かる（置き場が通らない・書けないときは残らないが、
    /// そのときの結果は「整っている」にならないので、どちらにしても使わない）。
    ExistingCertRevoked(CertThumbprint),
    /// 置き場が確かめに通らないので、この側には書かなかった（通るようになるまで作り直さない）。
    NotRepaired {
        /// どちらの側か。
        side: RevocationSide,
        /// 理由（表示用）。
        reason: String,
    },
    /// 読み書きが終わらなかった（I/O の誤り。理由）。
    Io(String),
}

/// 手順の結果と、その後の記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 結果。
    pub outcome: ReconcileOutcome,
    /// 警告の理由（無ければ空）。
    pub warnings: Vec<RevocationWarning>,
    /// 手順の後に、ロック無しで読み直した記録（照合に使う）。
    pub revocations: UserRevocations,
}

impl ReconcileReport {
    /// 消失を見たか（[`RevocationWarning::Lost`] がある）。
    pub fn saw_loss(&self) -> bool {
        self.warnings
            .iter()
            .any(|warning| matches!(warning, RevocationWarning::Lost { .. }))
    }
}

/// 利用者の記録を整える（§4.4.1「利用者の記録を整える手順」）。走らせるのは、デスクトップ版の
/// Hub（起動のとき、証明書を作る・作り直す前、周期ごと）と ChronoGazer デスクトップ（起動の
/// とき、見張りの周期ごと）。
///
/// 1. 置き場の指定の並びを確かめる（[`check_place_layout`]）。
/// 2. ロックファイルを確かめて排他に開く（無ければ作る。上限付きで待つ）。取れない・通らない
///    なら**何も書かずに**終える。
/// 3. ロックを握ったまま A・B を読み直して判定する:
///
/// | A        | B      | すること                                                   |
/// | -------- | ------ | ---------------------------------------------------------- |
/// | 無い     | 無い   | A を `準備中` で作る → B を `確定` で作る → A を `確定` に |
/// | `準備中` | 無い   | B を `確定` で作る → A を `確定` に（A の拇印は引き継ぐ）  |
/// | `準備中` | `確定` | 和を両方に書き、A を `確定` に                             |
/// | `確定`   | `確定` | 和を取り、片方にしか無い拇印をもう片方へ書き戻す           |
/// | それ以外 |        | 消失: 下                                                   |
///
/// - 記録のファイルを新しく作るとき（上の 1・2 行目と消失の直し）、利用者の `tls\` に証明書が
///   すでにあれば、その拇印を失効に入れる。証明書があるかどうかが分からない（開けない・
///   読めない・途中がリンク）ときは、**作らない・直さない**（その証明書を失効に入れ損ねた記録を
///   「整っている」にしないため）。
/// - 消失: 読めた拇印の和（確かめに通らない置き場のものも、形が読めれば入れる）に今の証明書の
///   拇印を足し、**先に残っているほう**を書き換え、欠けたほうを `確定` で作り直す。どちらも
///   読めなければ未初期化と同じ順で作る。確かめに通らない置き場には書かない。結果は、直せても
///   「通らない」（次の回に「整っている」になる）。
/// - 記録が I/O の誤りで読めないときは判定せず、「この回はできなかった」。
///
/// 何度走らせても同じ結果になり、途中で止まっても次の回が同じ判定で続ける。
pub fn reconcile_user_revocations<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    options: &ReconcileOptions,
) -> ReconcileReport {
    run(ops, place, options, None)
}

/// 失効を書き足す（§4.4.2 の場面 6・7・9: 露出を見た証明書の拇印を A・B に足す）。
/// [`reconcile_user_revocations`] と同じ手順（ロックを取って読み直す）で、和に `thumbprint` を
/// 足して両方に書く。
///
/// 書けたかどうかは、返った [`ReconcileReport::revocations`] に拇印があるかで分かる。置き場・
/// ロックが通らない・書けないときは記録に残らないので、呼ぶ側は**記録に残せなくても**その
/// 証明書をすぐ外し、動いている間は覚えておく（場面 23。再起動をまたぐ残りは設計 §7）。
pub fn record_user_revocation<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    options: &ReconcileOptions,
    thumbprint: CertThumbprint,
    cert_place: RevokedCertPlace,
) -> ReconcileReport {
    run(ops, place, options, Some((thumbprint, cert_place)))
}

fn run<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    options: &ReconcileOptions,
    extra: Option<(CertThumbprint, RevokedCertPlace)>,
) -> ReconcileReport {
    let mut warnings = Vec::new();
    let outcome = run_locked(ops, place, options, extra, &mut warnings);
    // ロックを放した後に読み直す（照合はロック無し）。
    let revocations = read_user_revocations(ops, place);
    // 「整っている」と返すのは、読み直しても両方が確定で揃っているときだけ（書いた直後に
    // 別の誰かに書き換えられた・書いたものが確かめに通らない、を「整っている」にしない）。
    let outcome = if outcome == ReconcileOutcome::Consistent && !revocations.is_settled() {
        warnings.push(RevocationWarning::Io(
            "書いた後に読み直した記録が揃っていません".to_string(),
        ));
        ReconcileOutcome::Incomplete
    } else {
        outcome
    };
    ReconcileReport {
        outcome,
        warnings,
        revocations,
    }
}

/// 握ったロック（drop で放す）。置き場のフォルダも握ったままにする。
struct LockGuard<O: RevocationOps> {
    _file: O::File,
    _place: CheckedUserPlace<O::Dir>,
}

enum LockError {
    Refused(UserPlaceProblem),
    Timeout(Duration),
    Io(String),
}

/// 置き場の問題を、ロック・書き込みの失敗の形に分ける（一時的なら I/O、それ以外は通らない）。
fn refusal_or_io<T>(
    problem: UserPlaceProblem,
    refused: impl FnOnce(UserPlaceProblem) -> T,
    io: impl FnOnce(String) -> T,
) -> T {
    if problem.fault == UserFault::Transient {
        io(problem.reason)
    } else {
        refused(problem)
    }
}

/// 置き場 `kind` を確かめて開く。フォルダが無ければ、確かめ済みのいちばん深いフォルダの
/// ハンドル基準で 1 つずつ作り、**作るたびに起点から確かめ直す**。
fn open_or_create_place<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    kind: UserPlaceKind,
) -> Result<CheckedUserPlace<O::Dir>, WriteError> {
    // 作るのは多くても、指定のフォルダの数だけ。
    let tries = place.spec(kind).dirs.len() + 1;
    for _ in 0..tries {
        let missing = match open_user_place(ops, place, kind) {
            Ok(UserPlaceState::Present(checked)) => return Ok(checked),
            Ok(UserPlaceState::Absent(missing)) => missing,
            Err(problem) => {
                return Err(refusal_or_io(problem, WriteError::Refused, WriteError::Io))
            }
        };
        let Some(name) = missing.missing().first() else {
            break;
        };
        ops.create_child_dir(missing.deepest(), name)
            .map_err(|error| {
                WriteError::Io(format!(
                    "{}のフォルダ（{}）を作れませんでした: {error}",
                    kind.label(),
                    missing.deepest_path().join(name).display()
                ))
            })?;
        // `missing`（握ったフォルダ）をここで放し、起点から確かめ直す。
    }
    Err(WriteError::Io(format!(
        "{}のフォルダを作っても、置き場が現れませんでした",
        kind.label()
    )))
}

/// ロックファイルを確かめて排他に開く（無ければ作る。上限付きで待つ）。
fn acquire_lock<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    options: &ReconcileOptions,
) -> Result<LockGuard<O>, LockError> {
    /// 「無い → 作る → 開く」をやり直す回数（作った直後に消された、への備え）。
    const CREATE_TRIES: u32 = 3;

    let kind = UserPlaceKind::RevocationsLock;
    let checked = open_or_create_place(ops, place, kind).map_err(|error| match error {
        WriteError::Refused(problem) => LockError::Refused(problem),
        WriteError::Io(error) => LockError::Io(error),
    })?;
    let Some(name) = checked.file_name().map(str::to_string) else {
        // `check_place_layout` を通っていれば起きない。
        return Err(LockError::Io(format!(
            "{}がファイルの指定ではありません",
            kind.label()
        )));
    };
    let shown = checked.dir_path().join(&name);
    let poll = options.lock_poll.max(Duration::from_millis(1));
    let mut waited = Duration::ZERO;
    let mut created = 0;
    let file = loop {
        match ops.open_child_file_exclusive(checked.dir(), &name) {
            Ok(Some(file)) => break file,
            Ok(None) => {
                if created == CREATE_TRIES {
                    return Err(LockError::Io(format!(
                        "ロックファイル（{}）を作っても開けませんでした",
                        shown.display()
                    )));
                }
                created += 1;
                ops.create_file_if_absent(checked.dir(), &name)
                    .map_err(|error| {
                        LockError::Io(format!(
                            "ロックファイル（{}）を作れませんでした: {error}",
                            shown.display()
                        ))
                    })?;
            }
            Err(FileOpenError::InUse) => {
                if waited >= options.lock_wait {
                    return Err(LockError::Timeout(waited));
                }
                let step = poll.min(options.lock_wait - waited);
                ops.sleep(step);
                waited += step;
            }
            Err(FileOpenError::Other(error)) => {
                return Err(LockError::Io(format!(
                    "ロックファイル（{}）を開けませんでした: {error}",
                    shown.display()
                )))
            }
        }
    };
    // 開いた後に、握ったハンドルで確かめる（作る前の確かめは置き場のフォルダ、作った後の
    // 確かめはここ）: ふつうのファイル・本当の場所・所有者と DACL・ハードリンクでない。
    let security = file.security();
    let untrusted = |reason: String, fault: UserFault| {
        LockError::Refused(UserPlaceProblem {
            reason,
            remedy: UserRemedy::FixAnchor(checked.dir_path().to_path_buf()),
            fault,
        })
    };
    let kind_problem = if security.is_dir {
        Some("フォルダです".to_string())
    } else if security.is_reparse_point {
        Some("シンボリックリンクなどの reparse point です".to_string())
    } else if !guard::paths_match(&file.final_path(), &checked.dir_real().join(&name)) {
        Some(format!(
            "本当の場所（{}）が期待と違います",
            file.final_path().display()
        ))
    } else {
        None
    };
    if let Some(found) = kind_problem {
        return Err(untrusted(
            format!("ロックファイル（{}）は{found}", shown.display()),
            UserFault::Untrusted,
        ));
    }
    let open = user_place_problems(&security, checked.owners(), true);
    if !open.is_empty() {
        return Err(untrusted(
            format!(
                "ロックファイル（{}）は保護されていません（{}）",
                shown.display(),
                user_reasons_text(&open)
            ),
            fault_of(&open),
        ));
    }
    Ok(LockGuard {
        _file: file,
        _place: checked,
    })
}

enum WriteError {
    /// 置き場が確かめに通らない（書かなかった）。
    Refused(UserPlaceProblem),
    /// I/O の誤りで終わらなかった。
    Io(String),
}

/// 片方の記録を書く: 置き場を確かめ（無ければ作って確かめ直し）、握ったフォルダのハンドル
/// 基準で、残った一時ファイルを片付けてから置き換える。
fn write_side<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    side: RevocationSide,
    record: &RevocationRecord,
) -> Result<(), WriteError> {
    let checked = open_or_create_place(ops, place, side.kind())?;
    let shown = checked.dir_path().join(REVOCATIONS_FILE);
    let bytes = record.encode();
    if bytes.len() > MAX_REVOCATIONS_BYTES {
        return Err(WriteError::Io(format!(
            "{RECORD_LABEL} {}（{}）が大きくなりすぎます（{} 件）",
            side.name(),
            shown.display(),
            record.entries().len()
        )));
    }
    // 片付けの失敗では止めない（一時ファイルは読まないので、残っても判定は変わらない）。
    let _ = ops.remove_stale_temps(checked.dir(), REVOCATIONS_FILE);
    ops.write_atomic(checked.dir(), REVOCATIONS_FILE, &bytes)
        .map_err(|error| {
            WriteError::Io(format!(
                "{RECORD_LABEL} {}（{}）を書けませんでした: {error}",
                side.name(),
                shown.display()
            ))
        })
}

/// 利用者の `tls\` に今ある証明書。
enum CurrentCert {
    /// 無い（証明書として読める形でないものも、失効に入れる拇印が無いのでここ）。
    None,
    /// ある（確かめに通ったか、形だけ読めた）。
    Thumbprint(CertThumbprint),
    /// あるかどうか分からない（理由）。
    Unknown(String),
}

fn current_user_cert<O: UserTlsOps, P: UserTlsPlace + ?Sized>(ops: &O, place: &P) -> CurrentCert {
    match load_pinned_user_cert(ops, place) {
        UserCertPin::Absent => CurrentCert::None,
        UserCertPin::Usable(pin) => match CertThumbprint::of_pem(&pin.cert_pem) {
            Ok(thumbprint) => CurrentCert::Thumbprint(thumbprint),
            Err(error) => CurrentCert::Unknown(error),
        },
        UserCertPin::Unusable(unusable) => match unusable.seen {
            UncheckedUserCert::Seen(seen) => CurrentCert::Thumbprint(seen.thumbprint),
            UncheckedUserCert::Absent | UncheckedUserCert::Malformed => CurrentCert::None,
            UncheckedUserCert::Unreadable => CurrentCert::Unknown(format!(
                "利用者の証明書があるかどうかを確かめられませんでした（{}）",
                unusable.problem.reason
            )),
        },
    }
}

/// 判定に使う、片方の記録の形。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Missing,
    Pending,
    Committed,
    Lost,
}

fn shape(side: &SideContent) -> Shape {
    match side {
        SideContent::Missing => Shape::Missing,
        SideContent::Record(record) => match record.state() {
            RecordState::Pending => Shape::Pending,
            RecordState::Committed => Shape::Committed,
        },
        // `Unread` は判定の前に外す。
        SideContent::Lost { .. } | SideContent::Unread { .. } => Shape::Lost,
    }
}

fn run_locked<O: RevocationOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    options: &ReconcileOptions,
    extra: Option<(CertThumbprint, RevokedCertPlace)>,
    warnings: &mut Vec<RevocationWarning>,
) -> ReconcileOutcome {
    use ReconcileOutcome::{Consistent, Incomplete, Refused};

    // --- 1. 置き場の指定 ----------------------------------------------------------------
    if let Err(error) = check_place_layout(place) {
        warnings.push(RevocationWarning::Layout(error));
        return Refused;
    }

    // --- 2. ロック ----------------------------------------------------------------------
    let _lock = match acquire_lock(ops, place, options) {
        Ok(lock) => lock,
        Err(LockError::Refused(problem)) => {
            warnings.push(RevocationWarning::LockRefused(problem));
            return Refused;
        }
        Err(LockError::Timeout(waited)) => {
            warnings.push(RevocationWarning::LockTimeout(waited));
            return Incomplete;
        }
        Err(LockError::Io(error)) => {
            warnings.push(RevocationWarning::Io(error));
            return Incomplete;
        }
    };

    // --- 3. 読み直して判定 ----------------------------------------------------------------
    // ロックを握っているので、残った一時ファイルは誰のものでもない。読まずに片付ける。
    let tidy = |dir: &O::Dir| {
        let _ = ops.remove_stale_temps(dir, REVOCATIONS_FILE);
    };
    let sides = [
        read_side(ops, place, RevocationSide::A, &tidy),
        read_side(ops, place, RevocationSide::B, &tidy),
    ];
    let mut unread = false;
    for side in &sides {
        if let SideContent::Unread { reason } = side {
            warnings.push(RevocationWarning::Io(reason.clone()));
            unread = true;
        }
    }
    if unread {
        return Incomplete;
    }

    let now = ops.now_unix_seconds();
    let mut union: BTreeMap<CertThumbprint, RevokedCert> = BTreeMap::new();
    for side in &sides {
        for entry in side_entries(side) {
            union.entry(entry.thumbprint).or_insert(*entry);
        }
    }
    if let Some((thumbprint, cert_place)) = extra {
        union.entry(thumbprint).or_insert(RevokedCert {
            thumbprint,
            place: cert_place,
            recorded_at: now,
        });
    }
    let shapes = [shape(&sides[0]), shape(&sides[1])];
    let [a, b] = RevocationSide::BOTH;

    // 記録を新しく作る・直すとき: 利用者の証明書がすでにあれば、失効に入れる。
    let add_current_cert = |union: &mut BTreeMap<CertThumbprint, RevokedCert>,
                            warnings: &mut Vec<RevocationWarning>|
     -> Result<(), String> {
        match current_user_cert(ops, place) {
            CurrentCert::None => Ok(()),
            CurrentCert::Unknown(reason) => Err(reason),
            CurrentCert::Thumbprint(thumbprint) => {
                if let Entry::Vacant(slot) = union.entry(thumbprint) {
                    slot.insert(RevokedCert {
                        thumbprint,
                        place: RevokedCertPlace::User,
                        recorded_at: now,
                    });
                    warnings.push(RevocationWarning::ExistingCertRevoked(thumbprint));
                }
                Ok(())
            }
        }
    };
    let record = |state: RecordState, union: &BTreeMap<CertThumbprint, RevokedCert>| {
        RevocationRecord::new(state, union.values().copied())
    };
    // 今の中身が、確定の和と同じか（同じなら書かない）。
    let up_to_date = |side: &SideContent, union: &BTreeMap<CertThumbprint, RevokedCert>| {
        matches!(side, SideContent::Record(found)
            if found.state() == RecordState::Committed && same_thumbprints(found.entries(), union))
    };
    // 書く順の並び（側と状態）を、失敗したところで止めて実行する。
    let write_all = |steps: &[(RevocationSide, RecordState)],
                     union: &BTreeMap<CertThumbprint, RevokedCert>,
                     warnings: &mut Vec<RevocationWarning>|
     -> Result<(), ReconcileOutcome> {
        for (side, state) in steps {
            match write_side(ops, place, *side, &record(*state, union)) {
                Ok(()) => {}
                Err(WriteError::Io(error)) => {
                    warnings.push(RevocationWarning::Io(error));
                    return Err(Incomplete);
                }
                Err(WriteError::Refused(problem)) => {
                    warnings.push(RevocationWarning::NotRepaired {
                        side: *side,
                        reason: problem.reason,
                    });
                    return Err(Refused);
                }
            }
        }
        Ok(())
    };
    let init_order = [
        (a, RecordState::Pending),
        (b, RecordState::Committed),
        (a, RecordState::Committed),
    ];

    let (steps, loss): (Vec<(RevocationSide, RecordState)>, bool) = match shapes {
        // 未初期化・初期化の途中で止まった（B がまだ無い）: 記録を新しく作る。
        [Shape::Missing, Shape::Missing] | [Shape::Pending, Shape::Missing] => {
            if let Err(reason) = add_current_cert(&mut union, warnings) {
                warnings.push(RevocationWarning::Io(reason));
                return Incomplete;
            }
            let steps = if shapes[0] == Shape::Missing {
                init_order.to_vec()
            } else {
                init_order[1..].to_vec()
            };
            (steps, false)
        }
        // 初期化の最後で止まった: 和を両方に書き、A を確定にする。
        [Shape::Pending, Shape::Committed] => {
            let mut steps = Vec::new();
            if !up_to_date(&sides[1], &union) {
                steps.push((b, RecordState::Committed));
            }
            steps.push((a, RecordState::Committed));
            (steps, false)
        }
        // 整っている: 片方にしか無い拇印をもう片方へ書き戻す。
        [Shape::Committed, Shape::Committed] => {
            let steps = RevocationSide::BOTH
                .into_iter()
                .zip(&sides)
                .filter(|(_, side)| !up_to_date(side, &union))
                .map(|(side, _)| (side, RecordState::Committed))
                .collect();
            (steps, false)
        }
        // 消失（§4.4.1「消失の直し方」）。
        _ => {
            warn_loss(&sides, warnings);
            // 今の証明書を失効に入れる。あるかどうか分からなければ、直さない（入れ損ねた記録を
            // 次の回に「整っている」にしないため。次の回も消失として扱う）。
            if let Err(reason) = add_current_cert(&mut union, warnings) {
                warnings.push(RevocationWarning::Io(reason));
                return Refused;
            }
            (loss_steps(&sides, &union), true)
        }
    };
    let written = write_all(&steps, &union, warnings);
    if loss {
        // 直せたかどうかに依らず「通らない」（消失を確かに見た）。次の回に「整っている」になる。
        return Refused;
    }
    match written {
        Ok(()) => Consistent,
        Err(outcome) => outcome,
    }
}

/// 確かめに通らない置き場か（通るようになるまで書かない）。
fn place_untrusted(content: &SideContent) -> bool {
    matches!(
        content,
        SideContent::Lost {
            place_trusted: false,
            ..
        }
    )
}

/// 消失の警告: 側ごとに何を見たかと、書かない側。
fn warn_loss(sides: &[SideContent; 2], warnings: &mut Vec<RevocationWarning>) {
    for (side, content) in RevocationSide::BOTH.into_iter().zip(sides) {
        let reason = match content {
            SideContent::Lost { reason, .. } => reason.clone(),
            SideContent::Missing => format!(
                "{RECORD_LABEL} {} がありません（もう片方はあります）",
                side.name()
            ),
            SideContent::Record(record)
                if side == RevocationSide::B && record.state() == RecordState::Pending =>
            {
                format!("{RECORD_LABEL} B が準備中になっています（準備中になるのは A だけです）")
            }
            SideContent::Record(_) | SideContent::Unread { .. } => continue,
        };
        warnings.push(RevocationWarning::Lost {
            side,
            reason: reason.clone(),
        });
        if place_untrusted(content) {
            warnings.push(RevocationWarning::NotRepaired { side, reason });
        }
    }
}

/// 消失を直すときに書く順（純関数）: **先に残っているほう**（確かめに通って読めた記録）、次に
/// 欠けたほう。どちらも読めず、どちらの置き場も通るなら、未初期化と同じ順。確かめに通らない
/// 置き場には書かない。残っているほうがもう確定の和と同じ中身なら、そこは書かない。
fn loss_steps(
    sides: &[SideContent; 2],
    union: &BTreeMap<CertThumbprint, RevokedCert>,
) -> Vec<(RevocationSide, RecordState)> {
    let [a, b] = RevocationSide::BOTH;
    let survives = |content: &SideContent| matches!(content, SideContent::Record(_));
    if sides.iter().all(|content| !survives(content))
        && sides.iter().all(|content| !place_untrusted(content))
    {
        return vec![
            (a, RecordState::Pending),
            (b, RecordState::Committed),
            (a, RecordState::Committed),
        ];
    }
    let mut order: Vec<(RevocationSide, &SideContent)> =
        RevocationSide::BOTH.into_iter().zip(sides).collect();
    order.sort_by_key(|(_, content)| !survives(content));
    order
        .into_iter()
        .filter(|(_, content)| !place_untrusted(content))
        .filter(|(_, content)| {
            !matches!(content, SideContent::Record(found)
                if found.state() == RecordState::Committed
                    && same_thumbprints(found.entries(), union))
        })
        .map(|(side, _)| (side, RecordState::Committed))
        .collect()
}
