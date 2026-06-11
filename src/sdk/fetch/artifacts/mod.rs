//! Per-artifact extractors for the seven fetch artifacts. `body` is used
//! by both the HTTP fast path and the browser path.

pub mod body;
pub mod collectors;
pub mod console;
pub mod content;
pub mod network;
pub mod network_bodies;
pub mod observation;
pub mod rendered_html;
pub mod screenshot;
pub mod storage;
pub mod text;
