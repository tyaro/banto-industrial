//! Authenticated driver catalog route. The catalog shape is shared with ChronoGazer.
use super::{Json, State, TagRegistryState};

pub(crate) use banto_tags::driver_catalog::driver_list;

pub(super) async fn drivers_list(
    State(state): State<TagRegistryState>,
) -> Json<Vec<banto_tags::driver_catalog::DriverListItem>> {
    Json(driver_list(state.manager.registry()))
}
