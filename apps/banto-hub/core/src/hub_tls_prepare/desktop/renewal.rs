//! One serialized disk/listener transaction. Identity lock precedes revocation
//! lock; every write uses the checked TLS directory handle. The listener callback
//! swaps in memory before publishing files and never releases an active socket.
use super::*;

const CERT_OLD: &str = "hub-cert.pem.old";
const KEY_OLD: &str = "hub-key.pem.old";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    BeforeSwitch,
    AfterSwitch,
    AfterBackup,
    AfterKeyPublish,
    AfterCertPublish,
    BeforeRollback,
    AfterCommitBeforeCleanup,
}

fn auxiliary_bytes(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    name: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, DesktopFailure> {
    checked_auxiliary_file(ops, dir, name)?
        .map(|mut file| {
            TlsFile::read_all(&mut file, MAX_PEM_BYTES)
                .map(Zeroizing::new)
                .map_err(|e| {
                    DesktopFailure::auxiliary(dir, name, format!("{name} を読めません: {e}"))
                })
        })
        .transpose()
}

fn replace(dir: &WinTlsDir, from: &str, to: &str) -> Result<(), DesktopFailure> {
    dir.managed()
        .remove(to)
        .map_err(|e| DesktopFailure::retry(format!("{to} を置き換えられません: {e}")))?;
    if !dir
        .managed()
        .rename_no_replace(from, to)
        .map_err(|e| DesktopFailure::retry(format!("{from} を {to} に付け替えられません: {e}")))?
    {
        return Err(DesktopFailure::retry(format!("{from} がありません")));
    }
    Ok(())
}

fn restore(dir: &WinTlsDir, cert: &[u8], key: &[u8]) -> Result<(), DesktopFailure> {
    cleanup(dir, &[KEY_TEMP, CERT_TEMP])?;
    create(dir, KEY_TEMP, key)?;
    create(dir, CERT_TEMP, cert)?;
    replace(dir, KEY_TEMP, KEY_FILE)?;
    replace(dir, CERT_TEMP, CERT_FILE)
}

/// Startup recovery always prefers the current cryptographically matching pair.
/// Old files are used only if current is incomplete/mismatched and old is valid,
/// unexpired, checked, and not revoked in the locked settled records.
pub(super) fn recover(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    revocations: &banto_hub_tls_trust::UserRevocations,
    now: i64,
    current: &PairBytes,
    explicit_recreation: bool,
) -> Result<bool, DesktopFailure> {
    let old_cert = checked_auxiliary_file(ops, dir, CERT_OLD)?;
    let old_key = checked_auxiliary_file(ops, dir, KEY_OLD)?;
    if old_cert.is_none() && old_key.is_none() {
        return Ok(false);
    }
    // Validate temp-file security before any removal or replacement.
    for name in [CERT_TEMP, KEY_TEMP] {
        drop(checked_auxiliary_file(ops, dir, name)?);
    }
    if matches!((&current.0, &current.1), (Some(cert), Some(key)) if pair_matches(cert, key)) {
        return Ok(true);
    }
    let read = |file: Option<PinnedFile>| {
        file.map(|mut file| TlsFile::read_all(&mut file, MAX_PEM_BYTES).map(Zeroizing::new))
            .transpose()
    };
    let (old_cert, old_key) = match (read(old_cert), read(old_key)) {
        (Ok(cert), Ok(key)) => (cert, key),
        _ if explicit_recreation => return Ok(true),
        _ => {
            return Err(DesktopFailure::recreate(
                "旧い組を読めないため復旧できません",
            ))
        }
    };
    let (Some(cert), Some(key)) = (old_cert, old_key) else {
        if explicit_recreation {
            return Ok(true);
        }
        return Err(DesktopFailure::recreate(
            "作り直しの旧い組が揃っていないため復旧できません",
        ));
    };
    if let Err(reason) = inspect_identity(&cert, &key, now) {
        if explicit_recreation {
            return Ok(true);
        }
        return Err(DesktopFailure::recreate(reason));
    }
    let thumbprint = CertThumbprint::of_pem(&cert).map_err(DesktopFailure::recreate)?;
    if !revocations.is_settled() || revocations.is_revoked(&thumbprint) {
        if explicit_recreation && revocations.is_settled() {
            return Ok(true);
        }
        return Err(DesktopFailure::recreate(
            "旧い証明書は失効しているか記録を確かめられないため戻しません",
        ));
    }
    restore(dir.dir(), &cert, &key)?;
    Ok(true)
}

pub(super) fn cleanup_old(dir: &WinTlsDir) -> Result<(), DesktopFailure> {
    cleanup(dir, &[KEY_OLD, CERT_OLD])
}

/// `switch(Some(pair))` installs the pair, `switch(None)` must stop TLS and
/// connections. Callback failure never authorizes publication of the new pair.
pub(super) fn recreate<P: UserTlsPlace + ?Sized>(
    place: &P,
    pending: &mut std::collections::BTreeSet<CertThumbprint>,
    known_serving: Option<CertThumbprint>,
    now: i64,
    mut switch: impl FnMut(Option<(&[u8], &[u8])>) -> Result<(), String>,
    mut observe: impl FnMut(Stage) -> Result<(), String>,
) -> Result<Option<String>, DesktopFailure> {
    // Creation is stricter than a periodic check: any unavailable validation,
    // including transient lock/record failure, closes the old listener too.
    // A subsequent *acceptor swap* failure is handled separately below and may
    // preserve the old validated listener, as required by the rollback contract.
    macro_rules! validated {
        ($operation:expr) => {{
            match $operation {
                Ok(value) => value,
                Err(error) => {
                    let _ = switch(None);
                    return Err(error);
                }
            }
        }};
    }
    let ops = WindowsTlsTrustOps;
    let options = ReconcileOptions::default();
    if known_serving.is_some_and(|serving| pending.contains(&serving)) {
        let _ = switch(None);
    }
    validated!(check_place_layout(place).map_err(DesktopFailure::retry));
    let dir = validated!(tls_place_observed(&ops, place, &options, |found| {
        if found.fault.revokes() {
            pending.extend(known_serving);
            if let UncheckedUserCert::Seen(cert) = peek_user_cert(&ops, place) {
                pending.insert(cert.thumbprint);
            }
            let _ = switch(None);
        }
    }));
    let _identity_lock = validated!(lock(&ops, &dir, &options));
    let mut records = validated!(
        lock_user_revocations(&ops, place, &options).map_err(|r| failure_from_report(*r))
    );
    validated!(settle_with(|| records.reconcile()));
    let current_records = records.read();
    if !current_records.is_settled() {
        validated!(Err::<(), _>(DesktopFailure::retry(
            "利用者の失効の記録を確かめられません"
        )));
    }
    if known_serving.is_some_and(|serving| current_records.is_revoked(&serving)) {
        let _ = switch(None);
    }
    for thumbprint in pending.clone() {
        let written = records.record(thumbprint, RevokedCertPlace::User);
        if written.outcome != ReconcileOutcome::Consistent
            || !written.revocations.is_revoked(&thumbprint)
        {
            let _ = switch(None);
            return Err(failure_from_report(written));
        }
        pending.remove(&thumbprint);
    }
    let mut exposure_seen = false;
    let mut old = validated!(read_pair(&ops, place, &dir, &mut |thumbprint| {
        exposure_seen = true;
        pending.insert(thumbprint);
        pending.extend(known_serving);
        let _ = switch(None);
        let report = records.record(thumbprint, RevokedCertPlace::User);
        // Keep the serving thumbprint pending too if it differs from the disk.
        if report.outcome == ReconcileOutcome::Consistent
            && report.revocations.is_revoked(&thumbprint)
        {
            pending.remove(&thumbprint);
        }
        report
    }));
    if validated!(recover(&ops, &dir, &records.read(), now, &old, true)) {
        old = validated!(read_pair(&ops, place, &dir, &mut |thumbprint| {
            exposure_seen = true;
            pending.insert(thumbprint);
            pending.extend(known_serving);
            let _ = switch(None);
            let report = records.record(thumbprint, RevokedCertPlace::User);
            if report.outcome == ReconcileOutcome::Consistent
                && report.revocations.is_revoked(&thumbprint)
            {
                pending.remove(&thumbprint);
            }
            report
        }));
    }
    for name in [KEY_OLD, CERT_OLD, KEY_TEMP, CERT_TEMP, CREATING] {
        drop(validated!(checked_auxiliary_file(&ops, &dir, name)));
    }
    let old_matches =
        matches!((&old.0, &old.1), (Some(cert), Some(key)) if pair_matches(cert, key));
    if !old_matches {
        if let Some(thumbprint) = old
            .0
            .as_ref()
            .and_then(|cert| CertThumbprint::of_pem(cert).ok())
        {
            pending.insert(thumbprint);
            let report = records.record(thumbprint, RevokedCertPlace::User);
            if report.outcome != ReconcileOutcome::Consistent
                || !report.revocations.is_revoked(&thumbprint)
            {
                let _ = switch(None);
                return Err(failure_from_report(report));
            }
            pending.remove(&thumbprint);
        }
    }
    validated!(cleanup_old(dir.dir()));
    validated!(cleanup_auxiliary_files(&dir));
    validated!(create(dir.dir(), CREATING, CREATING_CONTENTS));
    let identity = validated!(generate_identity(now).map_err(DesktopFailure::retry));
    validated!(create(dir.dir(), KEY_TEMP, identity.key_pem.as_bytes()));
    validated!(create(dir.dir(), CERT_TEMP, identity.cert_pem.as_bytes()));
    let cert = validated!(auxiliary_bytes(&ops, &dir, CERT_TEMP)
        .and_then(|cert| cert.ok_or_else(|| DesktopFailure::retry("新しい証明書がありません"))));
    let key = validated!(auxiliary_bytes(&ops, &dir, KEY_TEMP)
        .and_then(|key| key.ok_or_else(|| DesktopFailure::retry("新しい鍵がありません"))));
    validated!(inspect_identity(&cert, &key, now).map_err(DesktopFailure::retry));
    let current_records = records.read();
    if !current_records.is_settled() {
        validated!(Err::<(), _>(DesktopFailure::retry(
            "新しい証明書を使う前に失効の記録を確かめられません"
        )));
    }
    if known_serving.is_some_and(|serving| current_records.is_revoked(&serving)) {
        let _ = switch(None);
    }
    let allowed_old = match (&old.0, &old.1) {
        (Some(cert), Some(key)) => {
            inspect_identity(cert, key, now).is_ok()
                && CertThumbprint::of_pem(cert)
                    .is_ok_and(|thumb| !current_records.is_revoked(&thumb))
        }
        _ => false,
    };
    validated!(observe(Stage::BeforeSwitch).map_err(DesktopFailure::retry));
    switch(Some((&cert, &key))).map_err(DesktopFailure::retry)?;
    let mut published = false;
    let result = (|| {
        observe(Stage::AfterSwitch).map_err(DesktopFailure::retry)?;
        if old_matches {
            let (Some(cert), Some(key)) = (&old.0, &old.1) else {
                unreachable!()
            };
            // create_child_file_current_user writes and FlushFileBuffers before
            // returning. Originals remain untouched until both copies verify.
            create(dir.dir(), KEY_OLD, key)?;
            create(dir.dir(), CERT_OLD, cert)?;
            let backup_cert = auxiliary_bytes(&ops, &dir, CERT_OLD)?.unwrap();
            let backup_key = auxiliary_bytes(&ops, &dir, KEY_OLD)?.unwrap();
            if backup_cert.as_slice() != cert
                || backup_key.as_slice() != key.as_slice()
                || !pair_matches(&backup_cert, &backup_key)
            {
                return Err(DesktopFailure::retry("旧い組の写しを確認できません"));
            }
        }
        observe(Stage::AfterBackup).map_err(DesktopFailure::retry)?;
        published = true;
        replace(dir.dir(), KEY_TEMP, KEY_FILE)?;
        observe(Stage::AfterKeyPublish).map_err(DesktopFailure::retry)?;
        replace(dir.dir(), CERT_TEMP, CERT_FILE)?;
        observe(Stage::AfterCertPublish).map_err(DesktopFailure::retry)?;
        let current = read_pair(&ops, place, &dir, &mut |thumbprint| {
            exposure_seen = true;
            pending.insert(thumbprint);
            pending.extend(known_serving);
            let _ = switch(None);
            let report = records.record(thumbprint, RevokedCertPlace::User);
            if report.outcome == ReconcileOutcome::Consistent
                && report.revocations.is_revoked(&thumbprint)
            {
                pending.remove(&thumbprint);
            }
            report
        })?;
        if !matches!(current, (Some(ref c), Some(ref k)) if c.as_slice() == cert.as_slice() && k.as_slice() == key.as_slice())
        {
            return Err(DesktopFailure::recreate("公開した新しい組が一致しません"));
        }
        Ok(())
    })();
    if let Err(error) = result {
        let restored = if allowed_old && !exposure_seen && observe(Stage::BeforeRollback).is_ok() {
            let (Some(old_cert), Some(old_key)) = (&old.0, &old.1) else {
                unreachable!()
            };
            (!published || restore(dir.dir(), old_cert, old_key).is_ok())
                && switch(Some((old_cert, old_key))).is_ok()
        } else {
            false
        };
        if !restored {
            let _ = switch(None);
        }
        return Err(DesktopFailure::recreate(format!(
            "作り直しに失敗しました: {}。{}",
            error.reason,
            if restored {
                "旧い組と待ち受けへ戻しました"
            } else {
                "安全な旧い組へ戻せないため TLS を停止しました。Hub を閉じて置き場とログを確認してください。失効の記録は消さないでください"
            }
        )));
    }
    // Publication is committed. Auxiliary cleanup cannot turn it into an
    // uncommitted result or make the actor keep monitoring the old thumbprint.
    if let Err(error) = observe(Stage::AfterCommitBeforeCleanup)
        .map_err(DesktopFailure::retry)
        .and_then(|_| cleanup_old(dir.dir()))
        .and_then(|_| cleanup_auxiliary_files(&dir))
    {
        return Ok(Some(format!(
            "証明書の作り直しは完了しましたが補助ファイルを片付けられません: {}",
            error.reason
        )));
    }
    Ok(None)
}
