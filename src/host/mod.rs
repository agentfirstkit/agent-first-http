//! Browser host: launches Chromium, holds the profile lock, runs the
//! listener that exposes CDP, `/health`, `/capabilities`, and the ops panel.
//! Behind the `host` feature so SDK-only consumers don't link chromiumoxide.

pub mod bootstrap;
pub mod browser;
pub mod display;
pub mod listener;
pub mod ops_panel;

pub use bootstrap::HostArgs;
