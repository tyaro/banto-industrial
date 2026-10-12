//! 秘密のファイルの形式（版 1）。純関数だけで、全 OS でテストする。
//!
//! ```text
//! offset  size  内容
//! 0       4     マジック b"BSS1"
//! 4       2     版数（u16 LE）= 1
//! 6       4     アカウントの長さ N（u32 LE、1..=MAX_ACCOUNT_LEN）
//! 10      N     アカウント（UTF-8）
//! 10+N    4     blob の長さ M（u32 LE、0..=MAX_BLOB_LEN）
//! 14+N    M     blob（Protector が返した暗号文）
//! ```
//!
//! 末尾に余分なバイトがあれば壊れているとみなす。アカウントは平文で入る
//! （秘密ではない。ファイル名は `sha256(account)` なので、中身を読まないと
//! どのアカウントのファイルかは分からない）。アカウントは改ざんできる平文の
//! 欄なので、**読むときの一致の確認だけでは取り違えを防げない** - 本当の防御は
//! entropy にアカウントを入れること（[`crate::entropy_for`]）。

use crate::error::CorruptKind;

pub(crate) const MAGIC: &[u8; 4] = b"BSS1";
pub(crate) const VERSION: u16 = 1;

/// アカウント文字列の長さの上限（バイト）。
pub const MAX_ACCOUNT_LEN: usize = 1024;
/// blob（暗号文）の長さの上限（バイト）。
pub const MAX_BLOB_LEN: usize = 64 * 1024;
/// 保存できる平文の長さの上限（バイト）。DPAPI の暗号文は平文より 200〜300
/// バイトほど長いので、[`MAX_BLOB_LEN`] に十分収まる値にしてある。
pub const MAX_SECRET_LEN: usize = 16 * 1024;
/// ファイル全体の長さの上限（読むときに、これより大きいファイルは読み切らない）。
pub(crate) const MAX_FILE_LEN: usize = 4 + 2 + 4 + MAX_ACCOUNT_LEN + 4 + MAX_BLOB_LEN;

/// 読み取った中身（`bytes` を借りる）。
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Decoded<'a> {
    pub(crate) account: &'a str,
    pub(crate) blob: &'a [u8],
}

/// 書き出す。`account` と `blob` の長さは呼び出し側が上限内であることを確かめ済み。
pub(crate) fn encode(account: &str, blob: &[u8]) -> Vec<u8> {
    debug_assert!(!account.is_empty() && account.len() <= MAX_ACCOUNT_LEN);
    debug_assert!(blob.len() <= MAX_BLOB_LEN);
    let mut out = Vec::with_capacity(4 + 2 + 4 + account.len() + 4 + blob.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(account.len() as u32).to_le_bytes());
    out.extend_from_slice(account.as_bytes());
    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    out.extend_from_slice(blob);
    out
}

/// 読む。形式に合わなければ理由を返す（どの理由にも中身は載せない）。
pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded<'_>, CorruptKind> {
    if bytes.len() > MAX_FILE_LEN {
        return Err(CorruptKind::FileTooLarge);
    }
    let mut rest = bytes;
    let magic = take(&mut rest, 4)?;
    if magic != MAGIC {
        return Err(CorruptKind::BadMagic);
    }
    let version = u16::from_le_bytes(take(&mut rest, 2)?.try_into().expect("2 bytes"));
    if version != VERSION {
        return Err(CorruptKind::UnsupportedVersion(version));
    }
    let account_len = read_u32(&mut rest)? as usize;
    if account_len > MAX_ACCOUNT_LEN {
        return Err(CorruptKind::AccountTooLong);
    }
    let account = std::str::from_utf8(take(&mut rest, account_len)?)
        .map_err(|_| CorruptKind::AccountNotUtf8)?;
    let blob_len = read_u32(&mut rest)? as usize;
    if blob_len > MAX_BLOB_LEN {
        return Err(CorruptKind::BlobTooLarge);
    }
    let blob = take(&mut rest, blob_len)?;
    if !rest.is_empty() {
        return Err(CorruptKind::TrailingBytes);
    }
    Ok(Decoded { account, blob })
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Result<&'a [u8], CorruptKind> {
    if rest.len() < n {
        return Err(CorruptKind::Truncated);
    }
    let (head, tail) = rest.split_at(n);
    *rest = tail;
    Ok(head)
}

fn read_u32(rest: &mut &[u8]) -> Result<u32, CorruptKind> {
    Ok(u32::from_le_bytes(
        take(rest, 4)?.try_into().expect("4 bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = encode("hub:127.0.0.1:8722:abc", b"\x01\x02\x03");
        assert_eq!(&bytes[..4], b"BSS1");
        assert_eq!(&bytes[4..6], &1u16.to_le_bytes());
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.account, "hub:127.0.0.1:8722:abc");
        assert_eq!(decoded.blob, b"\x01\x02\x03");
    }

    #[test]
    fn round_trips_an_empty_blob_and_a_maximal_account() {
        let account = "a".repeat(MAX_ACCOUNT_LEN);
        let bytes = encode(&account, b"");
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.account, account);
        assert!(decoded.blob.is_empty());
    }

    #[test]
    fn rejects_every_malformed_shape() {
        let good = encode("acct", b"blob");

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert_eq!(decode(&bad_magic), Err(CorruptKind::BadMagic));

        let mut bad_version = good.clone();
        bad_version[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(
            decode(&bad_version),
            Err(CorruptKind::UnsupportedVersion(2))
        );

        for cut in 0..good.len() {
            assert_eq!(
                decode(&good[..cut]),
                Err(CorruptKind::Truncated),
                "{cut} バイトで切ったファイル"
            );
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(CorruptKind::TrailingBytes));

        // アカウントの長さの欄だけ大きくする（本体は短いまま）。
        let mut long_account = good.clone();
        long_account[6..10].copy_from_slice(&((MAX_ACCOUNT_LEN as u32) + 1).to_le_bytes());
        assert_eq!(decode(&long_account), Err(CorruptKind::AccountTooLong));

        // blob の長さの欄だけ大きくする。
        let mut long_blob = good.clone();
        let blob_len_at = 10 + "acct".len();
        long_blob[blob_len_at..blob_len_at + 4]
            .copy_from_slice(&((MAX_BLOB_LEN as u32) + 1).to_le_bytes());
        assert_eq!(decode(&long_blob), Err(CorruptKind::BlobTooLarge));

        let mut not_utf8 = good.clone();
        not_utf8[10] = 0xFF;
        assert_eq!(decode(&not_utf8), Err(CorruptKind::AccountNotUtf8));

        let huge = vec![0u8; MAX_FILE_LEN + 1];
        assert_eq!(decode(&huge), Err(CorruptKind::FileTooLarge));

        assert_eq!(decode(b""), Err(CorruptKind::Truncated));
    }
}
