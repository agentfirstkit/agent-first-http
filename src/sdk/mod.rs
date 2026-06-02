//! Client-side SDK. Never imports `chromiumoxide`; speaks CDP over raw
//! WebSocket and HTTP over `reqwest`. Everything browser-dependent goes
//! through an `afhttp host` endpoint.

pub mod capabilities;
pub mod cdp;
pub mod client;
pub mod endpoint;
pub mod fetch;
pub mod health;
pub mod profile;

pub mod inline;

pub use client::Client;
pub use endpoint::Endpoint;
pub use fetch::{FetchBuilder, FetchCookie, FetchCookieSameSite, FetchResult, RenderMode, Wait};
