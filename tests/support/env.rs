//! Test-environment helpers — browser discovery for the integration suite.

use std::path::PathBuf;

/// Resolve the chromium / chrome binary for the browser integration tests.
/// Honors `AFHTTP_TEST_BROWSER_BIN` first, then falls back to `which`-style
/// discovery; returns `None` so tests can `skip` rather than `fail` when no
/// browser is installed.
pub fn discover_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AFHTTP_TEST_BROWSER_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    for candidate in [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Print a single skip line and return early. Used by tests that need a
/// browser when none is available so CI matrices can either provide one or
/// accept the skip.
#[macro_export]
macro_rules! skip_if_no_browser {
    () => {
        match $crate::support::env::discover_browser() {
            Some(p) => p,
            None => {
                println!("(skipping: no browser binary; set AFHTTP_TEST_BROWSER_BIN)");
                return;
            }
        }
    };
}

/// Resolve the chrome-headless-shell binary for backend-specific tests.
/// Honors `AFHTTP_TEST_CHROME_SHELL_BIN` first, then checks the standard
/// install location used in Dockerfile.test. Returns `None` so tests can
/// skip when the binary is absent.
pub fn discover_chrome_shell() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AFHTTP_TEST_CHROME_SHELL_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let standard = PathBuf::from("/usr/local/bin/chrome-headless-shell");
    if standard.exists() {
        return Some(standard);
    }
    None
}

/// Resolve the lightpanda binary for backend-specific tests. Honors
/// `AFHTTP_TEST_LIGHTPANDA_BIN` first, then `/usr/local/bin/lightpanda`.
/// Returns `None` so tests skip gracefully when the binary is absent.
pub fn discover_lightpanda() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AFHTTP_TEST_LIGHTPANDA_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let standard = PathBuf::from("/usr/local/bin/lightpanda");
    if standard.exists() {
        return Some(standard);
    }
    None
}

/// Resolve the fingerprint-chromium binary for backend-specific tests.
/// Honors `AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN` first, then
/// `/usr/local/bin/fingerprint-chromium`. Tests self-skip on arm64
/// where the upstream does not publish binaries.
pub fn discover_fingerprint_chromium() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let standard = PathBuf::from("/usr/local/bin/fingerprint-chromium");
    if standard.exists() {
        return Some(standard);
    }
    None
}

/// Resolve the foxbridge binary. The camoufox backend needs BOTH this
/// and a camoufox binary; if either is missing the test should skip.
pub fn discover_foxbridge() -> Option<PathBuf> {
    discover_simple_bin("AFHTTP_TEST_FOXBRIDGE_BIN", "foxbridge")
}

/// Resolve the camoufox binary (the stealth Firefox fork foxbridge proxies to).
pub fn discover_camoufox() -> Option<PathBuf> {
    discover_simple_bin("AFHTTP_TEST_CAMOUFOX_BIN", "camoufox")
}

/// Resolve the KasmVNC Xvnc binary for display-takeover tests.
pub fn discover_kasmvnc() -> Option<PathBuf> {
    discover_simple_bin("AFHTTP_TEST_KASMVNC_BIN", "Xvnc")
}

fn discover_simple_bin(env_var: &str, default_name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env_var) {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    for dir in ["/usr/local/bin", "/usr/bin"] {
        let standard = PathBuf::from(dir).join(default_name);
        if standard.exists() {
            return Some(standard);
        }
    }
    None
}
