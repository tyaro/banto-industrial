//! Hub の現在値を読み取り専用 OPC UA サーバーへ公開する。
//! `HubRuntime` が所有し、管理 API の設定変更と終了処理で同じインスタンスを使う。

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_collect::Quality;
use banto_opcua_server::model::{DataType, Quality as UaQuality, SampleTimestamps};
use banto_opcua_server::{OpcUaServer, RunningServer, ServerConfig, TagReading};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::hub::{read_current_view, CollectorManager, CurrentView, TagEntry};
use crate::hub_log::log_err_line;
use crate::settings::OpcUaSettings;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq)]
struct ReadingKey {
    value: Option<f64>,
    quality: Quality,
    source_time_ms: Option<i64>,
    quality_since_ms: Option<i64>,
}

impl From<CurrentView> for ReadingKey {
    fn from(view: CurrentView) -> Self {
        Self {
            value: view.value,
            quality: view.quality,
            source_time_ms: view.source_time_ms,
            quality_since_ms: view.quality_since_ms,
        }
    }
}

#[derive(Debug, Clone)]
struct PublishedTag {
    ids: (i64, i64, i64),
    data_type: DataType,
    reading: ReadingKey,
    connection: String,
    group: String,
    name: String,
}

struct Inner {
    server: Option<RunningServer>,
    tags: HashMap<String, PublishedTag>,
    settings: Option<OpcUaSettings>,
    worker: Option<JoinHandle<()>>,
    last_errors: HashMap<String, String>,
    stopping: bool,
}

pub struct OpcUaPublisher {
    manager: Arc<CollectorManager>,
    inner: Mutex<Inner>,
}

impl OpcUaPublisher {
    pub fn new(manager: Arc<CollectorManager>) -> Arc<Self> {
        Arc::new(Self {
            manager,
            inner: Mutex::new(Inner {
                server: None,
                tags: HashMap::new(),
                settings: None,
                worker: None,
                last_errors: HashMap::new(),
                stopping: false,
            }),
        })
    }

    /// 設定を直ちに適用する。起動・bind の失敗はログへ残し、Hub 本体は継続する。
    pub async fn apply(self: &Arc<Self>, settings: &OpcUaSettings) {
        self.apply_inner(settings, false).await;
    }

    /// REST listener 起動後に設定を復元する。先に管理 API から設定変更が
    /// 入った場合、その新しい設定を古い起動時スナップショットで巻き戻さない。
    pub async fn initialize(self: &Arc<Self>, settings: &OpcUaSettings) {
        self.apply_inner(settings, true).await;
    }

    async fn apply_inner(self: &Arc<Self>, settings: &OpcUaSettings, only_if_uninitialized: bool) {
        let mut inner = self.inner.lock().await;
        if inner.stopping {
            return;
        }
        if only_if_uninitialized && inner.settings.is_some() {
            return;
        }
        if inner.settings.as_ref() == Some(settings)
            && (!settings.enabled || inner.server.is_some())
        {
            return;
        }
        if let Some(worker) = inner.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        if let Some(server) = inner.server.take() {
            if let Err(err) = server.shutdown().await {
                log_err_line(&format!("banto-hub: OPC UA の停止に失敗しました: {err}"));
            }
        }
        inner.tags.clear();
        inner.last_errors.clear();
        inner.settings = Some(settings.clone());
        if !settings.enabled {
            return;
        }
        let validation = crate::rest::validate_opcua_settings(settings);
        if !validation.is_empty() {
            log_err_line(&format!(
                "banto-hub: OPC UA 設定が不正なため起動しません: {validation:?}"
            ));
            return;
        }

        let bind: IpAddr = match settings.bind.parse() {
            Ok(bind) => bind,
            Err(err) => {
                log_err_line(&format!(
                    "banto-hub: OPC UA の bind アドレスが不正です: {err}"
                ));
                return;
            }
        };
        let mut config = ServerConfig::default()
            .with_bind(bind)
            .with_port(settings.port)
            .with_endpoint_path(&settings.endpoint_path)
            .with_namespace_uri(&settings.namespace_uri)
            .with_sample_tags(false);
        if let Some(host) = &settings.advertised_host {
            config = config.with_advertised_host(host);
        }
        let server = match OpcUaServer::start(config).await {
            Ok(server) => server,
            Err(err) => {
                log_err_line(&format!("banto-hub: OPC UA の起動に失敗しました: {err}"));
                return;
            }
        };
        log_err_line(&format!(
            "banto-hub: OPC UA endpoint {} (SecurityPolicy None / Anonymous; LAN 公開は機密保護なし)",
            server.endpoint()
        ));
        inner.server = Some(server);
        self.reconcile(&mut inner);
        let publisher = self.clone();
        inner.worker = Some(tokio::spawn(async move {
            let mut timer = tokio::time::interval(POLL_INTERVAL);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            loop {
                timer.tick().await;
                let mut inner = publisher.inner.lock().await;
                publisher.reconcile(&mut inner);
            }
        }));
    }

    /// 値取得は共通の `read_current_view` を使う。ServerTimestamp だけが
    /// 進む場合は更新を送らず、不要な DataChange 通知を抑える。
    fn reconcile(&self, inner: &mut Inner) {
        let Some(server) = inner.server.as_ref() else {
            return;
        };
        let map = self.manager.tag_map();
        let collect = self.manager.current_values();
        let store = self.manager.server_store();
        let now_ms = self.manager.clock().now_ms();
        let mut desired = HashMap::new();
        for entry in map.iter() {
            if let Some(data_type) = data_type(entry) {
                desired.insert(entry.external_name.clone(), (entry, data_type));
            }
        }
        inner
            .last_errors
            .retain(|name, _| desired.contains_key(name));

        let mut blocked = HashSet::new();
        inner.tags.retain(|name, old| {
            let keep = desired.get(name).is_some_and(|(entry, ty)| {
                entry.ids == old.ids
                    && *ty == old.data_type
                    && entry.connection == old.connection
                    && entry.group == old.group
                    && entry.name == old.name
            });
            if !keep {
                if let Err(err) = server.remove_tag(&old.connection, &old.group, &old.name) {
                    log_err_line(&format!("banto-hub: OPC UA タグ削除 {name}: {err}"));
                    blocked.insert(name.clone());
                    return true;
                }
            }
            keep
        });
        for (name, (entry, data_type)) in desired {
            if blocked.contains(&name) {
                continue;
            }
            let (connection, group, tag) = (
                entry.connection.as_str(),
                entry.group.as_str(),
                entry.name.as_str(),
            );
            let view = read_current_view(entry, collect.as_ref(), &store, now_ms);
            let reading_key = ReadingKey::from(view);
            if inner
                .tags
                .get(&name)
                .is_some_and(|old| old.reading == reading_key)
            {
                continue;
            }
            let reading = tag_reading(data_type, view);
            let result = if inner.tags.contains_key(&name) {
                server.update_tag_reading(connection, group, tag, reading)
            } else {
                server.add_tag_reading(connection, group, tag, reading)
            };
            match result {
                Ok(()) => {
                    inner.last_errors.remove(&name);
                    inner.tags.insert(
                        name,
                        PublishedTag {
                            ids: entry.ids,
                            data_type,
                            reading: reading_key,
                            connection: entry.connection.clone(),
                            group: entry.group.clone(),
                            name: entry.name.clone(),
                        },
                    );
                }
                Err(err) => {
                    let error = err.to_string();
                    if inner.last_errors.get(&name) != Some(&error) {
                        log_err_line(&format!("banto-hub: OPC UA タグ同期 {name}: {error}"));
                        inner.last_errors.insert(name, error);
                    }
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        inner.stopping = true;
        if let Some(worker) = inner.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        if let Some(server) = inner.server.take() {
            if let Err(err) = server.shutdown().await {
                log_err_line(&format!("banto-hub: OPC UA の停止に失敗しました: {err}"));
            }
        }
        inner.tags.clear();
    }

    #[cfg(test)]
    pub(crate) async fn endpoint_and_namespace(&self) -> Option<(String, u16)> {
        self.inner
            .lock()
            .await
            .server
            .as_ref()
            .map(|server| (server.endpoint().to_owned(), server.namespace_index()))
    }
}

fn data_type(entry: &TagEntry) -> Option<DataType> {
    match entry.data_type.as_str() {
        "string" => None,
        "bit" => Some(DataType::Boolean),
        _ => Some(DataType::Double),
    }
}

fn system_time(ms: i64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(u64::try_from(ms).ok()?))
}

fn tag_reading(data_type: DataType, view: CurrentView) -> TagReading {
    let quality = match view.quality {
        Quality::Good => UaQuality::Good,
        Quality::Bad => UaQuality::Bad,
        Quality::Stale => UaQuality::Stale,
    };
    let timestamps = SampleTimestamps::for_quality(
        quality,
        view.source_time_ms.and_then(system_time),
        view.quality_since_ms.and_then(system_time),
        SystemTime::now(),
    );
    TagReading::new(data_type, view.value, quality, timestamps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_missing_source_stays_unknown() {
        let reading = tag_reading(
            DataType::Double,
            CurrentView {
                value: None,
                quality: Quality::Bad,
                timestamp_ms: 100,
                source_time_ms: None,
                quality_since_ms: None,
            },
        );
        assert_eq!(reading.timestamps.source, None);
    }

    #[test]
    fn stale_uses_quality_transition_not_old_good_sample() {
        let reading = tag_reading(
            DataType::Boolean,
            CurrentView {
                value: Some(1.0),
                quality: Quality::Stale,
                timestamp_ms: 200,
                source_time_ms: Some(100),
                quality_since_ms: Some(175),
            },
        );
        assert_eq!(reading.timestamps.source, system_time(175));
        assert_eq!(reading.quality, UaQuality::Stale);
        assert_eq!(reading.value, Some(1.0));
    }
}
