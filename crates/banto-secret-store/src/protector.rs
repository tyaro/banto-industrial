//! 平文と blob（暗号文）の変換。
//!
//! - [`DpapiMachine`]（Windows）: DPAPI の machine スコープ。製品で使う。
//! - [`PlainProtector`]（`test-support` feature かこの crate のテストだけ）:
//!   **暗号化しない**。entropy の一致だけは DPAPI と同じく確かめるので、
//!   取り違えのテストは全 OS で意味を持つ。

use zeroize::Zeroizing;

use crate::error::Result;
#[cfg(any(test, feature = "test-support"))]
use crate::error::SecretStoreError;

/// 平文を blob に、blob を平文に変える。
///
/// `entropy` は DPAPI の「追加のエントロピー」と同じ意味: 守るときと同じ値を
/// 渡さないと復号できない。[`crate::FileSecretStore`] はアカウントごとに違う
/// 値（[`crate::entropy_for`]）を渡す。
///
/// 実装は失敗に平文・blob を載せてはならない（[`SecretStoreError`](crate::SecretStoreError) の variant は
/// そもそも載せる場所を持たない）。
pub trait Protector: Send + Sync + 'static {
    fn protect(&self, plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>>;
    fn unprotect(&self, blob: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}

/// テスト専用: 暗号化しない [`Protector`]。blob は `b"PLN1"`・entropy の長さ
/// （u32 LE）・entropy・平文をつないだもの（**平文がそのまま入る**）。
///
/// `unprotect` は渡された entropy が守ったときと同じであることを確かめ、違えば
/// DPAPI と同じく [`SecretStoreError::Unprotect`](crate::SecretStoreError::Unprotect) にする。
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainProtector;

#[cfg(any(test, feature = "test-support"))]
const PLAIN_MAGIC: &[u8; 4] = b"PLN1";

#[cfg(any(test, feature = "test-support"))]
impl Protector for PlainProtector {
    fn protect(&self, plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        let mut blob = Vec::with_capacity(8 + entropy.len() + plain.len());
        blob.extend_from_slice(PLAIN_MAGIC);
        blob.extend_from_slice(&(entropy.len() as u32).to_le_bytes());
        blob.extend_from_slice(entropy);
        blob.extend_from_slice(plain);
        Ok(blob)
    }

    fn unprotect(&self, blob: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let fail = SecretStoreError::Unprotect { os_error: None };
        let rest = blob
            .strip_prefix(PLAIN_MAGIC.as_slice())
            .ok_or(fail.clone())?;
        if rest.len() < 4 {
            return Err(fail);
        }
        let (len, rest) = rest.split_at(4);
        let len = u32::from_le_bytes(len.try_into().expect("4 bytes")) as usize;
        if rest.len() < len || &rest[..len] != entropy {
            return Err(fail);
        }
        Ok(Zeroizing::new(rest[len..].to_vec()))
    }
}

/// DPAPI の machine スコープ（`CRYPTPROTECT_LOCAL_MACHINE |
/// CRYPTPROTECT_UI_FORBIDDEN`）。
///
/// **この PC の上でファイルを読めれば、どのアカウントのどのプロセスでも
/// 復号できる**（crate の doc「DPAPI machine スコープの意味と限界」）。守れるのは
/// 別の PC へのコピーとオフラインの盗難だけで、同じ PC の中の境界は保存先の
/// ディレクトリの ACL が持つ。
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DpapiMachine;

#[cfg(windows)]
impl Protector for DpapiMachine {
    fn protect(&self, plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        dpapi::protect(plain, entropy)
    }

    fn unprotect(&self, blob: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        dpapi::unprotect(blob, entropy)
    }
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE,
        CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    use zeroize::{Zeroize, Zeroizing};

    use crate::error::{Result, SecretStoreError};

    const FLAGS: u32 = CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN;

    fn input_blob(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB> {
        // 長さは呼び出し側（store）が上限で抑えているが、u32 に収まらない値を
        // 切り詰めて渡さないよう、ここでも確かめる。
        let len = u32::try_from(bytes.len()).map_err(|_| SecretStoreError::SecretTooLarge)?;
        Ok(CRYPT_INTEGER_BLOB {
            cbData: len,
            // DPAPI は入力を書き換えない（const の引数）。
            pbData: bytes.as_ptr() as *mut u8,
        })
    }

    /// DPAPI が `LocalAlloc` で返したバッファ。drop で（`wipe` なら消してから）
    /// `LocalFree` する。
    struct OutBlob {
        blob: CRYPT_INTEGER_BLOB,
        wipe: bool,
    }

    impl OutBlob {
        fn as_slice(&self) -> &[u8] {
            if self.blob.pbData.is_null() || self.blob.cbData == 0 {
                return &[];
            }
            // SAFETY: 成功した CryptProtectData / CryptUnprotectData が
            // `cbData` バイトを `pbData` に確保して返した。drop まで解放しない。
            unsafe { std::slice::from_raw_parts(self.blob.pbData, self.blob.cbData as usize) }
        }
    }

    impl Drop for OutBlob {
        fn drop(&mut self) {
            if self.blob.pbData.is_null() {
                return;
            }
            if self.wipe && self.blob.cbData > 0 {
                // SAFETY: as_slice と同じバッファ。解放の前に平文を消す。
                let buf = unsafe {
                    std::slice::from_raw_parts_mut(self.blob.pbData, self.blob.cbData as usize)
                };
                buf.zeroize();
            }
            // SAFETY: DPAPI の契約どおり、出力のバッファは LocalFree で解放する。
            unsafe {
                LocalFree(self.blob.pbData as _);
            }
        }
    }

    pub(super) fn protect(plain: &[u8], entropy: &[u8]) -> Result<Vec<u8>> {
        let input = input_blob(plain)?;
        let entropy = input_blob(entropy)?;
        let mut out = OutBlob {
            blob: CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            },
            wipe: false,
        };
        // SAFETY: 入力・entropy は呼び出しの間生きているスライスを指す。
        // 説明文・プロンプトは渡さない（UI_FORBIDDEN）。出力は OutBlob が解放する。
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                FLAGS,
                &mut out.blob,
            )
        };
        if ok == 0 {
            // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
            let os_error = unsafe { GetLastError() };
            return Err(SecretStoreError::Protect {
                os_error: Some(os_error),
            });
        }
        Ok(out.as_slice().to_vec())
    }

    pub(super) fn unprotect(blob: &[u8], entropy: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        let input = input_blob(blob)?;
        let entropy = input_blob(entropy)?;
        let mut out = OutBlob {
            blob: CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: std::ptr::null_mut(),
            },
            wipe: true,
        };
        // SAFETY: protect と同じ。説明文は受け取らない（null）。
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                FLAGS,
                &mut out.blob,
            )
        };
        if ok == 0 {
            // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
            let os_error = unsafe { GetLastError() };
            return Err(SecretStoreError::Unprotect {
                os_error: Some(os_error),
            });
        }
        Ok(Zeroizing::new(out.as_slice().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_protector_round_trips_and_checks_entropy() {
        let p = PlainProtector;
        let blob = p.protect(b"secret", b"e1").unwrap();
        assert_eq!(&*p.unprotect(&blob, b"e1").unwrap(), b"secret");
        assert_eq!(
            p.unprotect(&blob, b"e2"),
            Err(SecretStoreError::Unprotect { os_error: None })
        );
        assert!(p.unprotect(b"PLN1", b"e1").is_err());
        assert!(p.unprotect(b"xxxx", b"e1").is_err());
    }

    #[cfg(windows)]
    mod dpapi_machine {
        use super::super::*;

        const SECRET: &[u8] = b"bh_TOPSECRET_dpapi_value_7f3a";

        #[test]
        fn round_trips() {
            let p = DpapiMachine;
            let blob = p.protect(SECRET, b"banto-secret-store/v1:a").unwrap();
            assert!(
                !blob.windows(SECRET.len()).any(|w| w == SECRET),
                "blob に平文がそのまま入っている"
            );
            let plain = p.unprotect(&blob, b"banto-secret-store/v1:a").unwrap();
            assert_eq!(&*plain, SECRET);
        }

        #[test]
        fn another_entropy_cannot_decrypt() {
            let p = DpapiMachine;
            let blob = p.protect(SECRET, b"banto-secret-store/v1:a").unwrap();
            let err = p.unprotect(&blob, b"banto-secret-store/v1:b").unwrap_err();
            assert!(matches!(err, SecretStoreError::Unprotect { .. }), "{err:?}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains("TOPSECRET"), "{shown}");
        }

        #[test]
        fn a_tampered_blob_cannot_decrypt() {
            let p = DpapiMachine;
            let mut blob = p.protect(SECRET, b"e").unwrap();
            let last = blob.len() - 1;
            blob[last] ^= 0x55;
            assert!(matches!(
                p.unprotect(&blob, b"e"),
                Err(SecretStoreError::Unprotect { .. })
            ));
        }

        #[test]
        fn round_trips_an_empty_value() {
            let p = DpapiMachine;
            let blob = p.protect(b"", b"e").unwrap();
            assert!(p.unprotect(&blob, b"e").unwrap().is_empty());
        }
    }
}
