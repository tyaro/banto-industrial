//! サーバー自身の CPU/メモリ状態のサンプリング（T19 S3-b、
//! docs/banto-hub-t19-design.md §3.9、UX-46「サーバー状態の拡充」）。
//!
//! 状態画面（`GET /api/status`・`GET /api/v1/status`、`crate::rest`の
//! `compute_status`）は「サーバーの状態」に属する情報として、接続状態・
//! pending 変更・各種サービス状態に加えて **CPU 使用率・メモリ使用量** を
//! 表示する（UX-47 で撤去した「全タグ現在値」の代わりに残す情報 -
//! docs/banto-hub-t19-design.md §3.9 末尾）。
//!
//! ## 毎回 `System` を作り直さない理由
//!
//! [`sysinfo`] の CPU 使用率はスナップショット差分でしか出せない -
//! `System::new()` した直後に1回 `refresh_cpu_usage()` しただけでは、
//! 比較対象が無いため必ず `0.0` になる（sysinfo 自身のドキュメントが明記
//! する仕様）。呼び出しのたびに新しい `System` を作ると毎回この「初回
//! ゼロ」を踏み続け、CPU% が永久に `0.0` から動かない。
//!
//! そのため [`SystemInfoSampler`] は `System` を1個だけ構築して
//! [`Mutex`] 越しに使い回し、[`SystemInfoSampler::sample`] を呼ぶたびに
//! その同じ `System` を `refresh` する。前回の呼び出しからの経過時間が
//! CPU% の差分窓になる - 状態画面は約3秒間隔でポーリングする
//! （`(app)/status/+page.svelte`の`poll()`）ので、この3秒がそのまま自然な
//! サンプリング間隔になる。**プロセス起動直後の最初の呼び出しは、まだ
//! 前回のスナップショットが無いため CPU% が 0 になりうる** - これは
//! 許容する（sysinfo の仕様上避けられず、次の呼び出し以降は正しい値になる
//! ため実害が無い）。
//!
//! ## 取得する値（最小、UX-46 決定範囲）
//!
//! - プロセス自身の CPU 使用率（%）・メモリ（RSS バイト）
//! - ホストの総メモリ・使用メモリ（バイト）
//!
//! ディスク・ネットワーク・センサー等は取得しない（`Cargo.toml`の
//! `sysinfo`依存コメント参照 - `default-features = false` + `system`
//! feature のみ）。

use std::sync::Mutex;

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
use utoipa::ToSchema;

/// [`SystemInfoSampler::sample`]が返すスナップショット。単位はいずれも
/// バイト・パーセントの生値 - MB/GB 表記やゲージ表示への整形はフロント側
/// （`systemInfoFormat.ts`）の責務であり、ここでは行わない
/// （`crate::rest`の他の DTO と同じ「サーバー側は生値、整形は UI 側」の
/// 分担）。
///
/// あえて `#[serde(rename_all = "camelCase")]` を付けない -
/// `GET /api/v1/status`（`crate::rest::StatusResponse`）が snake_case の
/// まま埋め込むため（`crate::rest`冒頭のモジュール doc comment「`/api/v1/*`
/// 側は意図して snake_case のまま」参照）。管理系 `GET /api/status`
/// （camelCase）向けには `crate::rest::AdminSystemInfoEntry` を別に用意し
/// `From`で変換する - `ConnectionStatusEntry`/`AdminConnectionStatusEntry`
/// と同じ二重定義の流儀（`crate::rest`の該当コメント参照）。
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct SystemInfoSnapshot {
    /// このプロセス自身の CPU 使用率（%、論理コア1個 = 100%換算 -
    /// `sysinfo::Process::cpu_usage`の単位をそのまま転記する）。起動直後の
    /// 最初のサンプルは前回スナップショットが無いため`0.0`になりうる
    /// （このモジュール doc comment参照）。
    pub cpu_percent: f32,
    /// このプロセス自身の常駐メモリ（RSS、バイト）。
    pub process_memory_bytes: u64,
    /// ホスト全体の使用中メモリ（バイト、他プロセス分も含む）。
    pub host_memory_used_bytes: u64,
    /// ホスト全体の総メモリ（バイト）。
    pub host_memory_total_bytes: u64,
}

/// 共有 `System` を1個だけ保持し、呼び出しのたびに `refresh` して
/// スナップショットを返すサンプラー。`TagSpaceState`（`crate::rest`）が
/// `Arc`で保持し、`compute_status`から参照する - このモジュール doc
/// comment の「毎回作り直さない」を実現する実体。
#[derive(Debug)]
pub struct SystemInfoSampler {
    /// 自プロセスの PID。構築時に一度だけ取得し、以降の`sample`呼び出し
    /// ごとに使い回す（`sysinfo::get_current_pid`は失敗しうるが、実行中の
    /// 自分自身のPIDを引けないことは実質無い - 万一失敗した場合はプロセス
    /// 側の値を`0`として扱う。ホスト側の値は取得できるため状態画面全体は
    /// 引き続き機能する）。
    pid: Option<Pid>,
    system: Mutex<System>,
}

impl SystemInfoSampler {
    /// `System`を1回だけ構築する。この時点では CPU 使用率の比較対象になる
    /// 前回スナップショットが無いため、直後の最初の`sample`呼び出しは
    /// `cpu_percent = 0.0`になりうる（モジュール doc comment参照）。
    pub fn new() -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_memory(sysinfo::MemoryRefreshKind::everything())
                .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
        );
        Self {
            pid: sysinfo::get_current_pid().ok(),
            system: Mutex::new(system),
        }
    }

    /// 共有`System`を`refresh`してから現在値を読む。前回の`sample`呼び出し
    /// （または`new`時点）からの経過時間が CPU% の差分窓になる。
    pub fn sample(&self) -> SystemInfoSnapshot {
        let mut system = self
            .system
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        system.refresh_memory();
        if let Some(pid) = self.pid {
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
        }
        let (cpu_percent, process_memory_bytes) = self
            .pid
            .and_then(|pid| system.process(pid))
            .map(|process| (process.cpu_usage(), process.memory()))
            .unwrap_or((0.0, 0));
        SystemInfoSnapshot {
            cpu_percent,
            process_memory_bytes,
            host_memory_used_bytes: system.used_memory(),
            host_memory_total_bytes: system.total_memory(),
        }
    }
}

impl Default for SystemInfoSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 初回サンプルは CPU% が 0 になりうる（前回スナップショットが無い -
    /// モジュール doc comment参照）が、メモリ値はその制約を受けず即座に
    /// 意味のある値になる。ここでは「クラッシュしない・値がもっともらしい」
    /// ことだけを検証する（実際の CPU% は環境依存で断言できない）。
    #[test]
    fn first_sample_reports_plausible_memory_without_panicking() {
        let sampler = SystemInfoSampler::new();
        let snapshot = sampler.sample();
        assert!(snapshot.cpu_percent >= 0.0);
        assert!(snapshot.host_memory_total_bytes > 0);
        assert!(snapshot.host_memory_used_bytes > 0);
        assert!(snapshot.host_memory_used_bytes <= snapshot.host_memory_total_bytes);
    }

    /// 同じサンプラーへ2回`sample`しても panic せず、プロセス自身の
    /// メモリは0より大きい値を返し続ける（差分窓が2回目以降で働く経路の
    /// 素通り確認）。
    #[test]
    fn second_sample_still_reports_process_memory() {
        let sampler = SystemInfoSampler::new();
        let _first = sampler.sample();
        let second = sampler.sample();
        assert!(second.process_memory_bytes > 0);
    }
}
