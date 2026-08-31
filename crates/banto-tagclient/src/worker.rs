//! One connection-generation worker and bounded latest-state publication.
//!
//! The worker is crate-private and is intentionally not spawned here. S3 will
//! provide the public owner for task lifetime, reconnect, rebinding, and
//! shutdown.

use tokio::sync::watch;

use crate::{
    binding::{resolve_bindings, BindingRequest},
    error::{Error, ErrorKind, Result},
    rest::RestClient,
    stream_core::{AcceptedWire, PublishGate},
    types::{TagClientConnectionState, TagClientState},
};

/// Run exactly one connection generation. No task is spawned and no retry is
/// attempted; any failure clears current and returns its stable error kind.
#[allow(
    dead_code,
    reason = "S2b-2b worker is owned and spawned by the S3 handle slice"
)]
pub(crate) async fn run_generation(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
) -> Result<()> {
    state_tx.send_replace(TagClientState::new(TagClientConnectionState::Connecting));
    let result = run_generation_inner(rest, requests, subscription_id, state_tx).await;
    if result.is_err() {
        state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
    }
    result
}

async fn run_generation_inner(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
) -> Result<()> {
    if requests.is_empty() {
        return Err(Error::new(ErrorKind::InvalidTagSelection));
    }
    let catalog = rest.fetch_catalog().await?;
    let resolution = resolve_bindings(requests, &catalog.tags)?;
    if !resolution.unresolved.is_empty() {
        return Err(Error::new(ErrorKind::BindingUnresolved));
    }
    let tags = resolution
        .resolved
        .iter()
        .map(|binding| binding.external_name.clone())
        .collect::<Vec<_>>();
    let mut gate = PublishGate::new(subscription_id, tags.clone())?;

    state_tx.send_replace(TagClientState::new(TagClientConnectionState::Handshaking));
    let mut connection = rest.connect_stream().await?;
    connection
        .subscribe_on_change(subscription_id, &tags)
        .await?;

    loop {
        let text = connection.receive_text().await?;
        if gate.accept_wire(&text)? == AcceptedWire::Data {
            break;
        }
    }

    let tag_refs = tags.iter().map(String::as_str).collect::<Vec<_>>();
    let mut values_future = Box::pin(rest.fetch_values(&tag_refs));
    let rest_snapshot = loop {
        tokio::select! {
            biased;
            text = connection.receive_text() => {
                gate.accept_wire(&text?)?;
            }
            snapshot = &mut values_future => break snapshot?,
        }
    };
    gate.record_rest_snapshot(rest_snapshot)?;
    publish_snapshot(&gate, &catalog, state_tx)?;

    loop {
        let text = connection.receive_text().await?;
        if gate.accept_wire(&text)? == AcceptedWire::Data {
            publish_snapshot(&gate, &catalog, state_tx)?;
        }
    }
}

fn publish_snapshot(
    gate: &PublishGate,
    catalog: &crate::types::CatalogSnapshot,
    state_tx: &watch::Sender<TagClientState>,
) -> Result<()> {
    let snapshot = gate.finalize(catalog)?;
    let mut state = TagClientState::new(TagClientConnectionState::Stopped);
    state.publish(snapshot);
    state_tx.send_replace(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{oneshot, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    use super::*;
    use crate::{
        endpoint::Endpoint,
        secret::SecretApiKey,
        types::{
            CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
            ValueSource,
        },
    };

    fn tag(id: StableTagId, name: &str) -> CatalogTag {
        CatalogTag {
            external_name: name.into(),
            tag_key: format!("key:{name}"),
            ids: id,
            connection: "connection".into(),
            group: "group".into(),
            name: name.into(),
            address: "address".into(),
            data_type: "f64".into(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: false,
            tag_kind: "tag".into(),
            expression: None,
            retain: false,
            simulation: false,
            configured_simulation: false,
            effective_simulation: false,
            value_source: ValueSource::Real,
        }
    }

    fn catalog(tags: Vec<CatalogTag>) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            tags,
        }
    }

    fn values(values: Vec<ValueEntry>) -> crate::types::ValuesSnapshot {
        crate::types::ValuesSnapshot {
            revision: 1,
            t: 10,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            values,
        }
    }

    fn value(tag: &str, number: f64, timestamp: i64) -> ValueEntry {
        ValueEntry {
            tag: tag.into(),
            v: Some(number),
            q: ValueQuality::Good,
            t: timestamp,
            value_source: ValueSource::Real,
        }
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    async fn write_json(stream: &mut tokio::net::TcpStream, body: String) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    fn client(address: String) -> RestClient {
        RestClient::new(
            Endpoint::new(address).unwrap(),
            SecretApiKey::new("test-token".into()).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn watch_keeps_only_latest_complete_state_and_non_live_clears_current() {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let mut latest = None;
        for number in 0..256 {
            let mut state = TagClientState::new(TagClientConnectionState::Stopped);
            state.publish(values(vec![value("tag", number as f64, number)]));
            sender.send_replace(state);
            latest = Some(number as f64);
        }
        assert_eq!(receiver.borrow().current().unwrap().values[0].v, latest);
        sender.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
        assert_eq!(receiver.borrow().current(), None);
    }

    #[tokio::test]
    async fn generation_orders_catalog_ws_subscribe_data_then_rest_and_publishes_atomically() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let order = Arc::new(Mutex::new(Vec::new()));
        let order_server = Arc::clone(&order);
        let (start_burst_tx, start_burst_rx) = oneshot::channel();
        let (burst_done_tx, burst_done_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let id_a = StableTagId::new(1, 1, 1);
            let id_b = StableTagId::new(1, 1, 2);
            let catalog_body =
                serde_json::to_string(&catalog(vec![tag(id_a, "alpha"), tag(id_b, "beta")]))
                    .unwrap();
            let values_body =
                serde_json::to_string(&values(vec![value("alpha", 1.0, 5), value("beta", 2.0, 5)]))
                    .unwrap();

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(request.starts_with("GET /api/v1/tags HTTP/1.1"));
            order_server.lock().unwrap().push("catalog");
            write_json(&mut stream, catalog_body).await;

            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                subscription,
                Message::Text(
                    r#"{"op":"subscribe","id":9,"tags":["alpha","beta"],"mode":"on_change"}"#
                        .into()
                )
            );
            order_server.lock().unwrap().push("subscribe");
            socket.send(Message::Text(r#"{"op":"data","id":9,"t":5,"values":[{"tag":"alpha","v":1,"q":"good","t":5}]}"#.into())).await.unwrap();

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(request.starts_with("GET /api/v1/values?tags=alpha%2Cbeta HTTP/1.1"));
            order_server.lock().unwrap().push("values");
            socket.send(Message::Text(r#"{"op":"data","id":9,"t":20,"values":[{"tag":"alpha","v":20,"q":"good","t":20}]}"#.into())).await.unwrap();
            write_json(&mut stream, values_body).await;
            let _ = start_burst_rx.await;
            for timestamp in 30..=125 {
                let body = format!(
                    r#"{{"op":"data","id":9,"t":{0},"values":[{{"tag":"alpha","v":{0},"q":"good","t":{0}}}]}}"#,
                    timestamp
                );
                socket.send(Message::Text(body.into())).await.unwrap();
            }
            socket.flush().await.unwrap();
            burst_done_tx.send(()).unwrap();
            let _ = release_rx.await;
        });

        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let mut progress_receiver = sender.subscribe();
        let requests = vec![
            BindingRequest {
                binding_key: "a".into(),
                stable_id: StableTagId::new(1, 1, 1),
            },
            BindingRequest {
                binding_key: "b".into(),
                stable_id: StableTagId::new(1, 1, 2),
            },
        ];
        let rest_client = client(address);
        let worker =
            tokio::spawn(async move { run_generation(&rest_client, &requests, 9, &sender).await });
        for expected in [
            TagClientConnectionState::Connecting,
            TagClientConnectionState::Handshaking,
        ] {
            tokio::time::timeout(Duration::from_secs(1), receiver.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(receiver.borrow().connection_state(), expected);
            assert_eq!(receiver.borrow().current(), None);
        }
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Live
        );
        let initial = receiver.borrow().clone();
        assert_eq!(initial.current().unwrap().values[0].v, Some(20.0));
        assert_eq!(initial.current().unwrap().values[1].v, Some(2.0));
        assert_eq!(initial.current().unwrap().t, 20);
        start_burst_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), burst_done_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                progress_receiver.changed().await.unwrap();
                if progress_receiver
                    .borrow_and_update()
                    .current()
                    .is_some_and(|snapshot| {
                        snapshot
                            .values
                            .iter()
                            .any(|entry| entry.tag == "alpha" && entry.v == Some(125.0))
                    })
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(receiver.has_changed().unwrap());
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let latest = receiver.borrow().clone();
        assert_eq!(latest.connection_state(), TagClientConnectionState::Live);
        assert_eq!(latest.current().unwrap().values[0].v, Some(125.0));
        assert_eq!(latest.current().unwrap().values[1].v, Some(2.0));
        assert_eq!(latest.current().unwrap().t, 125);
        release_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap();
        assert_eq!(result.unwrap().unwrap_err().kind(), ErrorKind::Transport);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(
            order.lock().unwrap().as_slice(),
            ["catalog", "subscribe", "values"]
        );
        let state = receiver.borrow().clone();
        assert_eq!(state.connection_state(), TagClientConnectionState::Stopped);
        assert_eq!(state.current(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_binding_requests_fail_before_websocket_and_keep_current_empty() {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(&client("http://127.0.0.1:1".into()), &[], 1, &sender),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidTagSelection);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
    }

    #[tokio::test]
    async fn unresolved_binding_fails_before_websocket_and_keeps_current_empty() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut stream).await;
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(Vec::new())).unwrap(),
            )
            .await;
        });
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let request = BindingRequest {
            binding_key: "missing".into(),
            stable_id: StableTagId::new(9, 9, 9),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(&client(address), &[request], 1, &sender),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::BindingUnresolved);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn config_changed_during_generation_fails_closed_and_clears_current() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let id = StableTagId::new(1, 1, 1);
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut stream).await;
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(vec![tag(id, "alpha")])).unwrap(),
            )
            .await;
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
        });
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let request = BindingRequest {
            binding_key: "alpha".into(),
            stable_id: StableTagId::new(1, 1, 1),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(&client(address), &[request], 1, &sender),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::RevisionMismatch);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        server.await.unwrap();
    }
}
