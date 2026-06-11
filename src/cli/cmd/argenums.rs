//! Shared `clap::ValueEnum` wrappers for the closed-enum CLI flags.
//!
//! The SDK/host layer enums (`RenderMode`, `BrowserChoice`, `DisplayMode`, …)
//! must not depend on `clap` — the `sdk`-only build has no clap. So the CLI
//! layer keeps thin `ValueEnum` mirrors here and converts into the domain types
//! at dispatch. Variants render in kebab-case by default, which gives each flag
//! a validated value list, shell completion, and a uniform "invalid value …
//! [possible values: …]" error for free.

use clap::ValueEnum;

use crate::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, Takeover, TakeoverProviderKind,
};
use crate::sdk::fetch::RenderMode;

/// `--render` strategy.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderArg {
    /// HTTP fast path, no browser.
    None,
    /// HTTP first, escalate to the browser on failure.
    #[default]
    Auto,
    /// Browser only.
    Always,
}

impl From<RenderArg> for RenderMode {
    fn from(v: RenderArg) -> Self {
        match v {
            RenderArg::None => RenderMode::None,
            RenderArg::Auto => RenderMode::Auto,
            RenderArg::Always => RenderMode::Always,
        }
    }
}

/// `--browser` backend. Kebab-case canonical names; the legacy underscore
/// spellings stay accepted as hidden aliases.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserArg {
    #[default]
    Auto,
    Chromium,
    Chrome,
    #[value(name = "chrome-headless-shell", alias = "chrome_shell")]
    ChromeHeadlessShell,
    #[value(name = "fingerprint-chromium", alias = "fingerprint_chromium")]
    FingerprintChromium,
    Edge,
    Brave,
    Lightpanda,
    Camoufox,
}

impl From<BrowserArg> for BrowserChoice {
    fn from(v: BrowserArg) -> Self {
        match v {
            BrowserArg::Auto => BrowserChoice::Auto,
            BrowserArg::Chromium => BrowserChoice::Chromium,
            BrowserArg::Chrome => BrowserChoice::Chrome,
            BrowserArg::ChromeHeadlessShell => BrowserChoice::ChromeShell,
            BrowserArg::FingerprintChromium => BrowserChoice::FingerprintChromium,
            BrowserArg::Edge => BrowserChoice::Edge,
            BrowserArg::Brave => BrowserChoice::Brave,
            BrowserArg::Lightpanda => BrowserChoice::Lightpanda,
            BrowserArg::Camoufox => BrowserChoice::Camoufox,
        }
    }
}

/// `--display` mode for the host.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayArg {
    Headless,
    Headful,
}

impl From<DisplayArg> for DisplayMode {
    fn from(v: DisplayArg) -> Self {
        match v {
            DisplayArg::Headless => DisplayMode::Headless,
            DisplayArg::Headful => DisplayMode::Headful,
        }
    }
}

/// `--health-public` mode.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthPublicArg {
    Off,
    Minimal,
}

impl From<HealthPublicArg> for HealthPublic {
    fn from(v: HealthPublicArg) -> Self {
        match v {
            HealthPublicArg::Off => HealthPublic::Off,
            HealthPublicArg::Minimal => HealthPublic::Minimal,
        }
    }
}

/// `--takeover-provider` selector: `off` (or `none`) disables the takeover
/// surface, a provider name selects it.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeoverProviderArg {
    #[value(alias = "none")]
    Off,
    Kasmvnc,
}

impl TakeoverProviderArg {
    /// The provider name to serve, or `None` when takeover is off.
    pub fn provider_name(self) -> Option<&'static str> {
        match self {
            TakeoverProviderArg::Off => None,
            TakeoverProviderArg::Kasmvnc => Some("kasmvnc"),
        }
    }
}

impl From<TakeoverProviderArg> for Takeover {
    fn from(v: TakeoverProviderArg) -> Self {
        match v {
            TakeoverProviderArg::Off => Takeover::Off,
            TakeoverProviderArg::Kasmvnc => Takeover::On {
                provider: TakeoverProviderKind::KasmVnc,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wrapper variant parses from its canonical kebab-case name (plus the
    /// underscore aliases for the browser backends).
    #[test]
    fn value_enum_names_parse() {
        for name in ["none", "auto", "always"] {
            assert!(RenderArg::from_str(name, true).is_ok(), "{name}");
        }
        for name in [
            "auto",
            "chromium",
            "chrome",
            "chrome-headless-shell",
            "chrome_shell",
            "fingerprint-chromium",
            "fingerprint_chromium",
            "edge",
            "brave",
            "lightpanda",
            "camoufox",
        ] {
            assert!(BrowserArg::from_str(name, true).is_ok(), "{name}");
        }
        assert!(TakeoverProviderArg::from_str("off", true).is_ok());
        assert!(TakeoverProviderArg::from_str("none", true).is_ok());
        assert!(TakeoverProviderArg::from_str("kasmvnc", true).is_ok());
        assert!(BrowserArg::from_str("rocket", true).is_err());
    }

    #[test]
    fn takeover_provider_maps_to_domain() {
        assert_eq!(Takeover::from(TakeoverProviderArg::Off), Takeover::Off);
        assert_eq!(TakeoverProviderArg::Off.provider_name(), None);
        assert_eq!(
            TakeoverProviderArg::Kasmvnc.provider_name(),
            Some("kasmvnc")
        );
        assert_eq!(
            Takeover::from(TakeoverProviderArg::Kasmvnc),
            Takeover::On {
                provider: TakeoverProviderKind::KasmVnc
            }
        );
    }

    #[test]
    fn browser_maps_canonical_and_alias() {
        assert_eq!(
            BrowserChoice::from(BrowserArg::ChromeHeadlessShell),
            BrowserChoice::ChromeShell
        );
        assert_eq!(
            BrowserChoice::from(BrowserArg::from_str("chrome_shell", true).unwrap()),
            BrowserChoice::ChromeShell
        );
    }

    #[test]
    fn toggles_map_to_bool_and_modes() {
        assert_eq!(RenderMode::from(RenderArg::None), RenderMode::None);
        assert_eq!(DisplayMode::from(DisplayArg::Headful), DisplayMode::Headful);
        assert_eq!(
            HealthPublic::from(HealthPublicArg::Minimal),
            HealthPublic::Minimal
        );
    }
}
