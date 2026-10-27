//! Optional, read-only device range diagnostics on an already running broker
//! connection. This never opens another PLC socket or changes the saved tags.

use super::{
    require_editor, ApiError, HeaderMap, IntoResponse, Json, Path, Response, State, StatusCode,
    TagRegistryState,
};
use banto_driver_api::{CountBound, DataType, TagKind, TagSpec, TagVerdict};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RangeDiagnosticRequest {
    /// Empty means all saved PLC tags for this connection, up to 256.
    #[serde(default)]
    tag_ids: Vec<i64>,
}

fn response(
    revision: u64,
    driver_version: &str,
    tag_ids: &[i64],
    report: &banto_driver_api::DiagnosticReport,
) -> Value {
    let model = report.model.as_ref().map(|model| {
        json!({
            "name": model.name, "code": model.code, "family": model.family,
            "rawName": model.raw_name,
        })
    });
    let counts: Vec<_> = report
        .counts
        .iter()
        .map(|count| {
            let (bound, points, reason) = match &count.bound {
                CountBound::Exact(n) => ("exact", Some(*n), None),
                CountBound::AtLeast(n) => ("atLeast", Some(*n), None),
                CountBound::Unknown(reason) => ("unknown", None, Some(reason.as_str())),
            };
            json!({
                "area": count.area, "scope": count.scope, "unit": count.unit,
                "bound": bound, "points": points, "reason": reason, "source": count.source,
            })
        })
        .collect();
    let tags: Vec<_> = report
        .tags
        .iter()
        .zip(tag_ids)
        .map(|(tag, id)| {
            let (verdict, reason) = match &tag.verdict {
                TagVerdict::Inside(r) => ("inside", r),
                TagVerdict::Outside(r) => ("outside", r),
                TagVerdict::Unknown(r) => ("unknown", r),
                TagVerdict::Invalid(r) => ("invalid", r),
                TagVerdict::Unsupported(r) => ("unsupported", r),
            };
            json!({"tagId": id, "address": tag.address, "verdict": verdict, "reason": reason})
        })
        .collect();
    json!({
        "status": "current", "catalogRevision": revision,
        "driverVersion": driver_version, "model": model,
        "modelReason": report.model_reason, "counts": counts, "tags": tags,
    })
}

fn unavailable(reason: &str) -> Value {
    json!({"status": "unavailable", "reason": reason})
}

pub(super) async fn range_diagnostic_get(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "GET",
        "/api/plc-connections/{id}/diagnostics",
    )
    .await?;
    Ok(Json(
        match state.manager.current_range_diagnostic(connection_id) {
            Some((revision, version, ids, report)) => response(revision, &version, &ids, &report),
            None => unavailable("no current diagnostic for this connection"),
        },
    ))
}

pub(super) async fn range_diagnostic_post(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(connection_id): Path<i64>,
    Json(input): Json<RangeDiagnosticRequest>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/{id}/diagnostics",
    )
    .await?;
    let manager = &state.manager;
    let (revision, snapshot) = manager.diagnostic_registry_snapshot();
    let Some(connection) = snapshot.connections.iter().find(|c| c.id == connection_id) else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(unavailable("connection not in applied catalog")),
        )
            .into_response());
    };
    let Some(driver) = manager.registry().get(&connection.protocol) else {
        return Ok(Json(unavailable("driver unavailable")).into_response());
    };
    let version = driver.descriptor().version.clone();
    let group_ids: HashSet<_> = snapshot
        .groups
        .iter()
        .filter(|g| g.plc_connection_id == connection_id)
        .map(|g| g.id)
        .collect();
    let requested: HashSet<_> = input.tag_ids.iter().copied().collect();
    if input.tag_ids.len() > 256 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(unavailable("select at most 256 saved PLC tags")),
        )
            .into_response());
    }
    if requested.len() != input.tag_ids.len() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(unavailable("duplicate tag ids")),
        )
            .into_response());
    }
    let mut tags: Vec<_> = snapshot
        .tags
        .iter()
        .filter(|tag| {
            group_ids.contains(&tag.collection_group_id)
                && tag.tag_kind == banto_tags::PLC_TAG_KIND
                && (requested.is_empty() || requested.contains(&tag.id))
        })
        .collect();
    tags.sort_by_key(|tag| tag.id);
    if !requested.is_empty() && tags.len() != requested.len() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(unavailable("select saved PLC tags on this connection")),
        )
            .into_response());
    }
    let mut specs = Vec::with_capacity(tags.len());
    let mut tag_ids = Vec::with_capacity(tags.len());
    for tag in tags {
        let Some(kind) =
            banto_tags::tag_kind(&tag.data_type, tag.string_length, &tag.string_encoding)
        else {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(unavailable("saved tag type is invalid")),
            )
                .into_response());
        };
        let Ok(address) = driver.base().parse_address(&tag.address, &kind) else {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(unavailable("saved tag address is invalid")),
            )
                .into_response());
        };
        tag_ids.push(tag.id);
        specs.push(TagSpec::new(address, kind));
    }
    let Some(handle) = manager.sessions().handle_for(connection_id) else {
        return Ok(Json(unavailable("no active broker session")).into_response());
    };
    let Some(epoch) = handle.session_epoch() else {
        return Ok(Json(unavailable("broker session is disconnected")).into_response());
    };
    // The guest keeps discovered bit counts on its connection resource.
    // Preflight every bit chunk when the request needs splitting. A first
    // chunk of X bits may not trigger M/Y/L/B SD acquisition, whereas a later
    // chunk does. Discard preflight verdicts and publish only the subsequent
    // complete pass with one consistent set of evidence, in saved-tag order.
    let selected: Vec<_> = tag_ids.iter().copied().zip(specs.iter().cloned()).collect();
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        handle
            .refresh_diagnostic()
            .await
            .map_err(|_| "diagnostic refresh failed")?;
        if !epoch.is_current(&handle) {
            return Err("connection changed during diagnostic refresh");
        }
        if selected.len() > 256 {
            let bit_specs: Vec<_> = selected
                .iter()
                .filter(|(_, spec)| matches!(spec.kind, TagKind::Numeric(DataType::Bit)))
                .map(|(_, spec)| spec.clone())
                .collect();
            for chunk in bit_specs.chunks(256) {
                if handle
                    .diagnose(chunk.to_vec())
                    .await
                    .map_err(|_| "diagnostic bit discovery failed")?
                    .is_none()
                {
                    return Ok(None);
                }
                if !epoch.is_current(&handle) {
                    return Err("connection changed during bit discovery");
                }
            }
        }
        let chunks: Vec<&[(i64, TagSpec)]> = if selected.is_empty() {
            vec![&[]]
        } else {
            selected.chunks(256).collect()
        };
        let mut complete: Option<banto_driver_api::DiagnosticReport> = None;
        for chunk in chunks {
            let request: Vec<_> = chunk.iter().map(|(_, spec)| spec.clone()).collect();
            let Some(part) = handle
                .diagnose(request)
                .await
                .map_err(|_| "diagnostic failed")?
            else {
                return Ok(None);
            };
            if !epoch.is_current(&handle) {
                return Err("connection changed during diagnostic");
            }
            if let Some(report) = &mut complete {
                if report.model != part.model
                    || report.model_reason != part.model_reason
                    || report.counts != part.counts
                {
                    return Err("device evidence changed between diagnostic chunks");
                }
                report.tags.extend(part.tags);
            } else {
                complete = Some(part);
            }
        }
        Ok(complete)
    })
    .await;
    let report = match result {
        Ok(Ok(Some(report))) => report,
        Ok(Ok(None)) => {
            return Ok(Json(
                json!({"status":"unsupported", "reason":"driver has no diagnostic export"}),
            )
            .into_response())
        }
        Ok(Err(reason)) => return Ok(Json(unavailable(reason)).into_response()),
        Err(_) => {
            return Ok(
                Json(unavailable("diagnostic timed out; result was discarded")).into_response(),
            )
        }
    };
    if !epoch.is_current(&handle) {
        return Ok(Json(unavailable("connection changed; result discarded")).into_response());
    }
    let accepted = handle
        .accept_diagnostic(epoch.clone(), specs, report.clone())
        .await
        .unwrap_or(false);
    if !accepted {
        return Ok(Json(unavailable("connection changed; result discarded")).into_response());
    }
    if !manager.publish_range_diagnostic(
        connection_id,
        revision,
        &version,
        epoch.clone(),
        tag_ids.clone(),
        report.clone(),
    ) {
        let _ = handle.discard_diagnostic(epoch).await;
        return Ok(Json(unavailable(
            "connection, settings, driver or catalog changed; result discarded",
        ))
        .into_response());
    }
    // Publication is synchronous; enqueue finalization before the next await
    // so a later diagnostic cannot treat this accepted result as tentative.
    let _ = handle.finalize_diagnostic(epoch).await;
    Ok(Json(response(revision, &version, &tag_ids, &report)).into_response())
}
