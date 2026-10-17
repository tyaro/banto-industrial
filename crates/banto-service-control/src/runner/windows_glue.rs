//! Windows の糊: ディスパッチャ・制御ハンドラの登録・`SetServiceStatus` を
//! [`super::run_program_with`] に差し込むだけ（判断は持たない）。

use std::ffi::OsString;
use std::sync::OnceLock;

use windows_service::service::{ServiceControl, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use super::handoff::Handoff;
use super::{
    run_program_with_args, ControlHandler, ControlReply, DispatchError, ErrorLog, ServiceExit,
    ServiceProgram,
};
use crate::shutdown::{service_status_from_plan, ScmControlKind, ServiceStatusPlan};

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

static HANDOFF: Handoff = Handoff::new();

/// 動いている本体のエラーの書き先と製品名（制御ハンドラの登録の直前に置く）。
/// 入口が 2 回呼ばれたときなど、本体の外で起きたことをサービスのログへ
/// 残すため（標準エラー出力はサービスとしては誰にも見えない）。
static ACTIVE_LOG: OnceLock<(String, ErrorLog)> = OnceLock::new();

/// 本体の外で起きたことを 1 行書く: 標準エラー出力と、本体が動いていれば
/// その`log_error`（`"{label}: ..."`）。
fn report_outside_body(message: &str) {
    match ACTIVE_LOG.get() {
        Some((label, log)) => {
            let line = format!("{label}: {message}");
            eprintln!("{line}");
            log(&line);
        }
        None => eprintln!("{message}"),
    }
}

define_windows_service!(ffi_service_main, runner_service_main);

pub(super) fn run_service<P: ServiceProgram>(program: P) -> Result<(), DispatchError> {
    let name = program.service_name().to_owned();
    HANDOFF.claim(Box::new(move |arguments| run_program(program, arguments)))?;
    service_dispatcher::start(&name, ffi_service_main).map_err(|err| {
        HANDOFF.abandon();
        DispatchError::NotUnderScm(err.to_string())
    })
}

/// FFI の入口から呼ばれる。ここで panic すると FFI 境界を越えて
/// アンワインドしうるので、本体全体を `catch_unwind` で包む（本体の
/// `run` の panic は [`run_program_with`] が先に受けて Stopped を報告する）。
fn runner_service_main(arguments: Vec<OsString>) {
    let Some(body) = HANDOFF.take() else {
        report_outside_body("サービスの本体が見つかりません（入口が 2 回呼ばれました）");
        return;
    };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(arguments))).is_err() {
        report_outside_body("サービスのランナーが予期せず panic しました");
    }
}

/// `ServiceControl` を共有の種別へ写す。
pub(super) fn scm_control_kind(control: &ServiceControl) -> ScmControlKind {
    match control {
        ServiceControl::Interrogate => ScmControlKind::Interrogate,
        ServiceControl::Stop => ScmControlKind::Stop,
        ServiceControl::Preshutdown => ScmControlKind::Preshutdown,
        ServiceControl::Shutdown => ScmControlKind::Shutdown,
        _ => ScmControlKind::Other,
    }
}

fn run_program<P: ServiceProgram>(program: P, arguments: Vec<OsString>) {
    let name = program.service_name().to_owned();
    let label = program.label().to_owned();
    let _ = run_program_with_args(
        program,
        arguments,
        move |handler: ControlHandler, log: ErrorLog| {
            let _ = ACTIVE_LOG.set((label.clone(), log.clone()));
            let status_handle = service_control_handler::register(&name, move |control| {
                match handler(scm_control_kind(&control)) {
                    ControlReply::NoError => ServiceControlHandlerResult::NoError,
                    ControlReply::NotImplemented => ServiceControlHandlerResult::NotImplemented,
                }
            })
            .map_err(|err| err.to_string())?;
            Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
                let status =
                    service_status_from_plan(plan, SERVICE_TYPE, exit.to_service_exit_code());
                if let Err(err) = status_handle.set_service_status(status) {
                    // 失敗はログだけ（停止処理自体は続ける）。
                    log(&format!("{label}: サービス状態の報告に失敗しました: {err}"));
                }
            })
        },
    );
}
