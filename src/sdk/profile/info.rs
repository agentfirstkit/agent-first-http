//! SDK-side view of `GET /profile`. Re-exports [`ProfileSnapshot`] from
//! `shared` so callers outside this module use the canonical type.

pub use crate::shared::profile_snapshot::ProfileSnapshot as ProfileInfo;
