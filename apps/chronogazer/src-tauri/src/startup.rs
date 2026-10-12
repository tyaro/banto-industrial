//! 起動に失敗したときの「エラーの窓と対処の選択」（#392、2026-10-12 オーナー決定）。
//!
//! 起動（[`crate::launch_desktop`]）が止まる失敗は panic せず、窓を出して利用者に
//! 対処を選ばせる。判断（失敗 → 窓の種類と選択肢）と対処そのもの（作り直す・
//! バックアップから戻す・保存先を選び直す）は Tauri に依存しない
//! `chronogazer_core::startup_recovery` にあり（後の B2 のシェルも同じものを
//! 使う）、ここにあるのは窓と、選ばれた対処を実行して起動をやり直す配線だけ。
//!
//! - 既に起動中（A1）・旧形式の DB は、今までのエラーの窓
//!   （`static/legacy-db.html`、[`crate::open_startup_error_window`]）のまま。
//! - それ以外は対処の窓（`static/startup-recovery.html`、label
//!   [`RECOVERY_WINDOW_LABEL`]）。表に当てはまらない失敗（データの置き場所を
//!   作れない・画面を作れない など）は「終了」だけの窓で、理由とデータの置き場所を
//!   出す（デスクトップ版はログファイルを残さないので、その旨も出す）。
//!
//! # 窓と Rust のやりとり
//!
//! 窓は今までのエラーの窓と同じく**コマンドを呼ばない静的ページ**
//! （capabilities は `main` だけ）。表示する内容は初期化スクリプトで
//! `window.__CHRONOGAZER_RECOVERY__` に入れ、選ばれた対処は窓が
//! `/__recovery/<対処>?file=<バックアップ>` へ移ろうとするのを
//! `on_navigation` で受け取って取り消す（[`navigation_decision`]）。Rust は
//! 今出している選択肢に含まれる対処だけを受け付け（[`RecoveryGate`]）、処理中は
//! 二度押しを無視し、窓を閉じさせない（名前の変更の途中で終わらせない）。結果は
//! `window.__chronogazerRecovery.render(<JSON>)` で同じ窓に描き直す。
//! 確認（作り直す・戻す・既定に戻す）は窓の中で 1 回はさむ。
//!
//! # やり直しの方式
//!
//! 対処の後は**同じプロセスで** [`crate::launch_desktop`] をやり直す（理由は
//! `chronogazer_core::startup_recovery` のモジュール doc「やり直しの方式」）。
//! 失敗した起動は A1 の排他と pool を返している。やり直しが成功したら
//! メインの窓・トレイができた後に対処の窓を閉じる。失敗したら同じ窓に新しい
//! 理由と選択肢を出す（対処そのものの失敗 = フォルダに書けない・バックアップが
//! 壊れている・名前を変えられない、も同じ窓に理由を出して選び直せる）。

use std::path::PathBuf;
use std::sync::Mutex;

use banto_core::BantoError;
use chronogazer_core::backup::BackupInfo;
use chronogazer_core::db::InitDbError;
use chronogazer_core::engine::EngineStartError;
use chronogazer_core::startup_recovery::{
    self as recovery, FailureKind, RecoveryChoice, RecoveryError, RecoveryNote, StoreDirChoice,
    WindowPlan,
};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

/// 対処の窓の label（capabilities の対象外 = コマンドを呼べない）。
pub(crate) const RECOVERY_WINDOW_LABEL: &str = "startup-recovery";

/// 対処の窓のページ。
const RECOVERY_PAGE: &str = "startup-recovery.html";

/// 窓が対処を知らせるときに移ろうとするパスの頭（[`navigation_decision`]）。
const ACTION_PATH_PREFIX: &str = "/__recovery/";

/// デスクトップのデータの置き場所。
#[derive(Debug, Clone)]
pub(crate) struct LaunchPaths {
    /// アプリのデータディレクトリ（`data.dir` の相対パスの基準）。
    pub(crate) data_dir: PathBuf,
    /// 設定 DB。
    pub(crate) db_path: PathBuf,
}

impl LaunchPaths {
    /// アプリのデータディレクトリを決めて作る。
    pub(crate) fn resolve(app: &AppHandle) -> Result<Self, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|err| format!("アプリのデータの置き場所を決められませんでした: {err}"))?;
        std::fs::create_dir_all(&data_dir).map_err(|err| {
            format!(
                "アプリのデータの置き場所（{}）を作れませんでした: {err}",
                data_dir.display()
            )
        })?;
        Ok(Self {
            db_path: data_dir.join("chronogazer.sqlite3"),
            data_dir,
        })
    }
}

/// 起動の前にすること（対処の窓で選ばれたもの）。
pub(crate) enum Prelude {
    /// 何もしない（最初の起動）。
    None,
    /// DB を開けた後・`start` の前に保存先を書き換える。
    StoreDir(StoreDirChoice),
    /// 対処は済んでいる（名前の変更・リストアの適用）。対処の**前に**取った A1 の
    /// 排他をそのままエンジンへ引き継ぎ（#594 のオーナーレビュー P1）、監査に残す。
    Recovered {
        lock: recovery::RecoveryLock,
        note: RecoveryNote,
    },
}

/// [`crate::launch_desktop`] の失敗。
#[derive(Debug)]
pub(crate) enum LaunchError {
    /// エンジンの `open`/`start`。
    Engine(EngineStartError),
    /// `start` の前の設定（認証・LAN）の読み取り。
    Settings(BantoError),
    /// 選んだ保存先を使えなかった（DB は開けていて、閉じてある）。
    Recovery(RecoveryError),
    /// エンジンは動いたが画面（メインの窓・トレイ）を作れなかった。
    Window(tauri::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(err) => err.fmt(f),
            Self::Settings(err) => write!(f, "設定の読み取りに失敗しました: {err}"),
            Self::Recovery(err) => err.fmt(f),
            Self::Window(err) => write!(f, "画面を作れませんでした: {err}"),
        }
    }
}

/// 失敗の分類（窓に出す内容を決める）。`Recovery` は窓を変えない
/// （呼び出し側が前の分類のまま理由だけ出す）ので、ここでは `Other`。
fn kind_of(err: &LaunchError) -> FailureKind {
    match err {
        LaunchError::Engine(err) => recovery::classify(err),
        LaunchError::Settings(err) => recovery::classify_settings_error(err),
        LaunchError::Recovery(_) | LaunchError::Window(_) => FailureKind::Other,
    }
}

/// 窓に渡す 1 つの選択肢。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ChoiceView {
    id: &'static str,
    label: String,
    description: String,
    /// 確認の文（`None` は確認しない）。`{file}` は窓が選んだバックアップの名前に
    /// 置き換える。
    confirm: Option<String>,
    confirm_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct BackupView {
    file_name: String,
    created_at: String,
    size_bytes: u64,
}

impl From<BackupInfo> for BackupView {
    fn from(info: BackupInfo) -> Self {
        Self {
            file_name: info.file_name,
            created_at: info.created_at,
            size_bytes: info.size_bytes,
        }
    }
}

/// 対処の窓に描く内容（`static/startup-recovery.html` が読む）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RecoveryView {
    title: String,
    message: String,
    /// 詳しい理由（エラーの文言そのまま）。
    detail: String,
    path: String,
    log_hint: String,
    choices: Vec<ChoiceView>,
    backups: Vec<BackupView>,
    backups_error: Option<String>,
    /// 直前の対処の結果（失敗の理由）。
    notice: Option<String>,
}

impl RecoveryView {
    fn initialization_script(&self) -> String {
        let json = serde_json::to_string(self).expect("RecoveryView serializes");
        format!("window.__CHRONOGAZER_RECOVERY__ = {json};")
    }

    fn render_script(&self) -> String {
        let json = serde_json::to_string(self).expect("RecoveryView serializes");
        format!("window.__chronogazerRecovery && window.__chronogazerRecovery.render({json});")
    }
}

/// 分類ごとの見出しと説明（平易な日本語）。
fn failure_text(kind: FailureKind) -> (&'static str, &'static str) {
    match kind {
        FailureKind::StoreDirUnreadable => (
            "時系列データの保存先を読み取れません",
            "設定に保存されている時系列データの保存先（data.dir）を読み取れないため、起動を止めました。保存先のフォルダを選び直すか、既定の場所に戻すと起動を続けられます。どちらを選んでも、今のデータベースや記録は消しません。",
        ),
        FailureKind::StoreDirUnwritable => (
            "時系列データの保存先に書き込めません",
            "設定されている時系列データの保存先のフォルダに書き込めないため、起動を止めました（USB メモリやネットワークドライブが外れている、ドライブが無い、権限が無い など）。差し直す・権限を直すなどした後に「もう一度確かめる」を押すか、保存先のフォルダを選び直すか、既定の場所に戻すと起動を続けられます。どれを選んでも、今のデータベースや記録は消しません。",
        ),
        FailureKind::DatabaseCorrupt => (
            "データベースが壊れています",
            "設定のデータベース（アカウント・接続・タグ・設定）が壊れていて開けないため、起動を止めました。バックアップから戻すか、新しく作り直すと起動できます。どちらを選んでも、今のデータベースのファイルは名前を変えて残します（消しません）。",
        ),
        FailureKind::DatabaseUnopenable => (
            "データベースを開けません",
            "設定のデータベースを開けないか、読み書きできないため、起動を止めました。他のソフト（ウイルス対策・バックアップ・同期ソフトなど）が使っていないか、フォルダの権限や空き容量を確かめて、もう一度起動してみてください。直らないときは、バックアップから戻すか新しく作り直せます（今のファイルは名前を変えて残します）。",
        ),
        FailureKind::DatabaseFromNewerVersion => (
            "新しい版の ChronoGazer のデータベースです",
            "このデータベースは、この版より新しい ChronoGazer で使われたため開けません。新しい版の ChronoGazer で開いてください。ファイルには触れていません。",
        ),
        FailureKind::AlreadyRunning | FailureKind::LegacyDatabase | FailureKind::Other => (
            "ChronoGazer を起動できません",
            "起動の途中で失敗したため、起動を止めました。下の詳しい理由を控えてから終了してください。",
        ),
    }
}

/// 選択肢の文言。`default_dir` は既定の保存先（解決済み）、`db_name` は DB の
/// ファイル名。
fn choice_view(choice: RecoveryChoice, default_dir: &str, db_name: &str) -> ChoiceView {
    let (label, description, confirm, confirm_label) = match choice {
        RecoveryChoice::RetryStoreCheck => (
            "もう一度確かめる".to_string(),
            "USB メモリを差し直した・権限を直したなどの後に、同じ保存先でもう一度起動を試みます。".to_string(),
            None,
            None,
        ),
        RecoveryChoice::ChooseStoreDir => (
            "保存先のフォルダを選び直す".to_string(),
            "フォルダを選ぶと、その場所を設定に保存して起動を続けます。".to_string(),
            None,
            None,
        ),
        RecoveryChoice::ResetStoreDir => (
            "既定の場所に戻して起動する".to_string(),
            format!("既定の場所（{default_dir}）を保存先にして起動を続けます。"),
            Some(format!(
                "保存先を既定の場所（{default_dir}）に戻して起動します。これまでの記録は元の保存先に残りますが（消しません）、画面からは見えなくなります。元の場所を使いたいときは、後で設定画面から保存先を設定し直してください。よろしいですか？"
            )),
            Some("既定の場所に戻す".to_string()),
        ),
        RecoveryChoice::RestoreBackup => (
            "バックアップから戻す".to_string(),
            "保存してあるバックアップを選び、その時点の状態に戻して起動します。".to_string(),
            Some(format!(
                "バックアップ「{{file}}」の時点に戻して起動します。今のデータベースは「{db_name}.broken-日時」という名前に変えて残します（消しません）。バックアップより後に変えた設定・アカウントは戻りません。よろしいですか？"
            )),
            Some("このバックアップから戻す".to_string()),
        ),
        RecoveryChoice::RecreateDatabase => (
            "新しく作り直す".to_string(),
            "空のデータベースで起動します。".to_string(),
            Some(format!(
                "今のデータベースを「{db_name}.broken-日時」という名前に変えて残し、空のデータベースで起動します。アカウント・接続・タグ・設定は空になり、初回セットアップからやり直します（時系列データのファイルは消しません）。よろしいですか？"
            )),
            Some("作り直す".to_string()),
        ),
        RecoveryChoice::Quit => (
            "終了する".to_string(),
            "何もせずに ChronoGazer を終了します。".to_string(),
            None,
            None,
        ),
    };
    ChoiceView {
        id: choice.as_str(),
        label,
        description,
        confirm,
        confirm_label,
    }
}

/// 窓の内容を組む（バックアップの一覧は呼び出し側が読んで渡す）。
fn build_view(
    kind: FailureKind,
    choices: &[RecoveryChoice],
    detail: String,
    paths: &LaunchPaths,
    backups: Result<Vec<BackupInfo>, String>,
    notice: Option<String>,
) -> RecoveryView {
    let (title, message) = failure_text(kind);
    let default_dir = recovery::without_cur_dir(&chronogazer_core::collect::resolve_data_dir(
        &paths.data_dir,
        chronogazer_core::settings::default_data_dir(),
    ));
    let db_name = paths
        .db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "chronogazer.sqlite3".to_string());
    let (backups, backups_error) = match backups {
        Ok(list) => (list.into_iter().map(BackupView::from).collect(), None),
        Err(err) => (Vec::new(), Some(err)),
    };
    RecoveryView {
        title: title.to_string(),
        message: message.to_string(),
        detail,
        path: paths.db_path.display().to_string(),
        log_hint: format!(
            "デスクトップ版の ChronoGazer はログファイルを残しません。問い合わせるときは、上の詳しい理由を控えてください（選択してコピーできます）。データの置き場所: {}",
            paths.data_dir.display()
        ),
        choices: choices
            .iter()
            .map(|choice| choice_view(*choice, &default_dir.display().to_string(), &db_name))
            .collect(),
        backups,
        backups_error,
        notice,
    }
}

/// 窓が移ろうとした先の扱い。
#[derive(Debug, PartialEq, Eq)]
enum Navigation {
    /// 対処の窓のページそのもの（読み込みを許す）。
    Page,
    /// 対処の選択（移動は取り消す）。
    Action {
        choice: RecoveryChoice,
        file: Option<String>,
    },
    /// それ以外（取り消す）。
    Deny,
}

fn navigation_decision(url: &tauri::Url) -> Navigation {
    let path = url.path();
    if let Some(name) = path.strip_prefix(ACTION_PATH_PREFIX) {
        return match RecoveryChoice::parse(name) {
            Some(choice) => Navigation::Action {
                choice,
                file: url
                    .query_pairs()
                    .find(|(key, _)| key == "file")
                    .map(|(_, value)| value.into_owned())
                    .filter(|value| !value.is_empty()),
            },
            None => Navigation::Deny,
        };
    }
    if path == format!("/{RECOVERY_PAGE}") {
        Navigation::Page
    } else {
        Navigation::Deny
    }
}

/// 対処の窓の状態（`run()` が manage する）。
#[derive(Default)]
pub(crate) struct RecoveryGate(Mutex<Option<RecoverySession>>);

struct RecoverySession {
    paths: LaunchPaths,
    kind: FailureKind,
    choices: Vec<RecoveryChoice>,
    detail: String,
    /// 対処の実行中（二度押しを無視し、窓を閉じさせない）。
    busy: bool,
}

impl RecoveryGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<RecoverySession>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 処理中か（窓の × を止める）。
    fn busy(&self) -> bool {
        self.lock().as_ref().map(|s| s.busy).unwrap_or(false)
    }
}

/// 起動の失敗を窓で知らせる（`setup()` の最初の起動から）。
pub(crate) fn show_failure(app: &AppHandle, paths: &LaunchPaths, err: LaunchError) {
    eprintln!("banto: 起動に失敗しました: {err}");
    // 既に起動中・旧形式の DB は今までのエラーの窓のまま。
    match &err {
        LaunchError::Engine(EngineStartError::InstanceLock(failure)) => {
            crate::show_startup_notice(app, &crate::StartupErrorNotice::already_running(failure));
            return;
        }
        LaunchError::Engine(EngineStartError::Database(InitDbError::Legacy(legacy))) => {
            crate::show_startup_notice(app, &crate::StartupErrorNotice::new(legacy));
            return;
        }
        _ => {}
    }
    let kind = kind_of(&err);
    let engine_running = matches!(err, LaunchError::Window(_));
    let choices = choices_for(kind);
    let detail = err.to_string();
    let backups = backups_for(&choices, paths);
    let view = build_view(kind, &choices, detail.clone(), paths, backups, None);
    *app.state::<RecoveryGate>().lock() = Some(RecoverySession {
        paths: paths.clone(),
        kind,
        choices,
        detail,
        busy: false,
    });
    if let Err(open_err) = open_recovery_window(app, &view) {
        eprintln!("banto: エラー表示の窓を開けませんでした: {open_err}");
        if engine_running {
            // エンジンが動いているので、終了の後始末（`RunEvent::Exit`）を通す。
            app.exit(1);
        } else {
            std::process::exit(1);
        }
    }
}

/// 表に当てはまらない失敗（データの置き場所を作れない等）の「終了」だけの窓。
pub(crate) fn show_fatal(app: &AppHandle, detail: String) {
    eprintln!("banto: 起動に失敗しました: {detail}");
    let view = RecoveryView {
        title: failure_text(FailureKind::Other).0.to_string(),
        message: failure_text(FailureKind::Other).1.to_string(),
        detail: detail.clone(),
        path: String::new(),
        log_hint: "デスクトップ版の ChronoGazer はログファイルを残しません。問い合わせるときは、上の詳しい理由を控えてください（選択してコピーできます）。".to_string(),
        choices: vec![choice_view(RecoveryChoice::Quit, "", "")],
        backups: Vec::new(),
        backups_error: None,
        notice: None,
    };
    *app.state::<RecoveryGate>().lock() = Some(RecoverySession {
        paths: LaunchPaths {
            data_dir: PathBuf::new(),
            db_path: PathBuf::new(),
        },
        kind: FailureKind::Other,
        choices: vec![RecoveryChoice::Quit],
        detail,
        busy: false,
    });
    if let Err(err) = open_recovery_window(app, &view) {
        eprintln!("banto: エラー表示の窓を開けませんでした: {err}");
        std::process::exit(1);
    }
}

fn choices_for(kind: FailureKind) -> Vec<RecoveryChoice> {
    match recovery::plan(kind) {
        WindowPlan::Choices(choices) => choices,
        // 対処の窓の中で既に起動中・旧形式に当たった（やり直しの後）ときは
        // 「終了」だけ（理由は詳しい理由に出る）。
        WindowPlan::ExistingNotice => vec![RecoveryChoice::Quit],
    }
}

fn backups_for(choices: &[RecoveryChoice], paths: &LaunchPaths) -> Result<Vec<BackupInfo>, String> {
    if !choices.contains(&RecoveryChoice::RestoreBackup) {
        return Ok(Vec::new());
    }
    tauri::async_runtime::block_on(recovery::list_backups(&paths.db_path))
        .map_err(|err| format!("バックアップの一覧を読めませんでした: {err}"))
}

fn open_recovery_window(app: &AppHandle, view: &RecoveryView) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(RECOVERY_WINDOW_LABEL) {
        // やり直しの後にもう一度失敗したなど、窓が既にある。
        window.eval(view.render_script())?;
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let navigation_app = app.clone();
    let window = tauri::WebviewWindowBuilder::new(
        app,
        RECOVERY_WINDOW_LABEL,
        tauri::WebviewUrl::App(RECOVERY_PAGE.into()),
    )
    .title(view.title.clone())
    .inner_size(700.0, 620.0)
    .resizable(true)
    .center()
    .initialization_script(view.initialization_script())
    .on_navigation(move |url| match navigation_decision(url) {
        Navigation::Page => true,
        Navigation::Action { choice, file } => {
            let app = navigation_app.clone();
            tauri::async_runtime::spawn(async move { handle_action(app, choice, file).await });
            false
        }
        Navigation::Deny => false,
    })
    .build()?;
    let close_app = app.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            // 対処の途中（名前の変更・起動のやり直し）で終わらせない。
            if close_app.state::<RecoveryGate>().busy() {
                api.prevent_close();
            }
        }
    });
    Ok(())
}

/// 同じ窓を描き直す。
fn rerender(app: &AppHandle, view: &RecoveryView) {
    match app.get_webview_window(RECOVERY_WINDOW_LABEL) {
        Some(window) => {
            if let Err(err) = window.eval(view.render_script()) {
                eprintln!("banto: エラー表示の窓を更新できませんでした: {err}");
            }
        }
        None => eprintln!("banto: エラー表示の窓が見つかりません"),
    }
}

/// 今の分類のまま、理由（`notice`）を付けて描き直し、処理中を解く。
async fn rerender_with_notice(app: &AppHandle, notice: Option<String>) {
    let snapshot = {
        let gate = app.state::<RecoveryGate>();
        let mut session = gate.lock();
        let Some(session) = session.as_mut() else {
            return;
        };
        session.busy = false;
        (
            session.kind,
            session.choices.clone(),
            session.detail.clone(),
            session.paths.clone(),
        )
    };
    let (kind, choices, detail, paths) = snapshot;
    let backups = list_backups_async(&choices, &paths).await;
    rerender(
        app,
        &build_view(kind, &choices, detail, &paths, backups, notice),
    );
}

async fn list_backups_async(
    choices: &[RecoveryChoice],
    paths: &LaunchPaths,
) -> Result<Vec<BackupInfo>, String> {
    if !choices.contains(&RecoveryChoice::RestoreBackup) {
        return Ok(Vec::new());
    }
    recovery::list_backups(&paths.db_path)
        .await
        .map_err(|err| format!("バックアップの一覧を読めませんでした: {err}"))
}

/// 窓で選ばれた対処を実行し、起動をやり直す。
async fn handle_action(app: AppHandle, choice: RecoveryChoice, file: Option<String>) {
    // 今出している選択肢だけを、処理中でなければ受け付ける。
    let paths = {
        let gate = app.state::<RecoveryGate>();
        let mut session = gate.lock();
        let Some(session) = session.as_mut() else {
            return;
        };
        if session.busy || !session.choices.contains(&choice) {
            return;
        }
        session.busy = true;
        session.paths.clone()
    };

    let prelude = match choice {
        RecoveryChoice::Quit => {
            app.exit(0);
            return;
        }
        RecoveryChoice::ChooseStoreDir => match pick_folder(&app).await {
            Some(folder) => Prelude::StoreDir(StoreDirChoice::Folder(folder)),
            None => {
                // 取り消した: 何もしない。
                rerender_with_notice(&app, None).await;
                return;
            }
        },
        RecoveryChoice::ResetStoreDir => Prelude::StoreDir(StoreDirChoice::Default),
        // 何も変えずにやり直す（保存先の試し書きもやり直す）。
        RecoveryChoice::RetryStoreCheck => Prelude::None,
        RecoveryChoice::RecreateDatabase => {
            // 排他を取ってから退ける（取れなければ何も変えずに理由を返す）。
            match recovery::recreate_database(&paths.db_path, &recovery::broken_stamp_now()).await {
                Ok(recovery::Prepared { lock, set_aside }) => {
                    let mut note = RecoveryNote::new(RecoveryChoice::RecreateDatabase);
                    if let Some(name) = set_aside.broken_file_name() {
                        eprintln!("banto: 壊れた DB を {name} として残しました");
                        note = note.with("brokenFileName", name);
                    }
                    Prelude::Recovered { lock, note }
                }
                Err(err) => {
                    rerender_with_notice(&app, Some(err.to_string())).await;
                    return;
                }
            }
        }
        RecoveryChoice::RestoreBackup => {
            let Some(file) = file else {
                rerender_with_notice(&app, Some("戻すバックアップを選んでください。".to_string()))
                    .await;
                return;
            };
            match recovery::restore_from_backup(
                &paths.db_path,
                &file,
                &recovery::broken_stamp_now(),
            )
            .await
            {
                // 排他を取ってから予約・退避・適用まで済ませ、適用を確かめてある
                // （適用されなければ元に戻して Err）。
                Ok(recovery::Prepared { lock, set_aside }) => {
                    let mut note = RecoveryNote::new(RecoveryChoice::RestoreBackup)
                        .with("backupFileName", file);
                    if let Some(name) = set_aside.broken_file_name() {
                        eprintln!("banto: 壊れた DB を {name} として残しました");
                        note = note.with("brokenFileName", name);
                    }
                    Prelude::Recovered { lock, note }
                }
                Err(err) => {
                    rerender_with_notice(&app, Some(err.to_string())).await;
                    return;
                }
            }
        }
    };

    match crate::launch_desktop(&app, &paths, prelude).await {
        Ok(()) => {
            *app.state::<RecoveryGate>().lock() = None;
            if let Some(window) = app.get_webview_window(RECOVERY_WINDOW_LABEL) {
                // メインの窓ができた後なので、閉じてもアプリは終わらない。
                let _ = window.destroy();
            }
        }
        Err(LaunchError::Recovery(err)) => {
            // 選んだ保存先を使えなかった（DB は無事）: 同じ窓で選び直す。
            rerender_with_notice(&app, Some(err.to_string())).await;
        }
        Err(err) => {
            eprintln!("banto: 対処の後の起動に失敗しました: {err}");
            let kind = match &err {
                // やり直しで既に起動中・旧形式に当たった: 「終了」だけ。
                LaunchError::Engine(EngineStartError::InstanceLock(_))
                | LaunchError::Engine(EngineStartError::Database(InitDbError::Legacy(_))) => {
                    FailureKind::Other
                }
                other => kind_of(other),
            };
            let choices = choices_for(kind);
            let detail = err.to_string();
            {
                let gate = app.state::<RecoveryGate>();
                let mut session = gate.lock();
                if let Some(session) = session.as_mut() {
                    session.kind = kind;
                    session.choices = choices.clone();
                    session.detail = detail.clone();
                    session.busy = false;
                }
            }
            let backups = list_backups_async(&choices, &paths).await;
            rerender(
                &app,
                &build_view(
                    kind,
                    &choices,
                    detail,
                    &paths,
                    backups,
                    Some("対処の後も起動できませんでした。下の理由を確かめて、もう一度選んでください。".to_string()),
                ),
            );
        }
    }
}

/// フォルダ選択ダイアログ（取り消しは `None`）。
async fn pick_folder(app: &AppHandle) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("時系列データの保存先のフォルダを選んでください");
    if let Some(window) = app.get_webview_window(RECOVERY_WINDOW_LABEL) {
        dialog = dialog.set_parent(&window);
    }
    dialog.pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.await.ok().flatten()?;
    match folder.into_path() {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("banto: 選んだフォルダのパスを読めませんでした: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> LaunchPaths {
        LaunchPaths {
            data_dir: PathBuf::from(r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer"),
            db_path: PathBuf::from(
                r"C:\Users\a\AppData\Roaming\dev.tyaro.chronogazer\chronogazer.sqlite3",
            ),
        }
    }

    /// 窓の選択肢は core の判断どおりで、確認の文があるのは確認をはさむ対処
    /// だけ（core の `needs_confirmation` と食い違わない）。既定に戻す確認は
    /// 「記録は元の場所に残り画面からは見えなくなる」を先に伝える。
    #[test]
    fn views_follow_the_core_plan_and_confirmation_rule() {
        for kind in [
            FailureKind::StoreDirUnreadable,
            FailureKind::StoreDirUnwritable,
            FailureKind::DatabaseCorrupt,
            FailureKind::DatabaseUnopenable,
            FailureKind::DatabaseFromNewerVersion,
            FailureKind::Other,
        ] {
            let choices = choices_for(kind);
            let view = build_view(kind, &choices, "理由".into(), &paths(), Ok(vec![]), None);
            assert_eq!(
                view.choices.iter().map(|c| c.id).collect::<Vec<_>>(),
                choices.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                "{kind:?}"
            );
            for (choice, view) in choices.iter().zip(&view.choices) {
                assert_eq!(
                    view.confirm.is_some(),
                    choice.needs_confirmation(),
                    "{kind:?} {choice:?}"
                );
                assert_eq!(view.confirm.is_some(), view.confirm_label.is_some());
            }
        }
        let reset = choice_view(
            RecoveryChoice::ResetStoreDir,
            r"C:\x\data",
            "chronogazer.sqlite3",
        );
        let confirm = reset.confirm.unwrap();
        assert!(confirm.contains("元の保存先に残ります"), "{confirm}");
        assert!(confirm.contains("画面からは見えなくなります"), "{confirm}");
        assert!(confirm.contains(r"C:\x\data"), "{confirm}");
        let recreate = choice_view(RecoveryChoice::RecreateDatabase, "", "chronogazer.sqlite3");
        assert!(recreate
            .confirm
            .unwrap()
            .contains("chronogazer.sqlite3.broken-日時"));
        let restore = choice_view(RecoveryChoice::RestoreBackup, "", "chronogazer.sqlite3");
        assert!(restore.confirm.unwrap().contains("{file}"));
    }

    /// 窓へ渡す JS は JSON として読み戻せる（パスに引用符や `</script>` が
    /// 入っても）。
    #[test]
    fn scripts_carry_the_view_as_json() {
        let mut paths = paths();
        paths.db_path = PathBuf::from(r#"C:\a "b"\</script>\chronogazer.sqlite3"#);
        let view = build_view(
            FailureKind::DatabaseCorrupt,
            &choices_for(FailureKind::DatabaseCorrupt),
            "file is not a database".into(),
            &paths,
            Err("読めない".into()),
            Some("名前を変えられませんでした".into()),
        );
        let json = view
            .initialization_script()
            .strip_prefix("window.__CHRONOGAZER_RECOVERY__ = ")
            .and_then(|rest| rest.strip_suffix(';'))
            .map(str::to_string)
            .expect("shape");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON");
        assert_eq!(parsed["path"], view.path);
        assert_eq!(parsed["choices"][0]["id"], "restoreBackup");
        assert_eq!(parsed["backupsError"], "読めない");
        assert_eq!(parsed["notice"], "名前を変えられませんでした");
        assert!(view
            .render_script()
            .starts_with("window.__chronogazerRecovery && window.__chronogazerRecovery.render({"));
    }

    /// 窓からの移動: ページ自体は読み込み、対処は名前で受け取り、知らない対処や
    /// 他の場所への移動は取り消す。
    #[test]
    fn navigation_is_limited_to_the_page_and_known_actions() {
        let url = |s: &str| tauri::Url::parse(s).unwrap();
        assert_eq!(
            navigation_decision(&url("http://tauri.localhost/startup-recovery.html")),
            Navigation::Page
        );
        assert_eq!(
            navigation_decision(&url("http://tauri.localhost/__recovery/recreateDatabase")),
            Navigation::Action {
                choice: RecoveryChoice::RecreateDatabase,
                file: None
            }
        );
        assert_eq!(
            navigation_decision(&url(
                "http://tauri.localhost/__recovery/restoreBackup?file=banto-20261012-010203.sqlite3"
            )),
            Navigation::Action {
                choice: RecoveryChoice::RestoreBackup,
                file: Some("banto-20261012-010203.sqlite3".to_string())
            }
        );
        assert_eq!(
            navigation_decision(&url("http://tauri.localhost/__recovery/deleteEverything")),
            Navigation::Deny
        );
        assert_eq!(
            navigation_decision(&url("https://example.com/")),
            Navigation::Deny
        );
        assert_eq!(
            navigation_decision(&url("http://tauri.localhost/index.html")),
            Navigation::Deny
        );
    }

    /// ページは Rust が渡す名前（初期化の変数・描き直しの関数・対処のパス）を
    /// 使っている。
    #[test]
    fn page_uses_the_agreed_names() {
        let page = include_str!("../../static/startup-recovery.html");
        assert!(page.contains("__CHRONOGAZER_RECOVERY__"));
        assert!(page.contains("__chronogazerRecovery"));
        assert!(page.contains(ACTION_PATH_PREFIX));
        assert!(page.contains("{file}"));
        assert!(page.contains("textContent"));
        assert!(!page.contains("innerHTML"), "文言は HTML として入れない");
    }
}
