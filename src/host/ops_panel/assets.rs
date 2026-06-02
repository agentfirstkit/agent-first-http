//! Static asset bundle for the ops panel. Embedded at compile time via
//! `include_bytes!` so the resulting binary is self-contained.

pub const INDEX_HTML: &[u8] = include_bytes!("../../../assets/screencast-panel/index.html");
pub const APP_JS: &[u8] = include_bytes!("../../../assets/screencast-panel/app.js");
pub const APP_CSS: &[u8] = include_bytes!("../../../assets/screencast-panel/app.css");
