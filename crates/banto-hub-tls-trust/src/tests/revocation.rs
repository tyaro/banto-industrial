//! 利用者の失効の記録（`crate::revocation`、#1121 PR1b）を偽物の [`RevocationOps`] で確かめる
//! （全 OS）。docs/hub-desktop-tls-design.md §4.4.2 の場面のうち、この crate の範囲で表せるもの
//! （1〜5、7 の記録の部分、9、12〜19、22〜24、27）を、試験の名前の `scene_N` と doc の番号で
//! 対応させる。最後に、実 Windows で書き込みの部品と手順を確かめる。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use banto_safe_root::acl::MODIFY_ACCESS_MASK;
use banto_safe_root::guard::DriveKind;

use super::user::{local, product, rev_a, rev_b, user_dir, user_file, user_tls, ME};
use super::{ace, cert_pem, DirEntry, FakeDir, FakeEnv, FakeFile, FileEntry, EVIL};
use crate::*;

const LOCK_WAIT: Duration = Duration::from_millis(200);
const LOCK_POLL: Duration = Duration::from_millis(50);
const NOW: u64 = 1_800_000_000;

fn lock_path() -> PathBuf {
    local().join(USER_REVOCATIONS_LOCK_FILE)
}

fn side_dir(side: RevocationSide) -> PathBuf {
    match side {
        RevocationSide::A => rev_a(),
        RevocationSide::B => rev_b(),
    }
}

fn record_path(side: RevocationSide) -> PathBuf {
    side_dir(side).join(REVOCATIONS_FILE)
}

/// 試験用の拇印（`n` ごとに違う）。
fn thumb(n: u8) -> CertThumbprint {
    CertThumbprint::of_der(&[n])
}

fn revoked(n: u8) -> RevokedCert {
    RevokedCert {
        thumbprint: thumb(n),
        place: RevokedCertPlace::User,
        recorded_at: 1_700_000_000 + u64::from(n),
    }
}

fn encoded(state: RecordState, thumbs: &[u8]) -> Vec<u8> {
    RevocationRecord::new(state, thumbs.iter().map(|n| revoked(*n))).encode()
}

use RecordState::{Committed, Pending};
use RevocationSide::{A, B};

/// 書き込みもできる偽物（フォルダとファイルを覚え、操作の順を記録する）。
struct FakeRevOps {
    dirs: RefCell<HashMap<PathBuf, DirEntry>>,
    files: RefCell<HashMap<PathBuf, FileEntry>>,
    /// 開くと I/O の誤りになるファイル（一時的な失敗）。
    open_error: RefCell<HashMap<PathBuf, FileOpenError>>,
    /// 排他の open が「握られている」を返す残りの回数（`u32::MAX` なら握られたまま）。
    lock_busy: Cell<u32>,
    /// あと何回書けるか（`None` は制限なし。使い切った後の書き込みは失敗する = そこで止まった）。
    writes_left: Cell<Option<u32>>,
    /// 新しく作るフォルダの形（親から継承した ACL の代わり）。
    new_dir: RefCell<DirEntry>,
    slept: RefCell<Vec<Duration>>,
    log: RefCell<Vec<String>>,
}

impl FakeRevOps {
    /// `%LOCALAPPDATA%` だけがある（Hub も ChronoGazer も動かしたことが無い利用者）。
    fn fresh() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(local(), user_dir());
        Self {
            dirs: RefCell::new(dirs),
            files: RefCell::new(HashMap::new()),
            open_error: RefCell::new(HashMap::new()),
            lock_busy: Cell::new(0),
            writes_left: Cell::new(None),
            new_dir: RefCell::new(user_dir()),
            slept: RefCell::new(Vec::new()),
            log: RefCell::new(Vec::new()),
        }
    }

    /// A・B が `確定` で、同じ拇印の並びを持つ（ロックファイルもある）。
    fn settled(thumbs: &[u8]) -> Self {
        let ops = Self::fresh();
        ops.put_record(A, Committed, thumbs);
        ops.put_record(B, Committed, thumbs);
        ops.files.borrow_mut().insert(lock_path(), user_file(b""));
        ops
    }

    fn place(&self) -> DefaultUserTlsPlace {
        DefaultUserTlsPlace::new(local(), ME.to_string())
    }

    fn add_dirs(&self, path: &Path) {
        let mut dirs = self.dirs.borrow_mut();
        let mut current = path.to_path_buf();
        while current != local() {
            dirs.entry(current.clone()).or_insert_with(user_dir);
            current.pop();
        }
    }

    fn put_file(&self, side: RevocationSide, contents: &[u8]) {
        self.add_dirs(&side_dir(side));
        self.files
            .borrow_mut()
            .insert(record_path(side), user_file(contents));
    }

    fn put_record(&self, side: RevocationSide, state: RecordState, thumbs: &[u8]) {
        self.put_file(side, &encoded(state, thumbs));
    }

    /// 利用者の `tls\` に証明書と鍵を置き、その拇印を返す。
    fn put_cert(&self) -> CertThumbprint {
        let pem = cert_pem();
        self.add_dirs(&user_tls());
        let mut files = self.files.borrow_mut();
        files.insert(user_tls().join(CERT_FILE), user_file(pem.as_bytes()));
        files.insert(user_tls().join(KEY_FILE), user_file(b"key"));
        CertThumbprint::of_pem(pem.as_bytes()).unwrap()
    }

    /// 別のアカウントに書き込みを許す（フォルダ）。
    fn loosen_dir(&self, path: &Path) {
        self.dirs.borrow_mut().get_mut(path).unwrap().aces.push(ace(
            EVIL,
            MODIFY_ACCESS_MASK,
            0x03,
        ));
    }

    fn tighten_dir(&self, path: &Path) {
        self.dirs
            .borrow_mut()
            .get_mut(path)
            .unwrap()
            .aces
            .retain(|ace| ace.sid != EVIL);
    }

    /// 別のアカウントに書き込みを許す（ファイル）。
    fn loosen_file(&self, path: &Path) {
        if let Some(aces) = self
            .files
            .borrow_mut()
            .get_mut(path)
            .unwrap()
            .security
            .dacl
            .aces
            .as_mut()
        {
            aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0x10));
        }
    }

    fn remove_tree(&self, path: &Path) {
        self.dirs
            .borrow_mut()
            .retain(|dir, _| !dir.starts_with(path));
        self.files
            .borrow_mut()
            .retain(|file, _| !file.starts_with(path));
    }

    /// 置いてある記録（無ければ `None`。形が壊れていれば落とす）。
    fn record(&self, side: RevocationSide) -> Option<RevocationRecord> {
        self.files
            .borrow()
            .get(&record_path(side))
            .map(|file| RevocationRecord::parse(&file.contents).expect("記録の形"))
    }

    /// 置いてある記録の状態と拇印（昇順）。
    fn stored(&self, side: RevocationSide) -> Option<(RecordState, Vec<CertThumbprint>)> {
        self.record(side).map(|record| {
            (
                record.state(),
                record.entries().iter().map(|e| e.thumbprint).collect(),
            )
        })
    }

    fn assert_committed(&self, expected: &[CertThumbprint]) {
        let mut expected = expected.to_vec();
        expected.sort();
        for side in RevocationSide::BOTH {
            assert_eq!(
                self.stored(side),
                Some((Committed, expected.clone())),
                "{side:?}"
            );
        }
    }

    fn options() -> ReconcileOptions {
        ReconcileOptions {
            lock_wait: LOCK_WAIT,
            lock_poll: LOCK_POLL,
        }
    }

    fn reconcile(&self) -> ReconcileReport {
        self.log.borrow_mut().clear();
        reconcile_user_revocations(self, &self.place(), &Self::options())
    }

    fn revoke(&self, thumbprint: CertThumbprint, place: RevokedCertPlace) -> ReconcileReport {
        self.log.borrow_mut().clear();
        record_user_revocation(self, &self.place(), &Self::options(), thumbprint, place)
    }

    /// 書き込み・作成・ロックの順（読み取りは記録しない）。
    fn steps(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    /// 記録の書き込みだけ。
    fn writes(&self) -> Vec<String> {
        self.steps()
            .into_iter()
            .filter(|step| step.starts_with("write"))
            .collect()
    }

    fn note(&self, step: String) {
        self.log.borrow_mut().push(step);
    }
}

impl TlsTrustOps for FakeRevOps {
    type Env = FakeEnv;
    type Dir = FakeDir;
    type File = FakeFile;

    fn machine_env(&self) -> &FakeEnv {
        &FakeEnv
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, String> {
        Ok(self.dirs.borrow().get(dir).map(|entry| FakeDir {
            path: dir.to_path_buf(),
            entry: entry.clone(),
        }))
    }
    fn open_child_dir(&self, parent: &FakeDir, name: &str) -> Result<Option<FakeDir>, String> {
        let path = parent.path.join(name);
        Ok(self.dirs.borrow().get(&path).map(|entry| FakeDir {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
    fn open_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeFile>, FileOpenError> {
        let path = parent.path.join(name);
        if let Some(error) = self.open_error.borrow().get(&path) {
            return Err(error.clone());
        }
        assert!(!name.contains(".tmp-"), "一時ファイルは読まない（{name}）");
        Ok(self.files.borrow().get(&path).map(|entry| FakeFile {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
}

impl UserTlsOps for FakeRevOps {
    fn local_app_data(&self) -> Result<PathBuf, String> {
        Ok(local())
    }
    fn current_user_sid(&self) -> Result<String, String> {
        Ok(ME.to_string())
    }
    fn drive_kind(&self, _real: &Path) -> Result<DriveKind, String> {
        Ok(DriveKind::Fixed)
    }
}

impl RevocationOps for FakeRevOps {
    fn create_child_dir(&self, parent: &FakeDir, name: &str) -> Result<(), String> {
        self.note(format!("mkdir {name}"));
        let template = self.new_dir.borrow().clone();
        self.dirs
            .borrow_mut()
            .entry(parent.path.join(name))
            .or_insert(template);
        Ok(())
    }

    fn write_atomic(&self, dir: &FakeDir, name: &str, contents: &[u8]) -> Result<(), String> {
        assert_eq!(name, REVOCATIONS_FILE);
        let side = if dir.path == rev_a() { "A" } else { "B" };
        assert!(self.dirs.borrow().contains_key(&dir.path), "無いフォルダ");
        match self.writes_left.get() {
            Some(0) => {
                self.note(format!("write {side} 失敗"));
                return Err("ディスクが一杯です".to_string());
            }
            Some(left) => self.writes_left.set(Some(left - 1)),
            None => {}
        }
        let record = RevocationRecord::parse(contents).expect("書く中身は正しい形");
        let state = match record.state() {
            Pending => "準備中",
            Committed => "確定",
        };
        self.note(format!("write {side} {state} {}", record.entries().len()));
        self.files
            .borrow_mut()
            .insert(dir.path.join(name), user_file(contents));
        Ok(())
    }

    fn remove_stale_temps(&self, dir: &FakeDir, name: &str) -> Result<(), String> {
        let prefix = format!(".{name}.tmp-");
        let mut files = self.files.borrow_mut();
        let before = files.len();
        files.retain(|path, _| {
            !(path.parent() == Some(dir.path.as_path())
                && path
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy().starts_with(&prefix)))
        });
        if files.len() != before {
            self.log
                .borrow_mut()
                .push(format!("tidy {}", before - files.len()));
        }
        Ok(())
    }

    fn create_file_if_absent(&self, dir: &FakeDir, name: &str) -> Result<(), String> {
        self.note(format!("touch {name}"));
        self.files
            .borrow_mut()
            .entry(dir.path.join(name))
            .or_insert_with(|| user_file(b""));
        Ok(())
    }

    fn open_child_file_exclusive(
        &self,
        dir: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeFile>, FileOpenError> {
        let path = dir.path.join(name);
        let Some(entry) = self.files.borrow().get(&path).cloned() else {
            return Ok(None);
        };
        match self.lock_busy.get() {
            0 => {}
            u32::MAX => return Err(FileOpenError::InUse),
            left => {
                self.lock_busy.set(left - 1);
                return Err(FileOpenError::InUse);
            }
        }
        self.note("lock".to_string());
        Ok(Some(FakeFile { path, entry }))
    }

    fn sleep(&self, duration: Duration) {
        self.slept.borrow_mut().push(duration);
    }

    fn now_unix_seconds(&self) -> u64 {
        NOW
    }
}

fn lost_sides(report: &ReconcileReport) -> Vec<RevocationSide> {
    report
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            RevocationWarning::Lost { side, .. } => Some(*side),
            _ => None,
        })
        .collect()
}

fn has_io_warning(report: &ReconcileReport) -> bool {
    report
        .warnings
        .iter()
        .any(|warning| matches!(warning, RevocationWarning::Io(_)))
}

// --- 記録の形 -------------------------------------------------------------------------

#[test]
fn record_round_trips_and_is_sorted_and_deduplicated() {
    let record = RevocationRecord::new(Committed, [revoked(3), revoked(1), revoked(3), revoked(2)]);
    let thumbs: Vec<_> = record.entries().iter().map(|e| e.thumbprint).collect();
    let mut sorted = vec![thumb(1), thumb(2), thumb(3)];
    sorted.sort();
    assert_eq!(thumbs, sorted);
    assert!(record.contains(&thumb(2)));
    assert!(!record.contains(&thumb(9)));

    let bytes = record.encode();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.starts_with("banto-hub-revocations 1\nstate committed\nrevoked "));
    assert!(text.ends_with("\nend 3\n"), "{text}");
    assert_eq!(RevocationRecord::parse(&bytes).unwrap(), record);

    let empty = RevocationRecord::new(Pending, []);
    assert_eq!(
        String::from_utf8(empty.encode()).unwrap(),
        "banto-hub-revocations 1\nstate pending\nend 0\n"
    );
    assert_eq!(RevocationRecord::parse(&empty.encode()).unwrap(), empty);

    // 置き場の種類と時刻も往復する。
    let service = RevocationRecord::new(
        Committed,
        [RevokedCert {
            thumbprint: thumb(7),
            place: RevokedCertPlace::Service,
            recorded_at: 0,
        }],
    );
    assert_eq!(RevocationRecord::parse(&service.encode()).unwrap(), service);
}

#[test]
fn record_parse_refuses_anything_off_the_form() {
    let good = String::from_utf8(encoded(Committed, &[1, 2])).unwrap();
    let lines: Vec<&str> = good.trim_end().split('\n').collect();
    let hex = thumb(1).to_hex();
    let refuse = |text: String, needle: &str| {
        let error = RevocationRecord::parse(text.as_bytes()).expect_err(needle);
        assert!(error.contains(needle), "{needle} が無い: {error}");
    };
    // 途中で切れた（end の行が無い）。
    refuse(
        format!("{}\n", lines[..3].join("\n")),
        "end の行がありません",
    );
    refuse(lines[..2].join("\n") + "\n", "end の行がありません");
    // 最後の改行が無い・CRLF・空・別の形。
    refuse(good.trim_end().to_string(), "改行で終わっていません");
    refuse(good.replace('\n', "\r\n"), "1 行目");
    refuse(String::new(), "改行で終わっていません");
    refuse("{}\n".to_string(), "1 行目");
    refuse(good.replace("revocations 1", "revocations 2"), "1 行目");
    // 状態。
    refuse(good.replace("state committed", "state done"), "状態");
    refuse(good.replace("state committed\n", ""), "状態");
    // 件数・続き・読めない行。
    refuse(good.replace("end 2", "end 3"), "件数");
    refuse(good.replace("end 2", "end 02"), "件数");
    refuse(format!("{good}revoked {hex} user 1\n"), "後に続き");
    refuse(good.replace("end 2\n", "end 2\n\n"), "後に続き");
    refuse(good.replace("revoked ", "revoke "), "読めない行");
    refuse(good.replace(" user ", "  user "), "読めない行");
    // 拇印・置き場・時刻。
    refuse(good.replace(&hex, &hex.to_uppercase()), "拇印");
    refuse(good.replace(&hex, &hex[..62]), "拇印");
    refuse(good.replace(" user ", " hub "), "置き場の種類");
    refuse(good.replace(" 17000", " -17000"), "時刻");
    refuse(good.replace(" 17000", " +17000"), "時刻");
    // 並び（昇順でない・同じ拇印が 2 行）。
    let swapped = format!(
        "{}\n{}\n{}\n{}\n{}\n",
        lines[0], lines[1], lines[3], lines[2], lines[4]
    );
    refuse(swapped, "昇順");
    let twice = format!(
        "{}\n{}\n{}\n{}\nend 2\n",
        lines[0], lines[1], lines[2], lines[2]
    );
    refuse(twice, "昇順");
    // ASCII 以外・大きすぎる。
    refuse(good.replace("committed", "確定"), "ASCII");
    let huge = vec![b'x'; MAX_REVOCATIONS_BYTES + 1];
    assert!(RevocationRecord::parse(&huge)
        .unwrap_err()
        .contains("大きすぎ"));
}

#[test]
fn thumbprint_is_the_sha256_of_the_der_in_lowercase_hex() {
    // SHA-256("abc")。
    let found = CertThumbprint::of_der(b"abc");
    assert_eq!(
        found.to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(CertThumbprint::from_hex(&found.to_hex()), Some(found));
    assert_eq!(CertThumbprint::from_hex(&found.to_hex()[1..]), None);
    assert_eq!(
        CertThumbprint::from_hex(&found.to_hex().replace('b', "g")),
        None
    );
    // PEM からは、証明書の DER の拇印。
    let pem = cert_pem();
    let der = parse_single_certificate(pem.as_bytes()).unwrap();
    assert_eq!(
        CertThumbprint::of_pem(pem.as_bytes()).unwrap(),
        CertThumbprint::of_der(&der)
    );
    assert!(CertThumbprint::of_pem(b"not a pem").is_err());
}

// --- 整える手順: 初期化 -----------------------------------------------------------------

/// 場面 1・3: 初回（Hub でも ChronoGazer でも、先に動いたほう）。A・B を空で作って `確定` に
/// する。フォルダとロックファイルも作る。利用者の証明書は無いので、空の記録になる。
#[test]
fn scene_1_3_first_run_creates_both_records_empty_and_committed() {
    let ops = FakeRevOps::fresh();
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty(), "{report:#?}");
    assert_eq!(
        ops.steps(),
        vec![
            "touch BantoHubRevocations.lock",
            "lock",
            "mkdir BantoHub",
            "mkdir revocations",
            "write A 準備中 0",
            "mkdir BantoHubRevocations",
            "write B 確定 0",
            "write A 確定 0",
        ]
    );
    ops.assert_committed(&[]);
    assert!(report.revocations.is_settled());
    assert_eq!(report.revocations.entries().count(), 0);
    assert!(ops.slept.borrow().is_empty());
}

/// 場面 2・5: 後から動いたプロセス（ChronoGazer を後から入れた・記録は `確定` で証明書はまだ
/// 無い）。「整っている」ので何も書かない。
#[test]
fn scene_2_5_a_settled_pair_is_left_untouched() {
    let ops = FakeRevOps::settled(&[1, 2]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(report.warnings.is_empty(), "{report:#?}");
    assert_eq!(ops.steps(), vec!["lock"]);
    assert!(report.revocations.is_revoked(&thumb(1)));
    assert!(report.revocations.is_revoked(&thumb(2)));
    assert!(!report.revocations.is_revoked(&thumb(3)));

    // 証明書があっても、整っている記録には足さない（失効に入れるのは、記録を新しく作るとき
    // だけ）。
    let cert = ops.put_cert();
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(ops.writes().is_empty());
    assert!(!report.revocations.is_revoked(&cert));
}

/// 場面 4: 初期化の途中で止まった（証明書はまだ無い）。「`準備中`・無い」も「`準備中`・`確定`」
/// も、次に動いたプロセスが続きから仕上げる。消失にはしない。A にある拇印は引き継ぐ。
#[test]
fn scene_4_an_interrupted_initialisation_is_finished_not_treated_as_loss() {
    // A を準備中で作ったところで止まった。
    let ops = FakeRevOps::fresh();
    ops.writes_left.set(Some(1));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert!(has_io_warning(&report));
    assert_eq!(ops.stored(A), Some((Pending, vec![])));
    assert_eq!(ops.stored(B), None);
    ops.writes_left.set(None);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty(), "{report:#?}");
    assert_eq!(ops.writes(), vec!["write B 確定 0", "write A 確定 0"]);
    ops.assert_committed(&[]);

    // B まで作ったところで止まった。
    let ops = FakeRevOps::fresh();
    ops.writes_left.set(Some(2));
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Incomplete);
    assert_eq!(ops.stored(A), Some((Pending, vec![])));
    assert_eq!(ops.stored(B), Some((Committed, vec![])));
    ops.writes_left.set(None);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(report.warnings.is_empty(), "{report:#?}");
    assert_eq!(ops.writes(), vec!["write A 確定 0"]);

    // 準備中の A にある拇印は引き継ぐ（B が無い・B が確定で別の拇印を持つ）。
    let ops = FakeRevOps::fresh();
    ops.put_record(A, Pending, &[1]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(lost_sides(&report).is_empty());
    ops.assert_committed(&[thumb(1)]);

    let ops = FakeRevOps::fresh();
    ops.put_record(A, Pending, &[1]);
    ops.put_record(B, Committed, &[2]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert_eq!(ops.writes(), vec!["write B 確定 2", "write A 確定 2"]);
    ops.assert_committed(&[thumb(1), thumb(2)]);
}

/// 場面 4 の「続き」で記録を新しく作るとき、利用者の証明書がすでにあれば失効に入れる（正しい
/// 順序では、記録が 2 つとも `確定` になる前に証明書は無い）。
#[test]
fn scene_4_continuing_with_an_existing_certificate_revokes_it() {
    let ops = FakeRevOps::fresh();
    ops.put_record(A, Pending, &[1]);
    let cert = ops.put_cert();
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert_eq!(
        report.warnings,
        vec![RevocationWarning::ExistingCertRevoked(cert)]
    );
    ops.assert_committed(&[thumb(1), cert]);
    assert!(report.revocations.is_revoked(&cert));
}

// --- 失効を書き足す ---------------------------------------------------------------------

/// 場面 7（記録の部分）・6: 利用者の証明書の露出を見た側が、A・B に拇印を書き足す。もう片方の
/// プロセスは、ロック無しの読み口で次の周期に見つける。
#[test]
fn scene_7_recording_an_exposed_user_certificate_reaches_both_records() {
    let ops = FakeRevOps::settled(&[1]);
    let cert = ops.put_cert();
    assert!(!read_user_revocations(&ops, &ops.place()).is_revoked(&cert));

    let report = ops.revoke(cert, RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty(), "{report:#?}");
    assert_eq!(
        ops.steps(),
        vec!["lock", "write A 確定 2", "write B 確定 2"]
    );
    ops.assert_committed(&[thumb(1), cert]);
    let entry = *ops
        .record(B)
        .unwrap()
        .entries()
        .iter()
        .find(|entry| entry.thumbprint == cert)
        .unwrap();
    assert_eq!(entry.place, RevokedCertPlace::User);
    assert_eq!(entry.recorded_at, NOW);

    // もう片方のプロセスの照合（ロック無し・書かない）。
    ops.log.borrow_mut().clear();
    let seen = read_user_revocations(&ops, &ops.place());
    assert!(seen.is_revoked(&cert));
    assert!(seen.is_settled());
    assert!(ops.steps().is_empty());

    // 同じ拇印をもう一度書き足しても、何も書かない。
    let report = ops.revoke(cert, RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(ops.writes().is_empty());
}

/// 場面 9: ChronoGazer デスクトップがサービスの証明書の露出を先に見た。利用者の記録 A・B に、
/// サービスの証明書として書く。記録がまだ無くても、作ってから書く（場面 3 を兼ねる）。
#[test]
fn scene_9_an_exposed_service_certificate_is_recorded_in_the_user_records() {
    let ops = FakeRevOps::fresh();
    let service = thumb(42);
    let report = ops.revoke(service, RevokedCertPlace::Service);
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    ops.assert_committed(&[service]);
    assert_eq!(
        ops.record(A).unwrap().entries()[0].place,
        RevokedCertPlace::Service
    );
    assert!(report.revocations.is_revoked(&service));
}

/// 場面 6・7 の入口: 露出（別のアカウントが鍵を読める・フォルダを書き換えられる）を見た
/// 読み手は、形だけ読めた証明書の拇印を返す。所有者の違い・壊れは露出ではないので返さない。
#[test]
fn an_exposed_certificate_yields_its_thumbprint_and_other_faults_do_not() {
    let load = |ops: &FakeRevOps| match load_pinned_user_cert(ops, &ops.place()) {
        UserCertPin::Unusable(found) => found,
        other => panic!("通らないはず: {other:#?}"),
    };
    // 鍵が別のアカウントに読める。
    let ops = FakeRevOps::fresh();
    let cert = ops.put_cert();
    ops.loosen_file(&user_tls().join(KEY_FILE));
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Exposed);
    assert_eq!(found.exposed_thumbprint(), Some(cert));
    assert!(matches!(found.seen, UncheckedUserCert::Seen(_)));

    // `tls\` を別のアカウントが書き換えられる（鍵と証明書を差し替えられた恐れ）。
    let ops = FakeRevOps::fresh();
    let cert = ops.put_cert();
    ops.loosen_dir(&user_tls());
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Exposed);
    assert_eq!(found.exposed_thumbprint(), Some(cert));

    // null DACL の鍵も露出。
    let ops = FakeRevOps::fresh();
    let cert = ops.put_cert();
    ops.files
        .borrow_mut()
        .get_mut(&user_tls().join(KEY_FILE))
        .unwrap()
        .security
        .dacl
        .aces = None;
    assert_eq!(load(&ops).exposed_thumbprint(), Some(cert));

    // 所有者が違う: 信じないが、露出としては扱わない（形は読める）。
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.dirs.borrow_mut().get_mut(&user_tls()).unwrap().owner = EVIL.to_string();
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Untrusted);
    assert_eq!(found.exposed_thumbprint(), None);
    assert!(matches!(found.seen, UncheckedUserCert::Seen(_)));

    // ハードリンクの鍵: 露出ではない。
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.files
        .borrow_mut()
        .get_mut(&user_tls().join(KEY_FILE))
        .unwrap()
        .security
        .links = 2;
    assert_eq!(load(&ops).problem.fault, UserFault::Untrusted);

    // 鍵が無い・証明書が読める形でない: 壊れ。
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.files.borrow_mut().remove(&user_tls().join(KEY_FILE));
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Broken);
    assert_eq!(found.exposed_thumbprint(), None);
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.files
        .borrow_mut()
        .get_mut(&user_tls().join(CERT_FILE))
        .unwrap()
        .contents = b"broken".to_vec();
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Broken);
    assert_eq!(found.seen, UncheckedUserCert::Malformed);

    // 開けない: 一時的。形も読めない。
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.open_error.borrow_mut().insert(
        user_tls().join(CERT_FILE),
        FileOpenError::Other("拒否".to_string()),
    );
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Transient);
    assert_eq!(found.seen, UncheckedUserCert::Unreadable);

    // リンクの先は読まない。
    let ops = FakeRevOps::fresh();
    ops.put_cert();
    ops.dirs
        .borrow_mut()
        .get_mut(&user_tls())
        .unwrap()
        .is_reparse_point = true;
    let found = load(&ops);
    assert_eq!(found.problem.fault, UserFault::Untrusted);
    assert_eq!(found.seen, UncheckedUserCert::Unreadable);
}

// --- 整える手順: 和と書き戻し -------------------------------------------------------------

/// 場面 12: 片方の記録が古い内容に戻された（片方だけに書ける相手）。和でもう片方から書き
/// 戻される。失効は消えない。結果は「整っている」（消失ではない）。
#[test]
fn scene_12_a_rolled_back_record_is_restored_from_the_other() {
    for rolled_back in RevocationSide::BOTH {
        let ops = FakeRevOps::settled(&[1, 2]);
        ops.put_record(rolled_back, Committed, &[1]);
        // 止まっている間も、照合は和なので失効は効く。
        assert!(read_user_revocations(&ops, &ops.place()).is_revoked(&thumb(2)));
        let report = ops.reconcile();
        assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
        assert!(report.warnings.is_empty(), "{report:#?}");
        assert_eq!(ops.writes().len(), 1, "{:?}", ops.writes());
        ops.assert_committed(&[thumb(1), thumb(2)]);
    }

    // 互いに片方にしか無い拇印を持つ: 両方へ書き戻す。
    let ops = FakeRevOps::settled(&[]);
    ops.put_record(A, Committed, &[1]);
    ops.put_record(B, Committed, &[2]);
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
    assert_eq!(ops.writes(), vec!["write A 確定 2", "write B 確定 2"]);
    ops.assert_committed(&[thumb(1), thumb(2)]);
}

/// 場面 18: 拇印の書き足しの途中で止まった（A だけ書けた）。照合は和なので、止まっている間も
/// 失効は効く。次の回の和で B に書き戻す。
#[test]
fn scene_18_an_interrupted_append_still_revokes_and_is_completed_next_time() {
    let ops = FakeRevOps::settled(&[1]);
    ops.writes_left.set(Some(1));
    let report = ops.revoke(thumb(5), RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert!(has_io_warning(&report));
    assert!(report.revocations.is_revoked(&thumb(5)));
    assert!(!report.revocations.is_settled());
    assert!(ops.record(A).unwrap().contains(&thumb(5)));
    assert!(!ops.record(B).unwrap().contains(&thumb(5)));

    ops.writes_left.set(None);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert_eq!(ops.writes(), vec!["write B 確定 2"]);
    ops.assert_committed(&[thumb(1), thumb(5)]);
}

// --- 整える手順: 消失 -------------------------------------------------------------------

/// 場面 13: 片方の記録が消えた・壊れた。利用者の証明書がある。消失として、残った拇印をすべて
/// 引き継ぎ、今の証明書を失効に入れて、両方の記録を直す。先に残っているほうを書く。結果は
/// 「通らない」で、次の回に「整っている」になる（証明書は失効したまま。作り直しで戻る）。
#[test]
fn scene_13_a_lost_record_with_a_certificate_revokes_it_and_repairs_both() {
    type Break = fn(&FakeRevOps, RevocationSide);
    let breaks: [(&str, Break); 3] = [
        ("消えた", |ops, side| {
            ops.files.borrow_mut().remove(&record_path(side));
        }),
        ("壊れた", |ops, side| ops.put_file(side, b"garbage\n")),
        ("途中で切れた", |ops, side| {
            let whole = encoded(Committed, &[1, 2]);
            ops.put_file(side, &whole[..whole.len() - 6]);
        }),
    ];
    for (what, break_it) in breaks {
        for lost in RevocationSide::BOTH {
            let kept = if lost == A { B } else { A };
            let ops = FakeRevOps::settled(&[1, 2]);
            let cert = ops.put_cert();
            break_it(&ops, lost);

            let report = ops.reconcile();
            assert_eq!(
                report.outcome,
                ReconcileOutcome::Refused,
                "{what} {lost:?}: {report:#?}"
            );
            assert!(report.saw_loss());
            assert_eq!(lost_sides(&report), vec![lost], "{what}");
            assert!(
                report
                    .warnings
                    .contains(&RevocationWarning::ExistingCertRevoked(cert)),
                "{what}: {report:#?}"
            );
            // 先に残っているほう、次に欠けたほう。
            let name = |side| if side == A { "A" } else { "B" };
            assert_eq!(
                ops.writes(),
                vec![
                    format!("write {} 確定 3", name(kept)),
                    format!("write {} 確定 3", name(lost)),
                ],
                "{what}"
            );
            ops.assert_committed(&[thumb(1), thumb(2), cert]);
            assert!(report.revocations.is_revoked(&cert));

            // 次の回は整っている。証明書は失効したまま。
            let report = ops.reconcile();
            assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{what}");
            assert!(report.warnings.is_empty(), "{what}: {report:#?}");
            assert!(ops.writes().is_empty());
            assert!(report.revocations.is_revoked(&cert));
        }
    }
}

/// 場面 14: 片方の記録が消えた・壊れた。利用者の証明書が無い（サービスだけの PC など）。消失
/// として残った拇印を引き継いで直し、警告を出す。失効に足すものは無い。次の回に戻る。
#[test]
fn scene_14_a_lost_record_without_a_certificate_is_repaired_from_the_other() {
    let ops = FakeRevOps::settled(&[1]);
    ops.put_file(B, b"");
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![B]);
    assert_eq!(report.warnings.len(), 1, "{report:#?}");
    // 残っているほうは同じ中身なので書かず、欠けたほうだけ作り直す。
    assert_eq!(ops.writes(), vec!["write B 確定 1"]);
    ops.assert_committed(&[thumb(1)]);
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
}

/// 上限（256 KiB）を超えた記録は、形が壊れているのと同じ恒久的な破損。一時的な読み取り失敗
/// （`Incomplete`）にせず、消失として B から直す。次の回は整う。
#[test]
fn an_oversized_record_is_loss_and_is_rebuilt_not_incomplete() {
    let ops = FakeRevOps::settled(&[1, 2]);
    ops.put_file(A, &vec![b'x'; MAX_REVOCATIONS_BYTES + 1]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![A]);
    ops.assert_committed(&[thumb(1), thumb(2)]);
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
}

/// 場面 22: 利用者が案内どおりに `BantoHub\` ごと消した。A も消えるので場面 14 になり、B の
/// 拇印を引き継いで A を直す（フォルダも作り直す）。`tls\` だけ消したのなら、記録はそのまま。
#[test]
fn scene_22_deleting_the_product_folder_loses_a_and_it_is_rebuilt_from_b() {
    let ops = FakeRevOps::settled(&[1, 2]);
    ops.put_cert();
    ops.remove_tree(&product());
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![A]);
    // 証明書も消えているので、失効に足すものは無い。
    assert_eq!(report.warnings.len(), 1, "{report:#?}");
    assert_eq!(
        ops.steps(),
        vec![
            "lock",
            "mkdir BantoHub",
            "mkdir revocations",
            "write A 確定 2"
        ]
    );
    ops.assert_committed(&[thumb(1), thumb(2)]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);

    // `tls\` だけ消した: 記録はそのまま（整っている）。
    let ops = FakeRevOps::settled(&[1]);
    ops.put_cert();
    ops.remove_tree(&user_tls());
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(ops.writes().is_empty());
}

/// 場面 15: 片方の記録の置き場が確かめに通らないまま。毎回、消失として扱う（結果は
/// 「通らない」）。ロックファイルは A・B の外にあるので、ロックを取って、残ったほうに今の
/// 証明書の拇印を入れる。通らないほうには書かない（形が読めた拇印は和に入れる）。置き場が
/// 直れば、次の回で記録が直る。
#[test]
fn scene_15_an_untrusted_place_is_loss_every_time_and_is_never_written() {
    for bad in RevocationSide::BOTH {
        let good = if bad == A { B } else { A };
        let ops = FakeRevOps::settled(&[1]);
        // 通らない置き場の記録にだけある拇印（形は読める）。
        ops.put_record(bad, Committed, &[1, 2]);
        let cert = ops.put_cert();
        ops.loosen_dir(&side_dir(bad));
        let before = ops.files.borrow()[&record_path(bad)].contents.clone();

        for round in 0..2 {
            let report = ops.reconcile();
            assert_eq!(
                report.outcome,
                ReconcileOutcome::Refused,
                "{bad:?} {round}: {report:#?}"
            );
            assert_eq!(lost_sides(&report), vec![bad]);
            assert!(report.warnings.iter().any(|warning| matches!(
                warning,
                RevocationWarning::NotRepaired { side, .. } if *side == bad
            )));
            assert_eq!(ops.steps()[0], "lock");
            // 残ったほうに、和（通らないほうから読めた拇印も）と今の証明書が入る。
            assert_eq!(
                ops.stored(good).unwrap().1.len(),
                3,
                "{:?}",
                ops.stored(good)
            );
            assert!(ops.record(good).unwrap().contains(&cert));
            assert!(ops.record(good).unwrap().contains(&thumb(2)));
            // 通らないほうには書いていない。
            assert_eq!(ops.files.borrow()[&record_path(bad)].contents, before);
            assert!(report.revocations.is_revoked(&cert));
            if round == 1 {
                // 2 回目は、残ったほうももう同じ中身なので何も書かない。
                assert!(ops.writes().is_empty(), "{:?}", ops.writes());
            }
        }

        // 置き場が直れば、次の回で記録が直る（和で書き戻される）。
        ops.tighten_dir(&side_dir(bad));
        let report = ops.reconcile();
        assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
        ops.assert_committed(&[thumb(1), thumb(2), cert]);
    }
}

/// 場面 15 の変形: 置き場のフォルダは通るが、記録のファイルだけが別のアカウントに書き換え
/// られる形。消失として扱い、置き換えで作り直す（新しいファイルはフォルダの ACL を継ぐ）。
#[test]
fn scene_15_a_record_file_open_to_others_is_loss_and_is_replaced() {
    let ops = FakeRevOps::settled(&[1]);
    ops.put_record(B, Committed, &[1, 2]);
    ops.loosen_file(&record_path(B));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![B]);
    assert_eq!(ops.writes(), vec!["write A 確定 2", "write B 確定 2"]);
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
    ops.assert_committed(&[thumb(1), thumb(2)]);
}

/// 場面 16: 両方の記録が消えた。利用者の証明書がある。「無い・無い」＋証明書ありなので、その
/// 証明書を失効に入れた記録を作る（作り直しで戻る）。
#[test]
fn scene_16_both_records_gone_with_a_certificate_creates_records_revoking_it() {
    let ops = FakeRevOps::fresh();
    let cert = ops.put_cert();
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert_eq!(
        report.warnings,
        vec![RevocationWarning::ExistingCertRevoked(cert)]
    );
    assert_eq!(
        ops.writes(),
        vec!["write A 準備中 1", "write B 確定 1", "write A 確定 1"]
    );
    ops.assert_committed(&[cert]);
    assert!(report.revocations.is_revoked(&cert));
    let entry = ops.record(A).unwrap().entries()[0];
    assert_eq!(entry.place, RevokedCertPlace::User);
    assert_eq!(entry.recorded_at, NOW);

    // 確かめに通らない証明書（鍵が露出している）でも、形が読めれば失効に入れる。
    let ops = FakeRevOps::fresh();
    let cert = ops.put_cert();
    ops.loosen_file(&user_tls().join(KEY_FILE));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    ops.assert_committed(&[cert]);
}

/// 場面 17: 両方の記録が消えた（証明書なし）、または両方とも同じ古い内容に戻された。見えない
/// （両方に書ける相手。受容する残りのリスク）。消えたときは未初期化として作り直される。
#[test]
fn scene_17_losing_or_rolling_back_both_records_is_not_visible() {
    let ops = FakeRevOps::settled(&[1, 2]);
    ops.remove_tree(&rev_a());
    ops.remove_tree(&rev_b());
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(report.warnings.is_empty(), "{report:#?}");
    ops.assert_committed(&[]);

    let ops = FakeRevOps::settled(&[1, 2]);
    ops.put_record(A, Committed, &[1]);
    ops.put_record(B, Committed, &[1]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert!(report.warnings.is_empty());
    assert!(!report.revocations.is_revoked(&thumb(2)));
}

/// 場面 19: 消失の直しの途中で止まった。残ったほうには、もう拇印が入っている。次の回が同じ
/// 判定で繰り返す。
#[test]
fn scene_19_an_interrupted_loss_repair_is_repeated_with_the_same_verdict() {
    let ops = FakeRevOps::settled(&[1]);
    let cert = ops.put_cert();
    ops.files.borrow_mut().remove(&record_path(B));
    ops.writes_left.set(Some(1));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert!(has_io_warning(&report));
    assert_eq!(ops.writes(), vec!["write A 確定 2", "write B 失敗"]);
    assert!(ops.record(A).unwrap().contains(&cert));
    assert_eq!(ops.stored(B), None);
    assert!(report.revocations.is_revoked(&cert));

    // 次の回も消失（B がまだ無い）。残ったほうは書き直さず、欠けたほうを作る。
    ops.writes_left.set(None);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert_eq!(lost_sides(&report), vec![B]);
    assert_eq!(ops.writes(), vec!["write B 確定 2"]);
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
    ops.assert_committed(&[thumb(1), cert]);
}

/// 「上のどれでもない」のほかの形: B が `準備中`、両方とも壊れている、A が無くて B だけある。
#[test]
fn other_shapes_outside_the_table_are_loss() {
    // B が準備中（準備中になるのは A だけ）。
    let ops = FakeRevOps::settled(&[1]);
    ops.put_record(B, Pending, &[1, 2]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![B]);
    ops.assert_committed(&[thumb(1), thumb(2)]);

    // A が準備中で、B が壊れている: 続きとしては扱わない。
    let ops = FakeRevOps::fresh();
    ops.put_record(A, Pending, &[1]);
    ops.put_file(B, b"x");
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert_eq!(ops.writes(), vec!["write A 確定 1", "write B 確定 1"]);

    // 両方とも壊れている: 利用者の証明書の拇印だけの記録を、未初期化と同じ順で作る。
    let ops = FakeRevOps::fresh();
    ops.put_file(A, b"x");
    ops.put_file(B, b"y");
    let cert = ops.put_cert();
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![A, B]);
    assert_eq!(
        ops.writes(),
        vec!["write A 準備中 1", "write B 確定 1", "write A 確定 1"]
    );
    ops.assert_committed(&[cert]);

    // A が無くて B だけある。
    let ops = FakeRevOps::fresh();
    ops.put_record(B, Committed, &[3]);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert_eq!(lost_sides(&report), vec![A]);
    ops.assert_committed(&[thumb(3)]);
}

/// 記録を新しく作る・直すときに、利用者の証明書があるかどうかが分からない（途中がリンク・
/// 開けない）なら、作らない・直さない（その証明書を失効に入れ損ねた記録を「整っている」に
/// しないため）。
#[test]
fn an_unknowable_certificate_blocks_creating_and_repairing_records() {
    let unknowable: [fn(&FakeRevOps); 2] = [
        |ops| {
            ops.dirs
                .borrow_mut()
                .get_mut(&user_tls())
                .unwrap()
                .is_reparse_point = true;
        },
        |ops| {
            ops.open_error.borrow_mut().insert(
                user_tls().join(CERT_FILE),
                FileOpenError::Other("拒否".to_string()),
            );
        },
    ];
    for make_unknowable in unknowable {
        // 未初期化: この回はできなかった。
        let ops = FakeRevOps::fresh();
        ops.put_cert();
        make_unknowable(&ops);
        let report = ops.reconcile();
        assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
        assert!(has_io_warning(&report));
        assert!(ops.writes().is_empty());

        // 消失: 通らない（直さない）。
        let ops = FakeRevOps::settled(&[1]);
        ops.put_cert();
        make_unknowable(&ops);
        ops.files.borrow_mut().remove(&record_path(A));
        let report = ops.reconcile();
        assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
        assert!(report.saw_loss());
        assert!(ops.writes().is_empty());

        // 整っている記録は、証明書を見ないので影響を受けない。
        let ops = FakeRevOps::settled(&[1]);
        ops.put_cert();
        make_unknowable(&ops);
        assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);
    }
}

// --- ロックと I/O ----------------------------------------------------------------------

/// 場面 23: 記録に書けない・ロックが上限までに取れない。結果は「この回はできなかった」。
/// ロックは上限まで待ち、取れなければ何も書かない。照合の読み口はロック無しで使える。
#[test]
fn scene_23_a_held_lock_or_a_failed_write_is_incomplete_not_refused() {
    // ほかのプロセスが握ったまま。
    let ops = FakeRevOps::settled(&[1]);
    ops.put_record(B, Committed, &[1, 2]);
    ops.lock_busy.set(u32::MAX);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert_eq!(
        report.warnings,
        vec![RevocationWarning::LockTimeout(LOCK_WAIT)]
    );
    assert_eq!(*ops.slept.borrow(), vec![LOCK_POLL; 4]);
    assert!(ops.steps().is_empty(), "{:?}", ops.steps());
    // ロック無しで読める記録との照合は続けられる。
    assert!(report.revocations.is_revoked(&thumb(2)));

    // 失効の書き足しも、ロックが取れなければ書けない（呼ぶ側は記録に無いことが分かる）。
    let report = ops.revoke(thumb(9), RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete);
    assert!(!report.revocations.is_revoked(&thumb(9)));

    // 間隔が上限を割り切れないときも、上限ちょうどまで待つ。
    let ops = FakeRevOps::settled(&[]);
    ops.lock_busy.set(u32::MAX);
    let options = ReconcileOptions {
        lock_wait: Duration::from_millis(120),
        lock_poll: Duration::from_millis(50),
    };
    let report = reconcile_user_revocations(&ops, &ops.place(), &options);
    assert_eq!(
        report.warnings,
        vec![RevocationWarning::LockTimeout(Duration::from_millis(120))]
    );
    assert_eq!(
        ops.slept.borrow().iter().sum::<Duration>(),
        Duration::from_millis(120)
    );

    // ディスクが一杯（書けない）。
    let ops = FakeRevOps::settled(&[1]);
    ops.put_record(B, Committed, &[1, 2]);
    ops.writes_left.set(Some(0));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert!(has_io_warning(&report));
    assert!(!report.saw_loss());

    // 記録が I/O の誤りで読めない: 判定せず、何も書かない。
    let ops = FakeRevOps::settled(&[1]);
    let cert = ops.put_cert();
    ops.open_error
        .borrow_mut()
        .insert(record_path(A), FileOpenError::InUse);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert!(has_io_warning(&report));
    assert!(!report.saw_loss());
    assert!(ops.writes().is_empty());
    assert!(!report.revocations.is_revoked(&cert));
    assert!(matches!(
        report.revocations.side(A),
        SideContent::Unread { .. }
    ));
}

/// 場面 24: 2 つのプロセスが同時に整える。後から来たほうは上限付きで待ち、取れたら読み直して
/// 「整っている」になり、何もしない。
#[test]
fn scene_24_the_second_process_waits_for_the_lock_then_finds_it_settled() {
    let ops = FakeRevOps::settled(&[1]);
    ops.lock_busy.set(2);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty());
    assert_eq!(*ops.slept.borrow(), vec![LOCK_POLL; 2]);
    assert_eq!(ops.steps(), vec!["lock"]);

    // 先のプロセスが初期化の途中（A が準備中）でも、待って取れたときの状態から続ける。
    let ops = FakeRevOps::fresh();
    ops.put_record(A, Pending, &[]);
    ops.files.borrow_mut().insert(lock_path(), user_file(b""));
    ops.lock_busy.set(1);
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert_eq!(ops.writes(), vec!["write B 確定 0", "write A 確定 0"]);
}

/// 場面 27: 両方の記録の置き場、またはロックファイルが確かめに通らない（共通の親が緩んでいる
/// 間など）。結果は「通らない」。ロック無しでは何も書かない。形が読めた記録での照合は続け
/// られる。置き場が直った後は、記録の内容で判定し直す。
#[test]
fn scene_27_untrusted_places_or_lock_refuse_and_write_nothing() {
    // 共通の親 `%LOCALAPPDATA%` が緩んでいる: A・B・ロックが揃って通らない。
    let ops = FakeRevOps::settled(&[1]);
    ops.put_cert();
    ops.loosen_dir(&local());
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert!(matches!(
        report.warnings.as_slice(),
        [RevocationWarning::LockRefused(problem)] if problem.fault == UserFault::Exposed
    ));
    assert!(ops.steps().is_empty(), "{:?}", ops.steps());
    // 形が読めた記録での照合は続く。
    assert!(report.revocations.is_revoked(&thumb(1)));
    assert!(!report.revocations.is_settled());
    assert!(matches!(
        report.revocations.side(A),
        SideContent::Lost {
            place_trusted: false,
            ..
        }
    ));
    // 失効の書き足しも書かない。
    let report = ops.revoke(thumb(9), RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert!(ops.steps().is_empty());
    assert!(!report.revocations.is_revoked(&thumb(9)));
    // 直った後は、記録の内容で判定し直す。
    ops.tighten_dir(&local());
    assert_eq!(ops.reconcile().outcome, ReconcileOutcome::Consistent);

    // 両方の記録の置き場が通らない（ロックは取れる）: 消失だが、どちらにも書かない。
    let ops = FakeRevOps::settled(&[1]);
    let cert = ops.put_cert();
    ops.loosen_dir(&rev_a());
    ops.loosen_dir(&rev_b());
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert_eq!(lost_sides(&report), vec![A, B]);
    assert_eq!(ops.steps(), vec!["lock"]);
    assert!(report.revocations.is_revoked(&thumb(1)));
    // 今の証明書を失効に入れたかったが、書けていない（呼ぶ側は結果「通らない」で使わない）。
    assert!(!report.revocations.is_revoked(&cert));

    // ロックファイルそのものが通らない（別のアカウントが書ける・リンク・フォルダ・所有者）。
    type Spoil = fn(&mut FileEntry);
    let spoils: [(Spoil, UserFault); 4] = [
        (
            |lock| {
                if let Some(aces) = lock.security.dacl.aces.as_mut() {
                    aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0));
                }
            },
            UserFault::Exposed,
        ),
        (|lock| lock.security.links = 2, UserFault::Untrusted),
        (
            |lock| lock.security.is_reparse_point = true,
            UserFault::Untrusted,
        ),
        (
            |lock| lock.security.dacl.owner = EVIL.to_string(),
            UserFault::Untrusted,
        ),
    ];
    for (spoil, fault) in spoils {
        let ops = FakeRevOps::settled(&[1]);
        ops.put_record(B, Committed, &[1, 2]);
        spoil(ops.files.borrow_mut().get_mut(&lock_path()).unwrap());
        let report = ops.reconcile();
        assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
        assert!(matches!(
            report.warnings.as_slice(),
            [RevocationWarning::LockRefused(problem)] if problem.fault == fault
        ));
        assert!(ops.writes().is_empty());
        assert_eq!(ops.stored(A).unwrap().1.len(), 1);
    }
    // 本当の場所が違うロックファイル。
    let ops = FakeRevOps::settled(&[1]);
    ops.files.borrow_mut().get_mut(&lock_path()).unwrap().real = Some(local().join("elsewhere"));
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert!(ops.writes().is_empty());
}

/// 作ったフォルダが確かめに通らない（親から緩い ACL を継いだ）なら、そこには書かない。
#[test]
fn a_freshly_created_folder_is_rechecked_before_writing() {
    let ops = FakeRevOps::fresh();
    let mut loose = user_dir();
    loose.aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0x03));
    *ops.new_dir.borrow_mut() = loose;
    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert!(report
        .warnings
        .iter()
        .any(|warning| matches!(warning, RevocationWarning::NotRepaired { side: A, .. })));
    assert!(ops.writes().is_empty(), "{:?}", ops.steps());
    assert!(ops.files.borrow().get(&record_path(A)).is_none());
}

/// 残った一時ファイルは、ロックを握っている間に、読まずに片付ける。ロック無しの読み口は
/// 触らない。
#[test]
fn stale_temp_files_are_removed_unread_under_the_lock() {
    let ops = FakeRevOps::settled(&[1]);
    let temp = rev_a().join(format!(".{REVOCATIONS_FILE}.tmp-0000abcd"));
    ops.files
        .borrow_mut()
        .insert(temp.clone(), user_file(b"half"));

    ops.log.borrow_mut().clear();
    read_user_revocations(&ops, &ops.place());
    assert!(ops.files.borrow().contains_key(&temp));
    assert!(ops.steps().is_empty());

    let report = ops.reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent);
    assert_eq!(ops.steps(), vec!["lock", "tidy 1"]);
    assert!(!ops.files.borrow().contains_key(&temp));
}

/// 置き場の指定の並びが使えない（A と B が同じ）なら、何もしない。
#[test]
fn a_bad_layout_is_refused_before_anything_is_opened() {
    struct Same;
    impl UserTlsPlace for Same {
        fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
            let kind = if kind == UserPlaceKind::RevocationsB {
                UserPlaceKind::RevocationsA
            } else {
                kind
            };
            DefaultUserTlsPlace::new(local(), ME.to_string()).spec(kind)
        }
    }
    let ops = FakeRevOps::fresh();
    let report = reconcile_user_revocations(&ops, &Same, &ReconcileOptions::default());
    assert_eq!(report.outcome, ReconcileOutcome::Refused);
    assert!(matches!(
        report.warnings.as_slice(),
        [RevocationWarning::Layout(_)]
    ));
    assert!(ops.steps().is_empty());
}

#[test]
fn default_options_wait_five_seconds() {
    let options = ReconcileOptions::default();
    assert_eq!(options.lock_wait, Duration::from_secs(5));
    assert_eq!(options.lock_poll, Duration::from_millis(50));
}

// --- 実 Windows ------------------------------------------------------------------------

/// 実 Windows の書き込みの部品（`win::WindowsTlsTrustOps` の [`RevocationOps`]）: 読み取り専用で
/// 握ったフォルダのハンドル基準で、フォルダを作る・置き換える・残った一時ファイルを消す・
/// 排他に開く。置き場の確かめは通さない（テストの実行ファイルの隣の一時フォルダ。
/// `%LOCALAPPDATA%` と違って、パッケージの振り向けを受けない）。
#[cfg(windows)]
#[test]
fn windows_ops_write_replace_tidy_and_lock_through_the_pinned_handle() {
    use crate::win::WindowsTlsTrustOps;

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let base = std::env::current_exe()
        .expect("テストの実行ファイル")
        .parent()
        .expect("実行ファイルのフォルダ")
        .join(format!(
            "tls-trust-revocation-{}-{nonce}",
            std::process::id()
        ));
    std::fs::create_dir(&base).expect("一時フォルダを作る");
    let _cleanup = Cleanup(base.clone());

    let ops = WindowsTlsTrustOps;
    let root = ops.pin_dir(&base).expect("開く").expect("ある");
    // 読み取り専用の pin のまま、子のフォルダを作れる（既にあっても Ok）。
    ops.create_child_dir(&root, "records")
        .expect("フォルダを作る");
    ops.create_child_dir(&root, "records")
        .expect("既にあっても Ok");
    let dir = ops
        .open_child_dir(&root, "records")
        .expect("開く")
        .expect("ある");

    // 置き換え: 新しく作る・上書きする。握ったハンドルから読み戻す。
    let read = |name: &str| -> Option<Vec<u8>> {
        ops.open_child_file(&dir, name)
            .expect("開く")
            .map(|mut file| file.read_all(MAX_REVOCATIONS_BYTES).expect("読む"))
    };
    assert_eq!(read(REVOCATIONS_FILE), None);
    let first = encoded(Pending, &[1]);
    let second = encoded(Committed, &[1, 2]);
    ops.write_atomic(&dir, REVOCATIONS_FILE, &first)
        .expect("書く");
    assert_eq!(read(REVOCATIONS_FILE), Some(first));
    ops.write_atomic(&dir, REVOCATIONS_FILE, &second)
        .expect("置き換える");
    assert_eq!(read(REVOCATIONS_FILE), Some(second.clone()));
    let names = || -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(base.join("records"))
            .expect("一覧")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    // 一時ファイルを残さない。
    assert_eq!(names(), vec![REVOCATIONS_FILE]);

    // ロック無しの読み手が一瞬握っている間の置き換えは、少し待って通る。
    let held = ops
        .open_child_file(&dir, REVOCATIONS_FILE)
        .expect("開く")
        .expect("ある");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        drop(held);
    });
    let third = encoded(Committed, &[1, 2, 3]);
    ops.write_atomic(&dir, REVOCATIONS_FILE, &third)
        .expect("読み手が放した後に置き換わる");
    release.join().unwrap();
    assert_eq!(read(REVOCATIONS_FILE), Some(third.clone()));
    assert_eq!(names(), vec![REVOCATIONS_FILE]);

    // 残った一時ファイルだけを消す（似た名前・記録そのものは残す）。
    let stale = format!(".{REVOCATIONS_FILE}.tmp-0000abcd");
    let other = format!(".{REVOCATIONS_FILE}.tmp-zzzz");
    let notes = "notes.txt".to_string();
    for name in [&stale, &other, &notes] {
        std::fs::write(base.join("records").join(name), b"x").expect("置く");
    }
    ops.remove_stale_temps(&dir, REVOCATIONS_FILE)
        .expect("片付ける");
    let mut expected = vec![other, notes, REVOCATIONS_FILE.to_string()];
    expected.sort();
    assert_eq!(names(), expected);
    assert_eq!(read(REVOCATIONS_FILE), Some(third));

    // 排他の open: 無ければ None、作れば開ける、握っている間は 2 つ目が「握られている」。
    let lock = "records.lock";
    assert!(ops
        .open_child_file_exclusive(&root, lock)
        .expect("開く")
        .is_none());
    ops.create_file_if_absent(&root, lock).expect("作る");
    ops.create_file_if_absent(&root, lock)
        .expect("既にあっても Ok");
    let first = ops
        .open_child_file_exclusive(&root, lock)
        .expect("開く")
        .expect("ある");
    assert!(!TlsFile::security(&first).is_dir);
    assert!(matches!(
        ops.open_child_file_exclusive(&root, lock),
        Err(FileOpenError::InUse)
    ));
    drop(first);
    assert!(ops
        .open_child_file_exclusive(&root, lock)
        .expect("放した後は開ける")
        .is_some());
    // 握っても、消えない・中身も変わらない。
    assert_eq!(std::fs::read(base.join(lock)).expect("残っている"), b"");
}

/// 実 Windows の置き場（起点 `anchor` の下に一時の名前で作る 4 つ）。
#[cfg(windows)]
struct RealPlace {
    anchor: PathBuf,
    name: String,
    owners: Vec<String>,
    anchor_is_local_app_data: bool,
}

#[cfg(windows)]
impl RealPlace {
    /// `anchor` の DACL にいる 3 者以外の相手（AppContainer の SID を除く）を owners に足す
    /// （PR1a の試験と同じ。既定でない ACL の PC・`%LOCALAPPDATA%` の外の起点でも通すため）。
    fn new(anchor: &Path, name: String, anchor_is_local_app_data: bool) -> Self {
        use banto_safe_root::acl::{ADMINISTRATORS_SID, SYSTEM_SID};

        let ops = crate::win::WindowsTlsTrustOps;
        let me = ops.current_user_sid().expect("今の利用者の SID");
        let pinned = ops
            .pin_dir(anchor)
            .expect("起点を開く")
            .expect("起点がある");
        let security = TlsDir::security(&pinned).expect("起点の ACL");
        let mut extra: Vec<String> = security
            .dacl
            .aces
            .expect("起点の DACL は null ではない")
            .into_iter()
            .map(|ace| ace.sid)
            .chain([security.dacl.owner])
            .filter(|sid| sid != &me && sid != SYSTEM_SID && sid != ADMINISTRATORS_SID)
            .filter(|sid| !is_app_container_sid(sid))
            .collect();
        extra.sort();
        extra.dedup();
        if !extra.is_empty() {
            eprintln!(
                "起点（{}）の ACL に 3 者以外の相手がいます（{extra:?}）。owners に加えて続けます",
                anchor.display()
            );
        }
        let mut owners = vec![me];
        owners.extend(extra);
        Self {
            anchor: anchor.to_path_buf(),
            name,
            owners,
            anchor_is_local_app_data,
        }
    }

    fn product(&self) -> PathBuf {
        self.anchor.join(&self.name)
    }
    fn b_dir(&self) -> PathBuf {
        self.anchor.join(format!("{}-B", self.name))
    }
    fn lock_name(&self) -> String {
        format!("{}.lock", self.name)
    }
    fn record(&self, side: RevocationSide) -> PathBuf {
        match side {
            A => self.product().join(USER_REVOCATIONS_A_DIR),
            B => self.b_dir(),
        }
        .join(REVOCATIONS_FILE)
    }
}

#[cfg(windows)]
impl UserTlsPlace for RealPlace {
    fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
        let (dirs, file) = match kind {
            UserPlaceKind::Tls => (vec![self.name.clone(), TLS_DIR.to_string()], None),
            UserPlaceKind::RevocationsA => (
                vec![self.name.clone(), USER_REVOCATIONS_A_DIR.to_string()],
                None,
            ),
            UserPlaceKind::RevocationsB => (vec![format!("{}-B", self.name)], None),
            UserPlaceKind::RevocationsLock => (Vec::new(), Some(self.lock_name())),
        };
        UserPlaceSpec {
            anchor: self.anchor.clone(),
            dirs,
            file,
            owners: self.owners.clone(),
            policy: UserPlacePolicy {
                anchor_is_local_app_data: self.anchor_is_local_app_data,
                require_local_fixed_drive: true,
            },
        }
    }
}

/// 一時の置き場を消す（失敗しても落とさない）。
#[cfg(windows)]
struct RealCleanup(Vec<PathBuf>);

#[cfg(windows)]
impl Drop for RealCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_dir_all(path);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 実 Windows の置き場で、整える手順を一巡させる: 初回の作成（場面 1）→ 失効の書き足し
/// （場面 7）→ 片方を消して消失、次の回に整う（場面 14）→ 証明書がある状態で片方を壊すと、
/// その証明書が失効に入る（場面 13）→ ロックを握られている間は「この回はできなかった」
/// （場面 23）。書き込みが別の場所へ振り向けられる環境（パッケージの中の `%LOCALAPPDATA%`）
/// では、置き場が断られることだけを確かめて `false` を返す。
#[cfg(windows)]
fn real_round_trip(place: &RealPlace) -> bool {
    let ops = crate::win::WindowsTlsTrustOps;
    let lock_file = place.anchor.join(place.lock_name());
    let _cleanup = RealCleanup(vec![place.product(), place.b_dir(), lock_file.clone()]);
    check_place_layout(place).expect("一時の置き場の並び");
    let options = ReconcileOptions {
        lock_wait: Duration::from_millis(150),
        lock_poll: Duration::from_millis(25),
    };
    let reconcile = || reconcile_user_revocations(&ops, place, &options);

    // 作る前: ロック無しの読み口は「無い・無い」。何も作らない。
    let before = read_user_revocations(&ops, place);
    assert_eq!(before.side(A), &SideContent::Missing);
    assert_eq!(before.side(B), &SideContent::Missing);
    assert!(!lock_file.exists());

    // 初回（場面 1）。
    let report = reconcile();
    if report.outcome == ReconcileOutcome::Refused {
        // パッケージ（MSIX）の中: `%LOCALAPPDATA%` に新しく作ったものは別の場所へ振り向けられ、
        // 本当の場所が期待と違うので断られる（設計 §4.4.2 の場面 28。既定の置き場をそのまま
        // 使えない環境）。
        let reasons = format!("{:?}", report.warnings);
        assert!(
            reasons.contains("本当の場所"),
            "振り向け以外で断られた: {report:#?}"
        );
        eprintln!(
            "このプロセスの {} への書き込みは振り向けられています。置き場が断られることだけを確かめます",
            place.anchor.display()
        );
        return false;
    }
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty(), "{report:#?}");
    let empty = RevocationRecord::new(Committed, []).encode();
    assert_eq!(std::fs::read(place.record(A)).expect("A"), empty);
    assert_eq!(std::fs::read(place.record(B)).expect("B"), empty);
    assert!(lock_file.exists());

    // 失効を書き足す（場面 7）。
    let exposed = thumb(7);
    let report = record_user_revocation(&ops, place, &options, exposed, RevokedCertPlace::User);
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(read_user_revocations(&ops, place).is_revoked(&exposed));
    assert_eq!(
        std::fs::read(place.record(A)).expect("A"),
        std::fs::read(place.record(B)).expect("B")
    );

    // 片方を消す（場面 14）: 消失 → 残ったほうから直る → 次の回は整っている。
    std::fs::remove_file(place.record(B)).expect("B を消す");
    let report = reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert!(report.saw_loss());
    assert!(report.revocations.is_revoked(&exposed));
    let report = reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.warnings.is_empty(), "{report:#?}");

    // 証明書がある状態で片方が壊れる（場面 13）: その証明書が失効に入る。残った一時ファイルも
    // 片付く。
    let tls_dir = place.product().join(TLS_DIR);
    let pem = cert_pem();
    std::fs::create_dir(&tls_dir).expect("tls を作る");
    std::fs::write(tls_dir.join(CERT_FILE), &pem).expect("証明書を書く");
    std::fs::write(tls_dir.join(KEY_FILE), b"not a real key").expect("鍵を書く");
    let cert = CertThumbprint::of_pem(pem.as_bytes()).unwrap();
    assert_eq!(reconcile().outcome, ReconcileOutcome::Consistent);
    assert!(!read_user_revocations(&ops, place).is_revoked(&cert));
    std::fs::write(place.record(A), b"garbage").expect("A を壊す");
    let stale = place
        .record(A)
        .with_file_name(format!(".{REVOCATIONS_FILE}.tmp-0000abcd"));
    std::fs::write(&stale, b"half").expect("一時ファイルを残す");
    let report = reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Refused, "{report:#?}");
    assert!(
        report
            .warnings
            .contains(&RevocationWarning::ExistingCertRevoked(cert)),
        "{report:#?}"
    );
    assert!(!stale.exists());
    let report = reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Consistent, "{report:#?}");
    assert!(report.revocations.is_revoked(&cert));
    assert!(report.revocations.is_revoked(&exposed));
    for side in RevocationSide::BOTH {
        let record = RevocationRecord::parse(&std::fs::read(place.record(side)).expect("記録"))
            .expect("記録の形");
        assert_eq!(record.state(), Committed);
        assert!(record.contains(&cert) && record.contains(&exposed));
    }

    // ロックを握られている間（場面 23）: 上限まで待って「この回はできなかった」。放せば戻る。
    let lock_place = match open_user_place(&ops, place, UserPlaceKind::RevocationsLock) {
        Ok(UserPlaceState::Present(checked)) => checked,
        _ => panic!("ロックの置き場が通るはず"),
    };
    let held = ops
        .open_child_file_exclusive(lock_place.dir(), &place.lock_name())
        .expect("ロックを開く")
        .expect("ロックファイルがある");
    let report = reconcile();
    assert_eq!(report.outcome, ReconcileOutcome::Incomplete, "{report:#?}");
    assert_eq!(
        report.warnings,
        vec![RevocationWarning::LockTimeout(options.lock_wait)]
    );
    // 握られていても、ロック無しの照合はできる。
    assert!(report.revocations.is_revoked(&exposed));
    drop(held);
    assert_eq!(reconcile().outcome, ReconcileOutcome::Consistent);
    true
}

#[cfg(windows)]
fn real_scratch_name(prefix: &str) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{nonce}", std::process::id())
}

/// 実 Windows の `%LOCALAPPDATA%`（既定の起点）で一巡させる（一時の名前で作って片付ける）。
/// PR1a の `real_local_app_data_place_is_checked_on_windows` と同じく、既定でない環境では途中で
/// 終える: ACL に 3 者以外の相手がいる PC ではその相手を owners に足し、パッケージ（MSIX）の
/// 中で書き込みが振り向けられる環境では、置き場が断られることだけを確かめる。
#[cfg(windows)]
#[test]
fn real_local_app_data_revocations_round_trip_on_windows() {
    let local = crate::win::WindowsTlsTrustOps
        .local_app_data()
        .expect("%LOCALAPPDATA%");
    let place = RealPlace::new(&local, real_scratch_name("BantoHubRevocationTest"), true);
    real_round_trip(&place);
}

/// 同じ一巡を、振り向けを受けない起点（テストの実行ファイルの隣。起点が `%LOCALAPPDATA%` で
/// ある必要の無い方針 = フォークが差し替える形）で通す。パッケージの中から走らせる PC でも、
/// 実 Windows のハンドルで手順の全体が通ることを確かめるため。
#[cfg(windows)]
#[test]
fn real_revocations_round_trip_under_a_replaced_anchor_on_windows() {
    let anchor = std::env::current_exe()
        .expect("テストの実行ファイル")
        .parent()
        .expect("実行ファイルのフォルダ")
        .join(real_scratch_name("tls-trust-anchor"));
    std::fs::create_dir(&anchor).expect("起点を作る");
    let _cleanup = RealCleanup(vec![anchor.clone()]);
    let place = RealPlace::new(&anchor, "BantoHubTest".to_string(), false);
    assert!(real_round_trip(&place), "振り向けを受けない起点のはず");
}
