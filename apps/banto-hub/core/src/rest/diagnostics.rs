//! Optional, read-only device range diagnostics on an already running broker
//! connection. This never opens another PLC socket or changes the saved tags.

use super::{
    require_editor, ApiError, HeaderMap, IntoResponse, Json, Path, Response, State, StatusCode,
    TagRegistryState,
};
use banto_driver_api::{CountBound, TagSpec, TagVerdict};
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
    if tags.len() > 256 || (!requested.is_empty() && tags.len() != requested.len()) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(unavailable(
                "select 1..=256 saved PLC tags on this connection",
            )),
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
    let result = tokio::time::timeout(Duration::from_secs(12), handle.diagnose(specs)).await;
    let report = match result {
        Ok(Ok(Some(report))) => report,
        Ok(Ok(None)) => {
            return Ok(Json(
                json!({"status":"unsupported", "reason":"driver has no diagnostic export"}),
            )
            .into_response())
        }
        Ok(Err(_)) => {
            return Ok(Json(unavailable(
                "diagnostic failed; retry after checking the connection",
            ))
            .into_response())
        }
        Err(_) => {
            return Ok(
                Json(unavailable("diagnostic timed out; result was discarded")).into_response(),
            )
        }
    };
    if !manager.publish_range_diagnostic(
        connection_id,
        revision,
        &version,
        epoch,
        tag_ids.clone(),
        report.clone(),
    ) {
        return Ok(Json(unavailable(
            "connection, settings, driver or catalog changed; result discarded",
        ))
        .into_response());
    }
    Ok(Json(response(revision, &version, &tag_ids, &report)).into_response())
}
