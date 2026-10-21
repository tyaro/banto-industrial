//! 接続の秘密値（R2b-1 の時点では `postgres` の `password` だけ）の封印と開封
//! （R2b-1b、docs/banto-hub-connection-settings-design.md §5）。
//!
//! # 何をするか
//!
//! 平文の秘密値を [`ConnectionSecrets::seal`] で [`SealedSecret`]（参照・方式・blob）に
//! 変え、[`ConnectionSecrets::open`] で平文（`Zeroizing<String>`）に戻す。blob は DB の
//! 秘密の表 `plc_connection_secrets`（migration `0025`、banto-tags の
//! `connection_secret`）に置き、接続の `settings` には値の代わりに参照
//! `{"$secret": "<ref>"}` を置く（設計 §3.3）。
//!
//! R2b-1d（part A）から配線した: REST・MCP・未適用キューの適用の入口が
//! [`password_changes`] で平文を封印して banto-tags へ渡し、読む側（DB Source の計画・
//! 接続のテスト・`GET /api/sink/config`・`GET /api/ingest/bridge-config`）が
//! [`open_connection_secret`] で使う直前に開く。封印は全 OS で `plain-v1`
//! （[`r2b1d_connection_secrets`]）。R2b-1d（part B）で、読めないものを空に潰さず
//! `passwordState`（[`secret_state`]）・`unavailableConnections` / `unavailableSources`・
//! DB Source の「パスワードを読めません」に出すところまで配線した。Windows の DPAPI・
//! 起動時の封印し直しは R2b-1e。
//!
//! # なぜ DB に DPAPI の blob を置くか（設計 §5.1 の Q1 (c)）
//!
//! Hub の接続の書き込みは DB のトランザクション（`BEGIN IMMEDIATE`）と未適用キューで
//! 確定する。秘密値を `banto_secret_store::FileSecretStore` のファイルに置くと DB と
//! 二相になり、取り消し・失敗・クラッシュで孤児と食い違いが出る。そこで同じ crate の
//! [`Protector`]（Windows は [`banto_secret_store::DpapiMachine`]）で封印した blob を
//! DB に置き、接続の行と同じトランザクションで確定させる。暗号は DPAPI に任せ、
//! 自前の暗号の組み立て（nonce・鍵の回転）は持たない。
//!
//! # 方式（[`SecretScheme`]。DB の `scheme` 列に文字列で入る）
//!
//! - `dpapi-machine-v1`: `protector` があるとき（Windows の既定）。blob = DPAPI の
//!   machine スコープの暗号文。
//! - `plain-v1`: `protector` が無いとき（非 Windows の既定。開発・CI・E2E、設計 §5.8 の
//!   Q2）。**blob = UTF-8 の平文そのもの**で、守りは今の平文の列と同じ。Windows の Hub は
//!   起動時に [`ConnectionSecrets::reseal_plain`] で `dpapi-machine-v1` へ封印し直す
//!   （設計 §6.5、参照は変えない）。
//!
//! # entropy（設計 §5.2）
//!
//! `"banto-hub/plc-connection-secret/v1:" + ref + ":" + key`。参照（行ごとの 128 ビットの
//! 乱数）とキーの名前を DPAPI の「追加のエントロピー」に入れるので、blob を別の行
//! （別の参照）や別のキーへ写しても開けない（`banto_secret_store::entropy_for` と同じ
//! 考え方）。参照は 32 文字の固定長、キーは `:` を含まない文字だけ（[`validate_key`]）
//! なので、entropy の区切りは一意に読める。
//!
//! # machine スコープの意味と限界（設計 §1.6・§5.1・§5.7）
//!
//! `dpapi-machine-v1` の blob は、**同じ PC の上なら DB を読めるすべてのプロセスが
//! 開ける**（サービスの LocalSystem・デスクトップの利用者・昇格ヘルパーのどれでも）。
//! 守るのは、DB のファイル・バックアップ・サポートのための複製・未適用キューの行を
//! **別の PC へ持ち出したとき**とオフラインの盗難だけ。同じ PC の中の境界は profile の
//! ACL（DB と同じ相手）で、このモジュールは ACL を見ない。OS を入れ直す・別の PC に DB を
//! 移すと開けなくなる（[`UnreadableReason::Undecryptable`]、設計 §5.5）。VM の複製先では
//! 開ける。user スコープの DPAPI は、サービスとデスクトップが同じ profile を交互に開くので
//! 採らない。
//!
//! # 秘密を外へ出さない（checklist §5「平文の資格情報は、それを持つ層の外に出さない」）
//!
//! - 平文は呼び出し側から受けた `&str` と、[`ConnectionSecrets::open`] が返す
//!   `Zeroizing<String>` の中だけにある。中間のバッファ（`plain-v1` の blob の写し・
//!   DPAPI が返した平文・UTF-8 でなかったバイト列）は `Zeroizing` か明示の `zeroize` で消す。
//! - [`SealedSecret::blob`] も `Zeroizing<Vec<u8>>`（`plain-v1` の blob は平文そのものなので）。
//! - [`SealedSecret`]・[`ConnectionSecrets`] の `Debug` は手で実装し、参照と方式（と protector
//!   の有無）だけを出す。blob も平文も出さない。
//! - [`SecretError`] のどの variant も平文・blob・入力の文字列を持たない（`banto-secret-store`
//!   の [`SecretStoreError`] と同じ約束。そちらを包むときもその約束の上にある）。このモジュールは
//!   ログを一切出さない。
//! - 「パスワード無し」（[`SecretState::None`]）と「読めない」（[`SecretState::Unreadable`]）を
//!   分ける。開封の失敗を空の値に潰さない（checklist §5「エラーを空に潰さない」、設計 §5.5）。
//!   空の値は封印しない（空は「消す」の意味で、`Clear` として秘密の行ごと消す。設計 §5.3）。
//!
//! # このモジュールがしないこと
//!
//! - DB への読み書き・参照と行の整合の確認（R2b-1d・設計 §5.3）。行が無い参照の
//!   [`UnreadableReason::Missing`] は、それを見つけた呼び出し側が作る。
//! - 起動時の封印し直しの手順・`VACUUM`（R2b-1e・設計 §6.5）。ここは 1 件の
//!   [`ConnectionSecrets::reseal_plain`] だけを持つ。
//! - ACL・ファイルの保存先（`FileSecretStore` は使わない）。

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use banto_secret_store::format::MAX_SECRET_LEN;
use banto_secret_store::{Protector, SecretStoreError};
use zeroize::{Zeroize, Zeroizing};

/// entropy の前置き（版 1）。設計 §5.2 と一字一句同じ。
const ENTROPY_PREFIX: &str = "banto-hub/plc-connection-secret/v1:";

/// 参照の乱数のバイト数（128 ビット。16 進で 32 文字、migration `0025` の
/// `lower(hex(randomblob(16)))` と同じ形）。
const REF_BYTES: usize = 16;
/// 参照の文字数。
pub const SECRET_REF_LEN: usize = REF_BYTES * 2;

/// キーの名前の長さの上限（バイト）。
pub const MAX_KEY_LEN: usize = 64;

/// 封印の方式。DB の `plc_connection_secrets.scheme` 列に [`SecretScheme::as_str`] の
/// 文字列で入る（設計 §3.1 の `CHECK (scheme IN ('plain-v1', 'dpapi-machine-v1'))`）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SecretScheme {
    /// 平文（blob = UTF-8 の平文）。非 Windows の既定（設計 §5.8）。
    PlainV1,
    /// DPAPI の machine スコープ（blob = 暗号文）。Windows の既定。
    DpapiMachineV1,
}

impl SecretScheme {
    /// DB の列に入れる名前。変えると保存済みの行が読めなくなる。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlainV1 => "plain-v1",
            Self::DpapiMachineV1 => "dpapi-machine-v1",
        }
    }

    /// DB の列の値から読む。知らない名前は [`SecretError::UnknownScheme`]
    /// （値は載せない）。
    pub fn parse(value: &str) -> Result<Self, SecretError> {
        match value {
            "plain-v1" => Ok(Self::PlainV1),
            "dpapi-machine-v1" => Ok(Self::DpapiMachineV1),
            _ => Err(SecretError::UnknownScheme),
        }
    }
}

impl fmt::Display for SecretScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SecretScheme {
    type Err = SecretError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// 秘密値の参照: 128 ビットの乱数の小文字 16 進 32 文字（設計 §3.3）。
///
/// 接続の `settings` の `{"$secret": "<ref>"}` と `plc_connection_secrets.ref` に入る。
/// 秘密ではない（`Debug`・`Display` に出してよい）が、entropy に入るので、行どうしで
/// blob を入れ替えても開けない。
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    /// OS の乱数源（`getrandom`）から新しい参照を作る。
    pub fn generate() -> Result<Self, SecretError> {
        let mut bytes = [0u8; REF_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| SecretError::Random)?;
        Ok(Self(hex::encode(bytes)))
    }

    /// 保存済みの参照を読む。ちょうど 32 文字の小文字 16 進だけを受ける（DB の
    /// `CHECK (length(ref) = 32 AND ref NOT GLOB '*[^0-9a-f]*')` と同じ）。
    /// 外れれば [`SecretError::InvalidRef`]（値は載せない）。
    pub fn parse(value: &str) -> Result<Self, SecretError> {
        let ok = value.len() == SECRET_REF_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(SecretError::InvalidRef)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SecretRef {
    type Err = SecretError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// 封印した秘密値（DB の `plc_connection_secrets` の 1 行の `ref`・`scheme`・`blob`）。
///
/// `blob` は `plain-v1` なら平文そのものなので `Zeroizing` で持ち、`Debug` には出さない
/// （参照と方式だけ）。
#[derive(Clone, Eq, PartialEq)]
pub struct SealedSecret {
    pub r#ref: SecretRef,
    pub scheme: SecretScheme,
    pub blob: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSecret")
            .field("ref", &self.r#ref.as_str())
            .field("scheme", &self.scheme.as_str())
            .finish_non_exhaustive()
    }
}

/// 秘密値が読めない理由（設計 §5.5。REST・状態の画面・`unavailableConnections` の
/// `reason` に出る）。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum UnreadableReason {
    /// `settings` に参照があるのに秘密の行が無い（呼び出し側が DB で見つけて作る）。
    Missing,
    /// 行はあるが開けない: DPAPI の失敗（別の PC・OS の入れ直し・壊れた blob・
    /// entropy の違い）、`dpapi-machine-v1` を protector の無い PC（非 Windows）で開いた、
    /// 開いた値が UTF-8 でない・空（空は封印しないので、壊れた行）。
    Undecryptable,
}

impl UnreadableReason {
    /// REST の `reason` の綴り（`missing` / `undecryptable`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Undecryptable => "undecryptable",
        }
    }
}

/// 接続の秘密のキー 1 つの状態（設計 §5.5 の `passwordState`）。
///
/// 「無い」（[`SecretState::None`]）と「読めない」（[`SecretState::Unreadable`]）を分ける。
/// 読めないときに空の値として繋ぎに行かない。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum SecretState {
    /// 値が設定されていない（参照が無い）。
    None,
    /// 値がある（参照と行があり、開ける見込み）。
    Set,
    /// 参照はあるが読めない。
    Unreadable { reason: UnreadableReason },
}

impl SecretState {
    /// REST の `passwordState` の綴り（`none` / `set` / `unreadable`）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Set => "set",
            Self::Unreadable { .. } => "unreadable",
        }
    }
}

/// キーの名前が使えない理由。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidKey {
    Empty,
    TooLong,
    /// ASCII の英数字・`_`・`-`・`.` 以外を含む（`:` は entropy の区切りなので特に断る）。
    BadChar,
}

/// このモジュールのすべての失敗。**どの variant も平文・blob・入力の文字列を持たない**
/// （`Display` と `Debug` にもそれ以上は出ない）。包む [`SecretStoreError`] も同じ約束
/// （操作の名前と OS のエラー番号だけ）。
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    #[error("秘密値のキーの名前が使えません（{0:?}）")]
    InvalidKey(InvalidKey),
    #[error("秘密値の参照の形が違います（32 文字の小文字 16 進）")]
    InvalidRef,
    #[error("秘密値の封印の方式を知りません")]
    UnknownScheme,
    #[error("空の秘密値は封印しません（空は値を消す意味です）")]
    EmptyValue,
    #[error("秘密値が大きすぎます")]
    TooLarge,
    #[error("秘密値の参照の乱数を作れませんでした")]
    Random,
    #[error("秘密値の封印に失敗しました: {0}")]
    Protect(SecretStoreError),
    #[error("秘密値を開けませんでした（別の PC からのコピー・OS の入れ直し・改ざんなど）: {0}")]
    Unprotect(SecretStoreError),
    #[error("この PC では {0} の秘密値を開けません（封印の部品がありません）")]
    ProtectorUnavailable(SecretScheme),
    #[error("開いた秘密値が UTF-8 ではありません")]
    NotUtf8,
    /// 開いた値が空。空は封印しない（[`SecretError::EmptyValue`]）ので、保存された行が
    /// 壊れている。空のパスワードとして使わない（空に潰さない）。
    #[error("開いた秘密値が空です（保存された値が壊れています）")]
    EmptyStored,
}

impl SecretError {
    /// 開封の失敗を「読めない」の理由に写す（[`ConnectionSecrets::open`] の失敗のうち、
    /// 保存された値のせいで読めないもの）。入力の誤り（キー・参照の形）や封印の側の
    /// 失敗は呼び出し側の不具合なので `None`。
    pub fn unreadable_reason(&self) -> Option<UnreadableReason> {
        match self {
            Self::Unprotect(_)
            | Self::ProtectorUnavailable(_)
            | Self::NotUtf8
            | Self::EmptyStored => Some(UnreadableReason::Undecryptable),
            Self::InvalidKey(_)
            | Self::InvalidRef
            | Self::UnknownScheme
            | Self::EmptyValue
            | Self::TooLarge
            | Self::Random
            | Self::Protect(_) => None,
        }
    }
}

/// キーの名前を確かめる: 1〜[`MAX_KEY_LEN`] バイトの ASCII の英数字・`_`・`-`・`.`。
///
/// キーはドライバの `secret_keys()` が返す設定のキー（`password` など、snake_case）。
/// entropy の区切りの `:` と、ログ・監査（`secretsChanged`）に出しても困らない文字だけに絞る。
pub fn validate_key(key: &str) -> Result<(), SecretError> {
    if key.is_empty() {
        return Err(SecretError::InvalidKey(InvalidKey::Empty));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(SecretError::InvalidKey(InvalidKey::TooLong));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(SecretError::InvalidKey(InvalidKey::BadChar));
    }
    Ok(())
}

/// 封印の entropy（`"banto-hub/plc-connection-secret/v1:" + ref + ":" + key`、設計 §5.2）。
fn entropy_for(r#ref: &SecretRef, key: &str) -> Vec<u8> {
    let mut entropy =
        Vec::with_capacity(ENTROPY_PREFIX.len() + r#ref.as_str().len() + 1 + key.len());
    entropy.extend_from_slice(ENTROPY_PREFIX.as_bytes());
    entropy.extend_from_slice(r#ref.as_str().as_bytes());
    entropy.push(b':');
    entropy.extend_from_slice(key.as_bytes());
    entropy
}

/// 平文のバイト列を `Zeroizing<String>` にする。UTF-8 でなければ、バイト列を消してから
/// [`SecretError::NotUtf8`]（エラーにバイト列を載せない）。
fn into_zeroizing_string(mut bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>, SecretError> {
    // `take` で中身を取り出す（`Zeroizing` 側には空の Vec が残る）。`String::from_utf8` は
    // 確保し直さないので、平文は同じバッファのまま `Zeroizing<String>` に移る。
    let raw = std::mem::take(&mut *bytes);
    match String::from_utf8(raw) {
        Ok(text) => Ok(Zeroizing::new(text)),
        Err(error) => {
            let mut raw = error.into_bytes();
            raw.zeroize();
            Err(SecretError::NotUtf8)
        }
    }
}

/// 接続の秘密値の封印と開封（設計 §5.2）。
///
/// `protector` があれば `dpapi-machine-v1`、無ければ `plain-v1` で封印する。製品は
/// [`ConnectionSecrets::platform_default`]（Windows は `DpapiMachine`、それ以外は無し）、
/// テストは [`ConnectionSecrets::new`] で偽の protector を差し込める。
#[derive(Clone)]
pub struct ConnectionSecrets {
    protector: Option<Arc<dyn Protector>>,
}

impl fmt::Debug for ConnectionSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionSecrets")
            .field("seal_scheme", &self.seal_scheme().as_str())
            .finish()
    }
}

impl ConnectionSecrets {
    /// protector を指定して作る（`None` なら `plain-v1`）。テストで偽の protector を
    /// 差し込む口でもある。
    pub fn new(protector: Option<Arc<dyn Protector>>) -> Self {
        Self { protector }
    }

    /// この OS の既定: Windows は DPAPI の machine スコープ（`dpapi-machine-v1`）、
    /// それ以外は protector 無し（`plain-v1`、設計 §5.8）。
    pub fn platform_default() -> Self {
        #[cfg(windows)]
        {
            Self::new(Some(Arc::new(banto_secret_store::DpapiMachine)))
        }
        #[cfg(not(windows))]
        {
            Self::new(None)
        }
    }

    /// [`ConnectionSecrets::seal`] が使う方式。`PlainV1` なら平文で保存していることを
    /// 起動時のログ・状態の画面で警告する（設計 §5.8。警告するのは呼び出し側）。
    pub fn seal_scheme(&self) -> SecretScheme {
        if self.protector.is_some() {
            SecretScheme::DpapiMachineV1
        } else {
            SecretScheme::PlainV1
        }
    }

    /// 平文を新しい参照で封印する。参照は呼ぶたびに新しく作る（更新の `Set` も新しい
    /// 参照の行を足して古い行を消す、設計 §5.3）。
    ///
    /// 空の値は [`SecretError::EmptyValue`]（空は値を消す `Clear` で、封印しない）。
    pub fn seal(&self, key: &str, plain: &str) -> Result<SealedSecret, SecretError> {
        validate_key(key)?;
        let r#ref = SecretRef::generate()?;
        self.seal_with_ref(r#ref, key, plain)
    }

    fn seal_with_ref(
        &self,
        r#ref: SecretRef,
        key: &str,
        plain: &str,
    ) -> Result<SealedSecret, SecretError> {
        if plain.is_empty() {
            return Err(SecretError::EmptyValue);
        }
        if plain.len() > MAX_SECRET_LEN {
            return Err(SecretError::TooLarge);
        }
        let (scheme, blob) = match &self.protector {
            Some(protector) => {
                let entropy = entropy_for(&r#ref, key);
                let blob = protector
                    .protect(plain.as_bytes(), &entropy)
                    .map_err(SecretError::Protect)?;
                (SecretScheme::DpapiMachineV1, Zeroizing::new(blob))
            }
            None => (
                SecretScheme::PlainV1,
                Zeroizing::new(plain.as_bytes().to_vec()),
            ),
        };
        Ok(SealedSecret {
            r#ref,
            scheme,
            blob,
        })
    }

    /// 封印した値を開く。平文は `Zeroizing<String>` で返す。使う直前（DB への接続を張る・
    /// 設定を返す応答を組む）にだけ呼ぶ（設計 §5.2）。
    ///
    /// - `plain-v1`: blob を UTF-8 として読む（protector の有無を問わない。封印し直す前の行）。
    /// - `dpapi-machine-v1`: protector で entropy（参照とキー）を付けて開く。protector が
    ///   無ければ [`SecretError::ProtectorUnavailable`]。
    ///
    /// 開いた値が空なら [`SecretError::EmptyStored`]（空は封印しないので壊れた行）。
    /// 保存された値のせいで開けない失敗は [`SecretError::unreadable_reason`] で
    /// [`UnreadableReason::Undecryptable`] に写る。空の値に潰さない。
    pub fn open(&self, key: &str, sealed: &SealedSecret) -> Result<Zeroizing<String>, SecretError> {
        validate_key(key)?;
        let plain = match sealed.scheme {
            SecretScheme::PlainV1 => Zeroizing::new(sealed.blob.to_vec()),
            SecretScheme::DpapiMachineV1 => {
                let protector = self
                    .protector
                    .as_ref()
                    .ok_or(SecretError::ProtectorUnavailable(sealed.scheme))?;
                let entropy = entropy_for(&sealed.r#ref, key);
                protector
                    .unprotect(&sealed.blob, &entropy)
                    .map_err(SecretError::Unprotect)?
            }
        };
        let plain = into_zeroizing_string(plain)?;
        if plain.is_empty() {
            return Err(SecretError::EmptyStored);
        }
        Ok(plain)
    }

    /// `plain-v1` の値を、**同じ参照のまま** protector で封印し直す（設計 §6.5 の起動時の
    /// 後処理と、非 Windows で積んだキューの `sealedPassword`）。
    ///
    /// 封印し直したら `Some`。`plain-v1` でない・protector が無い（非 Windows）なら何もせず
    /// `None`。`plain-v1` の blob が UTF-8 でなければ [`SecretError::NotUtf8`]。
    pub fn reseal_plain(
        &self,
        key: &str,
        sealed: &SealedSecret,
    ) -> Result<Option<SealedSecret>, SecretError> {
        validate_key(key)?;
        if sealed.scheme != SecretScheme::PlainV1 || self.protector.is_none() {
            return Ok(None);
        }
        let plain = self.open(key, sealed)?;
        let resealed = self.seal_with_ref(sealed.r#ref.clone(), key, &plain)?;
        debug_assert_eq!(resealed.scheme, SecretScheme::DpapiMachineV1);
        Ok(Some(resealed))
    }
}

// ---- R2b-1d: banto-tags の秘密の行との橋渡し（設計 §5.3・§5.4） ----------------------------
//
// banto-tags はこの crate に依存できないので、封印した値を同じ中身の
// `banto_tags::SealedSecretRow`（`ref`・`scheme`・`blob`）で運ぶ。変換はここに置く。
//
// **R2b-1d（part A）の範囲**: 封印は全 OS で `plain-v1`（[`r2b1d_connection_secrets`]。設計 §10 の
// R2b-1d の行「Windows でもこの PR では `plain-v1`」）。Windows の `dpapi-machine-v1` への
// 切り替えと起動時の封印し直しは R2b-1e。

/// R2b-1d の封印・開封に使う [`ConnectionSecrets`]: protector 無し（`plain-v1`）。
///
/// TODO(R2b-1e): `ConnectionSecrets::platform_default()` に替え、起動時に `plain-v1` の行を
/// 封印し直す（設計 §6.5）。開封は今も `dpapi-machine-v1` を知っている（protector が無ければ
/// [`SecretError::ProtectorUnavailable`] = 読めない）。
pub fn r2b1d_connection_secrets() -> ConnectionSecrets {
    ConnectionSecrets::new(None)
}

impl From<SealedSecret> for banto_tags::SealedSecretRow {
    fn from(sealed: SealedSecret) -> Self {
        banto_tags::SealedSecretRow {
            r#ref: sealed.r#ref.as_str().to_owned(),
            scheme: sealed.scheme.as_str().to_owned(),
            blob: sealed.blob,
        }
    }
}

impl SealedSecret {
    /// banto-tags の行から読む（参照と方式を確かめ直す。外れれば
    /// [`SecretError::InvalidRef`] / [`SecretError::UnknownScheme`]、値は載せない）。
    pub fn from_row(row: &banto_tags::SealedSecretRow) -> Result<Self, SecretError> {
        Ok(SealedSecret {
            r#ref: SecretRef::parse(&row.r#ref)?,
            scheme: SecretScheme::parse(&row.scheme)?,
            blob: row.blob.clone(),
        })
    }
}

/// REST・MCP・未適用キューの平らな形の `password`（tri-state）を、banto-tags の
/// [`banto_tags::SecretChanges`] に写す（設計 §5.3）。平文はここで封印し、banto-tags へは
/// 封印した値だけを渡す。
///
/// | `password`        | 結果                                                  |
/// | ----------------- | ----------------------------------------------------- |
/// | `None`（省略・`null`） | 空（= `Keep`。作成では「パスワード無し」）      |
/// | `Some("")`        | `Clear`（作成では「パスワード無し」、更新では消す）  |
/// | `Some(値)`        | `Set(封印した値)`（新しい参照）                       |
///
/// `postgres` 以外のプロトコルに値を送ったときは、`Set` を受けた banto-tags が今と同じ
/// 「postgres接続以外では指定できません」で断る（封印した値はどこにも書かれない）。
pub fn password_changes(
    secrets: &ConnectionSecrets,
    password: Option<&str>,
) -> Result<banto_tags::SecretChanges, SecretError> {
    let mut changes = banto_tags::SecretChanges::new();
    match password {
        None => {}
        Some("") => {
            changes.insert(
                banto_tags::PASSWORD_KEY.to_owned(),
                banto_tags::SecretChange::Clear,
            );
        }
        Some(plain) => {
            let sealed = secrets.seal(banto_tags::PASSWORD_KEY, plain)?;
            changes.insert(
                banto_tags::PASSWORD_KEY.to_owned(),
                banto_tags::SecretChange::Set(sealed.into()),
            );
        }
    }
    Ok(changes)
}

/// 接続 `conn` の秘密のキー `key` を開く（設計 §5.4。使う直前にだけ呼ぶ）。
///
/// - `settings[key]` に参照が無い → `Ok(None)`（値が設定されていない）。
/// - 参照があるのに、その参照・接続・キーの行が `rows` に無い → `Err(Missing)`。
/// - 行はあるが開けない（方式・参照の形が違う、DPAPI の失敗、空・UTF-8 でない）→
///   `Err(Undecryptable)`。
///
/// **「無い」と「読めない」を分ける**（checklist §5「エラーを空に潰さない」、設計 §5.5）。
/// 呼び出し側は `Err` を空のパスワードとして使わない。
pub fn open_connection_secret(
    secrets: &ConnectionSecrets,
    conn: &banto_tags::PlcConnection,
    key: &str,
    rows: &[banto_tags::StoredSecretRow],
) -> Result<Option<Zeroizing<String>>, UnreadableReason> {
    let Some(r#ref) = conn.secret_ref(key) else {
        return Ok(None);
    };
    let row = rows
        .iter()
        .find(|row| row.sealed.r#ref == r#ref && row.connection_id == conn.id && row.key == key)
        .ok_or(UnreadableReason::Missing)?;
    let sealed =
        SealedSecret::from_row(&row.sealed).map_err(|_| UnreadableReason::Undecryptable)?;
    secrets.open(key, &sealed).map(Some).map_err(|error| {
        error
            .unreadable_reason()
            .unwrap_or(UnreadableReason::Undecryptable)
    })
}

/// 接続 `conn` の秘密のキー `key` の状態（設計 §5.5 の `passwordState`）。開けるかを
/// 実際に開いて確かめ、平文はすぐ捨てる（`Zeroizing`）。参照が無い → [`SecretState::None`]、
/// 開ける → [`SecretState::Set`]、行が無い・開けない → [`SecretState::Unreadable`]。
pub fn secret_state(
    secrets: &ConnectionSecrets,
    conn: &banto_tags::PlcConnection,
    key: &str,
    rows: &[banto_tags::StoredSecretRow],
) -> SecretState {
    match open_connection_secret(secrets, conn, key, rows) {
        Ok(None) => SecretState::None,
        Ok(Some(_plain)) => SecretState::Set,
        Err(reason) => SecretState::Unreadable { reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use banto_secret_store::Result as StoreResult;
    use sha2::{Digest, Sha256};

    const SECRET: &str = "pg_TOPSECRET_pass-7f3a";
    const KEY: &str = "password";

    /// テスト用の protector: entropy で鍵を作る XOR（暗号としての強さは無い）。
    ///
    /// blob = `sha256("tag" + entropy)`（32 バイト）+ 平文 XOR `sha256("ks" + entropy + i)`
    /// の鍵の流れ。開くときは渡された entropy のタグが一致しなければ失敗するので、
    /// **entropy の束縛**を全 OS で確かめられ、blob に平文がそのまま入らない。
    /// DPAPI と同じく、失敗には平文も blob も載せない。
    #[derive(Debug, Default)]
    struct EntropyXorProtector;

    fn tag(entropy: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"tag");
        h.update(entropy);
        h.finalize().into()
    }

    fn keystream(entropy: &[u8], len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut counter: u64 = 0;
        while out.len() < len {
            let mut h = Sha256::new();
            h.update(b"ks");
            h.update(entropy);
            h.update(counter.to_le_bytes());
            out.extend_from_slice(&h.finalize());
            counter += 1;
        }
        out.truncate(len);
        out
    }

    impl Protector for EntropyXorProtector {
        fn protect(&self, plain: &[u8], entropy: &[u8]) -> StoreResult<Vec<u8>> {
            let mut blob = tag(entropy).to_vec();
            blob.extend(
                plain
                    .iter()
                    .zip(keystream(entropy, plain.len()))
                    .map(|(p, k)| p ^ k),
            );
            Ok(blob)
        }

        fn unprotect(&self, blob: &[u8], entropy: &[u8]) -> StoreResult<Zeroizing<Vec<u8>>> {
            let fail = SecretStoreError::Unprotect { os_error: None };
            if blob.len() < 32 || blob[..32] != tag(entropy) {
                return Err(fail);
            }
            let body = &blob[32..];
            Ok(Zeroizing::new(
                body.iter()
                    .zip(keystream(entropy, body.len()))
                    .map(|(c, k)| c ^ k)
                    .collect(),
            ))
        }
    }

    /// 開くときに常に決まったバイト列（UTF-8 でない・秘密の印入り）を返す protector。
    #[derive(Debug, Default)]
    struct BadUtf8Protector;

    const BAD_UTF8: &[u8] = b"LEAKMARK\xff\xfe";

    impl Protector for BadUtf8Protector {
        fn protect(&self, _plain: &[u8], _entropy: &[u8]) -> StoreResult<Vec<u8>> {
            Ok(b"opaque".to_vec())
        }

        fn unprotect(&self, _blob: &[u8], _entropy: &[u8]) -> StoreResult<Zeroizing<Vec<u8>>> {
            Ok(Zeroizing::new(BAD_UTF8.to_vec()))
        }
    }

    fn protected() -> ConnectionSecrets {
        ConnectionSecrets::new(Some(Arc::new(EntropyXorProtector)))
    }

    fn plain() -> ConnectionSecrets {
        ConnectionSecrets::new(None)
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// `shown` に平文も blob の中身も出ていないこと。
    fn assert_no_leak(shown: &str, sealed: &SealedSecret) {
        assert!(!shown.contains(SECRET), "平文が出ている: {shown}");
        assert!(
            !shown.contains("TOPSECRET"),
            "平文の一部が出ている: {shown}"
        );
        let hex_blob = hex::encode(&*sealed.blob);
        assert!(
            !shown.contains(&hex_blob),
            "blob の hex が出ている: {shown}"
        );
        assert!(
            !shown.contains(&format!("{:?}", *sealed.blob)),
            "blob のバイト列が出ている: {shown}"
        );
    }

    #[test]
    fn entropy_is_exactly_the_documented_format() {
        let r = SecretRef::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            entropy_for(&r, "password"),
            b"banto-hub/plc-connection-secret/v1:0123456789abcdef0123456789abcdef:password"
        );
    }

    #[test]
    fn plain_v1_round_trips_and_its_blob_is_the_utf8_value() {
        let secrets = plain();
        assert_eq!(secrets.seal_scheme(), SecretScheme::PlainV1);
        let sealed = secrets.seal(KEY, SECRET).unwrap();
        assert_eq!(sealed.scheme, SecretScheme::PlainV1);
        assert_eq!(&**sealed.blob, SECRET.as_bytes());
        assert_eq!(&*secrets.open(KEY, &sealed).unwrap(), SECRET);
        // protector がある側でも plain-v1 は開ける（封印し直す前の行）。
        assert_eq!(&*protected().open(KEY, &sealed).unwrap(), SECRET);
    }

    #[test]
    fn protected_round_trips_and_the_blob_does_not_contain_the_plaintext() {
        let secrets = protected();
        assert_eq!(secrets.seal_scheme(), SecretScheme::DpapiMachineV1);
        let sealed = secrets.seal(KEY, SECRET).unwrap();
        assert_eq!(sealed.scheme, SecretScheme::DpapiMachineV1);
        assert!(!contains(&sealed.blob, SECRET.as_bytes()));
        assert_eq!(&*secrets.open(KEY, &sealed).unwrap(), SECRET);
        // 非 ASCII も往復する。
        let jp = secrets.seal(KEY, "パスワード✓").unwrap();
        assert_eq!(&*secrets.open(KEY, &jp).unwrap(), "パスワード✓");
    }

    #[test]
    fn each_seal_gets_a_new_valid_ref() {
        let secrets = protected();
        let a = secrets.seal(KEY, SECRET).unwrap();
        let b = secrets.seal(KEY, SECRET).unwrap();
        assert_ne!(a.r#ref, b.r#ref);
        assert_ne!(a.blob, b.blob, "参照が entropy に入るので blob も違う");
        for r in [&a.r#ref, &b.r#ref] {
            assert_eq!(SecretRef::parse(r.as_str()).unwrap(), *r);
        }
    }

    #[test]
    fn a_blob_cannot_be_opened_under_another_ref_or_key() {
        let secrets = protected();
        let a = secrets.seal(KEY, SECRET).unwrap();
        let b = secrets.seal(KEY, "other").unwrap();

        // 行 A の blob を行 B（別の参照）へ写しても開けない。
        let moved = SealedSecret {
            r#ref: b.r#ref.clone(),
            scheme: a.scheme,
            blob: a.blob.clone(),
        };
        let err = secrets.open(KEY, &moved).unwrap_err();
        assert!(matches!(err, SecretError::Unprotect(_)), "{err:?}");
        assert_eq!(
            err.unreadable_reason(),
            Some(UnreadableReason::Undecryptable)
        );
        assert_no_leak(&format!("{err} {err:?}"), &a);

        // 同じ参照でも別のキーでは開けない。
        let err = secrets.open("api_key", &a).unwrap_err();
        assert!(matches!(err, SecretError::Unprotect(_)), "{err:?}");

        // 壊れた blob も開けない。
        let mut tampered = a.clone();
        tampered.blob[0] ^= 0x55;
        assert!(matches!(
            secrets.open(KEY, &tampered),
            Err(SecretError::Unprotect(_))
        ));
    }

    #[test]
    fn dpapi_scheme_without_a_protector_is_undecryptable_not_empty() {
        let sealed = protected().seal(KEY, SECRET).unwrap();
        let err = plain().open(KEY, &sealed).unwrap_err();
        assert_eq!(
            err,
            SecretError::ProtectorUnavailable(SecretScheme::DpapiMachineV1)
        );
        assert_eq!(
            err.unreadable_reason(),
            Some(UnreadableReason::Undecryptable)
        );
        assert_no_leak(&format!("{err} {err:?}"), &sealed);
    }

    #[test]
    fn invalid_utf8_is_an_error_without_the_bytes() {
        // plain-v1 の blob が UTF-8 でない。
        let sealed = SealedSecret {
            r#ref: SecretRef::generate().unwrap(),
            scheme: SecretScheme::PlainV1,
            blob: Zeroizing::new(BAD_UTF8.to_vec()),
        };
        for secrets in [plain(), protected()] {
            let err = secrets.open(KEY, &sealed).unwrap_err();
            assert_eq!(err, SecretError::NotUtf8);
            assert_eq!(
                err.unreadable_reason(),
                Some(UnreadableReason::Undecryptable)
            );
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains("LEAKMARK"), "{shown}");
            assert!(!shown.contains("255"), "{shown}");
            assert!(!shown.to_lowercase().contains("ff"), "{shown}");
        }
        // protector が UTF-8 でない平文を返した。
        let secrets = ConnectionSecrets::new(Some(Arc::new(BadUtf8Protector)));
        let sealed = secrets.seal(KEY, SECRET).unwrap();
        let err = secrets.open(KEY, &sealed).unwrap_err();
        assert_eq!(err, SecretError::NotUtf8);
        assert!(!format!("{err} {err:?}").contains("LEAKMARK"));
    }

    #[test]
    fn debug_and_display_never_show_the_plaintext_or_the_blob() {
        for secrets in [plain(), protected()] {
            let sealed = secrets.seal(KEY, SECRET).unwrap();
            let shown = format!("{sealed:?} {sealed:#?}");
            assert!(shown.contains(sealed.r#ref.as_str()), "{shown}");
            assert!(shown.contains(sealed.scheme.as_str()), "{shown}");
            assert_no_leak(&shown, &sealed);

            let shown = format!("{secrets:?} {secrets:#?}");
            assert!(shown.contains(secrets.seal_scheme().as_str()), "{shown}");
            assert_no_leak(&shown, &sealed);

            let opened = secrets.open(KEY, &sealed).unwrap();
            assert_eq!(&*opened, SECRET);
        }

        let sealed = plain().seal(KEY, SECRET).unwrap();
        let errors = [
            SecretError::InvalidKey(InvalidKey::BadChar),
            SecretError::InvalidRef,
            SecretError::UnknownScheme,
            SecretError::EmptyValue,
            SecretError::TooLarge,
            SecretError::Random,
            SecretError::Protect(SecretStoreError::Protect { os_error: Some(5) }),
            SecretError::Unprotect(SecretStoreError::Unprotect { os_error: Some(13) }),
            SecretError::ProtectorUnavailable(SecretScheme::DpapiMachineV1),
            SecretError::NotUtf8,
            SecretError::EmptyStored,
        ];
        for err in errors {
            assert_no_leak(&format!("{err} {err:?}"), &sealed);
        }
    }

    #[test]
    fn reseal_plain_moves_plain_v1_to_the_protected_scheme_keeping_the_ref() {
        let sealed = plain().seal(KEY, SECRET).unwrap();
        let secrets = protected();
        let resealed = secrets.reseal_plain(KEY, &sealed).unwrap().unwrap();
        assert_eq!(resealed.r#ref, sealed.r#ref);
        assert_eq!(resealed.scheme, SecretScheme::DpapiMachineV1);
        assert!(!contains(&resealed.blob, SECRET.as_bytes()));
        assert_eq!(&*secrets.open(KEY, &resealed).unwrap(), SECRET);
        // 封印し直した値は同じ参照・同じキーに束縛されている。
        assert!(secrets.open("api_key", &resealed).is_err());
    }

    #[test]
    fn reseal_plain_is_none_when_nothing_to_do() {
        // protected の値は封印し直さない。
        let secrets = protected();
        let sealed = secrets.seal(KEY, SECRET).unwrap();
        assert_eq!(secrets.reseal_plain(KEY, &sealed).unwrap(), None);
        // protector が無い（非 Windows）ときは plain-v1 のまま。
        let plain_sealed = plain().seal(KEY, SECRET).unwrap();
        assert_eq!(plain().reseal_plain(KEY, &plain_sealed).unwrap(), None);
        assert_eq!(plain().reseal_plain(KEY, &sealed).unwrap(), None);
        // UTF-8 でない plain-v1 は封印し直さずに失敗する。
        let bad = SealedSecret {
            r#ref: SecretRef::generate().unwrap(),
            scheme: SecretScheme::PlainV1,
            blob: Zeroizing::new(BAD_UTF8.to_vec()),
        };
        assert_eq!(
            secrets.reseal_plain(KEY, &bad).unwrap_err(),
            SecretError::NotUtf8
        );
    }

    #[test]
    fn scheme_names_round_trip_and_unknown_is_rejected() {
        for scheme in [SecretScheme::PlainV1, SecretScheme::DpapiMachineV1] {
            assert_eq!(SecretScheme::parse(scheme.as_str()).unwrap(), scheme);
            assert_eq!(scheme.to_string().parse::<SecretScheme>().unwrap(), scheme);
        }
        assert_eq!(SecretScheme::PlainV1.as_str(), "plain-v1");
        assert_eq!(SecretScheme::DpapiMachineV1.as_str(), "dpapi-machine-v1");
        for bad in ["", "PLAIN-V1", "plain-v2", "dpapi-machine", " plain-v1"] {
            assert_eq!(SecretScheme::parse(bad), Err(SecretError::UnknownScheme));
        }
    }

    #[test]
    fn ref_parse_accepts_only_32_lowercase_hex() {
        let good = "0123456789abcdef0123456789abcdef";
        let r = SecretRef::parse(good).unwrap();
        assert_eq!(r.as_str(), good);
        assert_eq!(r.to_string().parse::<SecretRef>().unwrap(), r);
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",   // 31
            "0123456789abcdef0123456789abcdef0", // 33
            "0123456789ABCDEF0123456789abcdef",  // 大文字
            "0123456789abcdeg0123456789abcdef",  // g
            "0123456789abcdef0123456789abcde ",
            "０123456789abcdef0123456789abcde", // 全角（バイト長も違う）
        ] {
            assert_eq!(
                SecretRef::parse(bad),
                Err(SecretError::InvalidRef),
                "{bad:?}"
            );
        }
        // 作った参照は DB の CHECK と同じ形。
        for _ in 0..32 {
            let r = SecretRef::generate().unwrap();
            assert_eq!(r.as_str().len(), SECRET_REF_LEN);
            assert!(SecretRef::parse(r.as_str()).is_ok());
        }
    }

    #[test]
    fn keys_are_validated_on_every_entry() {
        let secrets = protected();
        let sealed = secrets.seal(KEY, SECRET).unwrap();
        let long = "k".repeat(MAX_KEY_LEN + 1);
        let cases = [
            ("", InvalidKey::Empty),
            (long.as_str(), InvalidKey::TooLong),
            ("pass:word", InvalidKey::BadChar),
            ("pass word", InvalidKey::BadChar),
            ("パスワード", InvalidKey::BadChar),
        ];
        for (key, why) in cases {
            let want = Err(SecretError::InvalidKey(why));
            assert_eq!(secrets.seal(key, SECRET).map(|_| ()), want.clone());
            assert_eq!(secrets.open(key, &sealed).map(|_| ()), want.clone());
            assert_eq!(secrets.reseal_plain(key, &sealed).map(|_| ()), want);
        }
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN)).is_ok());
        assert!(validate_key("api_key.v-1").is_ok());
    }

    #[test]
    fn an_empty_stored_value_is_undecryptable_not_an_empty_password() {
        let r = SecretRef::generate().unwrap();
        let empty_plain = SealedSecret {
            r#ref: r.clone(),
            scheme: SecretScheme::PlainV1,
            blob: Zeroizing::new(Vec::new()),
        };
        let secrets = protected();
        let err = secrets.open(KEY, &empty_plain).unwrap_err();
        assert_eq!(err, SecretError::EmptyStored);
        assert_eq!(
            err.unreadable_reason(),
            Some(UnreadableReason::Undecryptable)
        );
        assert_eq!(
            secrets.reseal_plain(KEY, &empty_plain).unwrap_err(),
            SecretError::EmptyStored
        );
        // 保護された blob が空の平文に開いた場合も同じ。
        let entropy = entropy_for(&r, KEY);
        let empty_protected = SealedSecret {
            r#ref: r,
            scheme: SecretScheme::DpapiMachineV1,
            blob: Zeroizing::new(EntropyXorProtector.protect(b"", &entropy).unwrap()),
        };
        assert_eq!(
            secrets.open(KEY, &empty_protected).unwrap_err(),
            SecretError::EmptyStored
        );
    }

    #[test]
    fn empty_and_oversized_values_are_not_sealed() {
        for secrets in [plain(), protected()] {
            assert_eq!(secrets.seal(KEY, "").unwrap_err(), SecretError::EmptyValue);
            let huge = "x".repeat(MAX_SECRET_LEN + 1);
            assert_eq!(secrets.seal(KEY, &huge).unwrap_err(), SecretError::TooLarge);
            assert!(secrets.seal(KEY, &"x".repeat(MAX_SECRET_LEN)).is_ok());
        }
    }

    #[test]
    fn states_and_reasons_have_stable_names() {
        assert_eq!(SecretState::None.as_str(), "none");
        assert_eq!(SecretState::Set.as_str(), "set");
        for reason in [UnreadableReason::Missing, UnreadableReason::Undecryptable] {
            assert_eq!(SecretState::Unreadable { reason }.as_str(), "unreadable");
        }
        assert_eq!(UnreadableReason::Missing.as_str(), "missing");
        assert_eq!(UnreadableReason::Undecryptable.as_str(), "undecryptable");
        // 入力の誤り・封印の側の失敗は「読めない」に写らない。
        assert_eq!(SecretError::InvalidRef.unreadable_reason(), None);
        assert_eq!(SecretError::EmptyValue.unreadable_reason(), None);
        assert_eq!(
            SecretError::Protect(SecretStoreError::Protect { os_error: None }).unreadable_reason(),
            None
        );
    }

    #[test]
    fn platform_default_scheme_matches_the_os() {
        let want = if cfg!(windows) {
            SecretScheme::DpapiMachineV1
        } else {
            SecretScheme::PlainV1
        };
        assert_eq!(ConnectionSecrets::platform_default().seal_scheme(), want);
    }

    /// 本物の DPAPI（machine スコープ）。windows-latest の CI で走る。
    #[cfg(windows)]
    mod dpapi_machine {
        use super::*;

        fn dpapi() -> ConnectionSecrets {
            ConnectionSecrets::platform_default()
        }

        #[test]
        fn round_trips_and_the_blob_does_not_contain_the_plaintext() {
            let secrets = dpapi();
            let sealed = secrets.seal(KEY, SECRET).unwrap();
            assert_eq!(sealed.scheme, SecretScheme::DpapiMachineV1);
            assert!(!contains(&sealed.blob, SECRET.as_bytes()));
            assert_eq!(&*secrets.open(KEY, &sealed).unwrap(), SECRET);
            assert_no_leak(&format!("{sealed:?} {secrets:?}"), &sealed);
        }

        #[test]
        fn another_ref_or_key_cannot_open() {
            let secrets = dpapi();
            let a = secrets.seal(KEY, SECRET).unwrap();
            let b = secrets.seal(KEY, "other").unwrap();
            let moved = SealedSecret {
                r#ref: b.r#ref.clone(),
                scheme: a.scheme,
                blob: a.blob.clone(),
            };
            let err = secrets.open(KEY, &moved).unwrap_err();
            assert!(matches!(err, SecretError::Unprotect(_)), "{err:?}");
            assert_eq!(
                err.unreadable_reason(),
                Some(UnreadableReason::Undecryptable)
            );
            assert_no_leak(&format!("{err} {err:?}"), &a);
            assert!(matches!(
                secrets.open("api_key", &a),
                Err(SecretError::Unprotect(_))
            ));
        }

        #[test]
        fn reseal_plain_v1_into_dpapi() {
            let sealed = ConnectionSecrets::new(None).seal(KEY, SECRET).unwrap();
            let secrets = dpapi();
            let resealed = secrets.reseal_plain(KEY, &sealed).unwrap().unwrap();
            assert_eq!(resealed.r#ref, sealed.r#ref);
            assert_eq!(resealed.scheme, SecretScheme::DpapiMachineV1);
            assert!(!contains(&resealed.blob, SECRET.as_bytes()));
            assert_eq!(&*secrets.open(KEY, &resealed).unwrap(), SECRET);
        }
    }
}

/// R2b-1d: the bridge to banto-tags' secret rows.
#[cfg(test)]
mod r2b1d_tests {
    use super::*;

    fn connection(settings: serde_json::Value) -> banto_tags::PlcConnection {
        banto_tags::PlcConnection::from_settings(
            7,
            "pg".to_string(),
            "postgres".to_string(),
            true,
            false,
            settings.as_object().unwrap().clone(),
            1,
        )
    }

    #[test]
    fn password_changes_maps_the_rest_tri_state() {
        let secrets = r2b1d_connection_secrets();
        assert!(password_changes(&secrets, None).unwrap().is_empty());
        assert_eq!(
            password_changes(&secrets, Some("")).unwrap()["password"],
            banto_tags::SecretChange::Clear
        );
        let changes = password_changes(&secrets, Some("pw-1")).unwrap();
        let banto_tags::SecretChange::Set(row) = &changes["password"] else {
            panic!("expected Set: {changes:?}");
        };
        assert_eq!(row.scheme, "plain-v1");
        assert_eq!(row.blob.as_slice(), b"pw-1");
        assert!(banto_tags::connection_secret::is_valid_secret_ref(
            &row.r#ref
        ));
        // A new reference per call.
        let again = password_changes(&secrets, Some("pw-1")).unwrap();
        let banto_tags::SecretChange::Set(other) = &again["password"] else {
            panic!("expected Set");
        };
        assert_ne!(row.r#ref, other.r#ref);
        assert!(!format!("{changes:?}").contains("pw-1"));
    }

    #[test]
    fn open_connection_secret_tells_none_set_and_unreadable_apart() {
        let secrets = r2b1d_connection_secrets();
        let sealed = secrets.seal("password", "pw-2").unwrap();
        let r#ref = sealed.r#ref.as_str().to_owned();
        let row = banto_tags::StoredSecretRow {
            connection_id: 7,
            key: "password".to_string(),
            sealed: sealed.into(),
        };
        let with_ref = connection(serde_json::json!({ "password": { "$secret": r#ref } }));

        // No reference: no password.
        assert_eq!(
            open_connection_secret(
                &secrets,
                &connection(serde_json::json!({})),
                "password",
                &[]
            )
            .unwrap(),
            None
        );
        // Reference and row: the value.
        assert_eq!(
            open_connection_secret(&secrets, &with_ref, "password", std::slice::from_ref(&row))
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("pw-2")
        );
        // Reference without its row (or a row of another connection / key): Missing.
        for rows in [
            vec![],
            vec![banto_tags::StoredSecretRow {
                connection_id: 8,
                ..row.clone()
            }],
            vec![banto_tags::StoredSecretRow {
                key: "token".to_string(),
                ..row.clone()
            }],
        ] {
            assert_eq!(
                open_connection_secret(&secrets, &with_ref, "password", &rows),
                Err(UnreadableReason::Missing)
            );
        }
        // A row that cannot be opened here: Undecryptable (never an empty password).
        for sealed in [
            banto_tags::SealedSecretRow::new(r#ref.clone(), "dpapi-machine-v1", b"x".to_vec()),
            banto_tags::SealedSecretRow::new(r#ref.clone(), "rot13", b"x".to_vec()),
            banto_tags::SealedSecretRow::new(r#ref.clone(), "plain-v1", Vec::new()),
            banto_tags::SealedSecretRow::new(r#ref.clone(), "plain-v1", vec![0xff, 0xfe]),
        ] {
            let broken = banto_tags::StoredSecretRow {
                sealed,
                ..row.clone()
            };
            assert_eq!(
                open_connection_secret(&secrets, &with_ref, "password", &[broken]),
                Err(UnreadableReason::Undecryptable)
            );
        }
    }
}
