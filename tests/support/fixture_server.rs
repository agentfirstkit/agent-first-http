//! Tiny axum HTTP server used by integration tests. Each fixture is a
//! static page; the server listens on `127.0.0.1:0` so multiple tests can
//! run in parallel without port collisions.

use std::net::SocketAddr;

use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub const PLAIN_HTML: &str =
    "<!doctype html><html><head><title>fixture</title></head><body><h1>Hello</h1></body></html>";

pub const JS_SHELL_HTML: &str = "<!doctype html><html><body><div id=\"root\">loading…</div>\
     <script>setTimeout(()=>{document.getElementById('root').innerText='ready';},50);</script>\
     </body></html>";

/// SPA shell whose initial HTTP body looks empty (a `<div id=root>` mount
/// point with a script tag that hydrates client-side). The post-hydration
/// body contains a `<h1>hydrated</h1>` so a browser-path fetch sees real
/// content while an HTTP-only fetch sees an empty shell.
pub const SPA_SHELL_HTML: &str = "<!doctype html><html><head><title>shell</title></head>\
     <body><div id=\"root\"></div>\
     <script>document.getElementById('root').innerHTML='<h1>hydrated</h1>';</script>\
     </body></html>";

/// Page that puts `.target` in the DOM immediately but only flips it
/// to visible after a setTimeout. Tests Wait::SelectorVisible: a plain
/// Wait::Selector would resolve immediately on the hidden element; the
/// visible variant must wait for the layout flip.
pub const HIDDEN_THEN_VISIBLE_HTML: &str = "<!doctype html><html><body>\
     <div class=\"target\" style=\"display:none\">delayed</div>\
     <script>setTimeout(function(){\
        var el = document.querySelector('.target');\
        el.style.display = 'block';\
        el.textContent = 'visible';\
     }, 150);</script>\
     </body></html>";

pub const JSON_PAYLOAD: &str = "{\"hello\":\"world\"}";

pub const CONSOLE_HTML: &str = "<!doctype html><html><body>\
     <script>\
       console.log('hello from fixture');\
       console.warn('warning from fixture');\
       setTimeout(()=>{ throw new Error('boom from fixture'); },10);\
     </script>\
     </body></html>";

pub const XHR_HTML: &str = "<!doctype html><html><body>\
     <div id=\"out\">pending</div>\
     <script>\
       fetch('/data.json').then(r=>r.json()).then(d=>{\
         document.getElementById('out').innerText = JSON.stringify(d);\
       });\
     </script>\
     </body></html>";

pub const DELAYED_XHR_HTML: &str = "<!doctype html><html><body>\
     <div id=\"out\">loading</div>\
     <script>\
       fetch('/delayed-data.json').then(r=>r.json()).then(d=>{\
         document.getElementById('out').innerText = d.message;\
       });\
     </script>\
     </body></html>";

pub const NEVER_XHR_HTML: &str = "<!doctype html><html><body>\
     <div id=\"out\">long poll started</div>\
     <script>fetch('/never.json').catch(()=>{});</script>\
     </body></html>";

pub const EMPTY_HTML: &str =
    "<!doctype html><html><head><title>empty</title></head><body></body></html>";

pub const CLOUDFLARE_TURNSTILE_HTML: &str = "<!doctype html><html><head>\
     <title>Just a moment...</title></head><body>\
     <h1>Checking your browser before accessing example.test.</h1>\
     <div class=\"cf-turnstile\" data-sitekey=\"test\"></div>\
     <script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script>\
     </body></html>";

pub const INTERACTIVE_HTML: &str = "<!doctype html><html><body>\
     <a id=\"docs\" href=\"/plain.html\">Docs</a>\
     <form action=\"/submit\"><label for=\"q\">Query</label><input id=\"q\" name=\"q\" value=\"secret\">\
     <button id=\"go\" type=\"submit\">Go</button><input id=\"ok\" type=\"checkbox\" checked>\
     <select id=\"single\"><option>One</option></select>\
     <select id=\"menu\" aria-haspopup=\"listbox\"><option>Menu</option></select>\
     <select id=\"multi\" multiple><option>Many</option></select></form>\
     <div id=\"click-div\" style=\"cursor:pointer\" onclick=\"window.clicked=true\">Plain Div</div>\
     <span id=\"click-span\" style=\"cursor:pointer\">Plain Span</span>\
     <iframe id=\"child\" src=\"/plain.html\"></iframe>\
     </body></html>";

pub const OBSERVATION_CONTEXTS_HTML: &str = "<!doctype html><html><body>\
     <shadow-widget id=\"shadow-host\"></shadow-widget>\
     <iframe id=\"same-frame\" src=\"/frame-inner.html\"></iframe>\
     <iframe id=\"cross-frame\"></iframe>\
     <script>\
       customElements.define('shadow-widget', class extends HTMLElement {\
         connectedCallback() {\
           const root = this.attachShadow({mode: 'open'});\
           root.innerHTML = '<button id=\"shadow-action\">Shadow Action</button><input id=\"shadow-field\" value=\"secret\">';\
         }\
       });\
       const cross = new URL(location.href).searchParams.get('cross');\
       if (cross) document.getElementById('cross-frame').src = cross;\
     </script>\
     </body></html>";

pub const FRAME_INNER_HTML: &str = "<!doctype html><html><body>\
     <button id=\"frame-action\">Frame Action</button>\
     <input id=\"frame-field\" value=\"inside\">\
     </body></html>";

pub const OBSERVATION_TRUNCATED_HTML: &str = "<!doctype html><html><body>\
     <div id=\"root\"></div>\
     <script>\
       const root = document.getElementById('root');\
       for (let i = 0; i < 130; i++) {\
         const button = document.createElement('button');\
         button.id = 'many-' + i;\
         button.textContent = 'Many ' + i;\
         root.appendChild(button);\
       }\
     </script>\
     </body></html>";

/// Page that seeds localStorage + sessionStorage synchronously and opens an
/// IndexedDB database, so a `--want storage` fetch captures all three.
pub const STORAGE_HTML: &str = "<!doctype html><html><body>\
     <script>\
       localStorage.setItem('ls_key', 'ls_value');\
       sessionStorage.setItem('ss_key', 'ss_value');\
       try { indexedDB.open('afhttp_test_db'); } catch (e) {}\
     </script>\
     </body></html>";

/// Page that seeds a localStorage value larger than the 256 KiB storage-artifact
/// cap, so `capture()` takes the truncation branch.
pub const STORAGE_LARGE_HTML: &str = "<!doctype html><html><body>\
     <script>\
       localStorage.setItem('big', 'x'.repeat(300 * 1024));\
     </script>\
     </body></html>";

pub struct Fixture {
    pub addr: SocketAddr,
    pub _handle: JoinHandle<()>,
}

impl Fixture {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Spawn the fixture server. Routes:
///   - `/plain.html` → `PLAIN_HTML` with `text/html`
///   - `/js.html` → `JS_SHELL_HTML` with `text/html`
///   - `/identity.html` → JS exposes navigator.userAgent + document.cookie
///   - `/headers.json` → request header echo
///   - `/data.json` → `JSON_PAYLOAD` with `application/json`
///   - `/slow.html` → delayed response for timeout trace tests
///   - `/delayed-xhr.html` → JS page whose text appears after a delayed XHR
///   - `/never-xhr.html` → JS page with a never-ending XHR
///   - `/interactive.html` → controls for observation tests
///   - `/observation-contexts.html` → shadow/iframe observation fixture
///   - `/frame-inner.html` → same-origin iframe body for observation tests
///   - `/observation-truncated.html` → large observation cap fixture
///   - `/404` → 404 with body "missing"
///   - `/redirect` → 302 to `/plain.html`
///   - `/sse` → text/event-stream with two events
///   - `/download.bin` → attachment response for browser download capture
pub async fn spawn() -> Fixture {
    let app = Router::new()
        .route("/plain.html", get(plain_html))
        .route("/spa-shell.html", get(spa_shell_html))
        .route("/set-cookie", get(set_cookie_handler))
        .route("/echo-cookie", get(echo_cookie_handler))
        .route("/large-body", get(large_body_handler))
        .route("/hidden-then-visible.html", get(hidden_then_visible_html))
        .route("/js.html", get(js_html))
        .route("/console.html", get(console_html))
        .route("/xhr.html", get(xhr_html))
        .route("/delayed-xhr.html", get(delayed_xhr_html))
        .route("/never-xhr.html", get(never_xhr_html))
        .route("/empty.html", get(empty_html))
        .route("/cloudflare-turnstile.html", get(cloudflare_turnstile_html))
        .route("/identity.html", get(identity_html))
        .route("/headers.json", get(headers_json))
        .route("/interactive.html", get(interactive_html))
        .route("/observation-contexts.html", get(observation_contexts_html))
        .route("/frame-inner.html", get(frame_inner_html))
        .route(
            "/observation-truncated.html",
            get(observation_truncated_html),
        )
        .route("/storage.html", get(storage_html))
        .route("/storage-large.html", get(storage_large_html))
        .route("/data.json", get(data_json))
        .route("/delayed-data.json", get(delayed_data_json))
        .route("/slow.html", get(slow_html))
        .route("/never.json", get(never_json))
        .route("/404", get(four_oh_four))
        .route("/redirect", get(redirect))
        .route("/download.bin", get(download_bin))
        .route("/sse", get(sse));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Fixture {
        addr,
        _handle: handle,
    }
}

async fn download_bin() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"fixture-download.bin\"",
            ),
        ],
        "downloaded from browser",
    )
        .into_response()
}

async fn plain_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        PLAIN_HTML,
    )
        .into_response()
}

async fn js_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        JS_SHELL_HTML,
    )
        .into_response()
}

async fn spa_shell_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        SPA_SHELL_HTML,
    )
        .into_response()
}

async fn console_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        CONSOLE_HTML,
    )
        .into_response()
}

async fn xhr_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        XHR_HTML,
    )
        .into_response()
}

async fn delayed_xhr_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DELAYED_XHR_HTML,
    )
        .into_response()
}

async fn never_xhr_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        NEVER_XHR_HTML,
    )
        .into_response()
}

async fn empty_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        EMPTY_HTML,
    )
        .into_response()
}

async fn cloudflare_turnstile_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        CLOUDFLARE_TURNSTILE_HTML,
    )
        .into_response()
}

async fn identity_html(headers: HeaderMap) -> Response {
    let server_cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let html = format!(
        "<!doctype html><html><body>\
         <pre id=\"server-cookie\">{}</pre>\
         <pre id=\"identity\"></pre>\
         <script>\
           document.getElementById('identity').innerText = JSON.stringify({{\
             ua: navigator.userAgent,\
             cookie: document.cookie\
           }});\
         </script>\
         </body></html>",
        escape_html(server_cookie)
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn headers_json(headers: HeaderMap) -> Response {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let body = serde_json::json!({
        "x-afhttp-test": get("x-afhttp-test"),
        "user-agent": get("user-agent"),
        "cookie": get("cookie"),
    })
    .to_string();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn interactive_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INTERACTIVE_HTML,
    )
        .into_response()
}

async fn observation_contexts_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        OBSERVATION_CONTEXTS_HTML,
    )
        .into_response()
}

async fn frame_inner_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        FRAME_INNER_HTML,
    )
        .into_response()
}

async fn observation_truncated_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        OBSERVATION_TRUNCATED_HTML,
    )
        .into_response()
}

async fn storage_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        STORAGE_HTML,
    )
        .into_response()
}

async fn storage_large_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        STORAGE_LARGE_HTML,
    )
        .into_response()
}

async fn data_json() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        JSON_PAYLOAD,
    )
        .into_response()
}

async fn delayed_data_json() -> Response {
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"message\":\"delayed ready\"}",
    )
        .into_response()
}

async fn slow_html() -> Response {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        PLAIN_HTML,
    )
        .into_response()
}

async fn never_json() -> Response {
    std::future::pending::<()>().await;
    (StatusCode::OK, "").into_response()
}

async fn four_oh_four() -> Response {
    (StatusCode::NOT_FOUND, "missing").into_response()
}

async fn redirect() -> Response {
    (
        StatusCode::FOUND,
        [(header::LOCATION, "/plain.html")],
        "redirecting",
    )
        .into_response()
}

async fn set_cookie_handler() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/plain".parse().expect("ct"));
    headers.append(
        header::SET_COOKIE,
        "afhttp_sid=session-token-1; Path=/".parse().expect("sc"),
    );
    headers.append(
        header::SET_COOKIE,
        "afhttp_marker=present; Path=/".parse().expect("sc"),
    );
    (StatusCode::OK, headers, "ok").into_response()
}

async fn echo_cookie_handler(headers: HeaderMap) -> Response {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let body = serde_json::json!({ "cookie": cookie }).to_string();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn hidden_then_visible_html() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        HIDDEN_THEN_VISIBLE_HTML,
    )
        .into_response()
}

async fn large_body_handler() -> Response {
    // 128 KiB of `A` bytes. The cap test sets max_response_bytes to a
    // smaller number and asserts the truncation warning + prefix.
    let body = vec![b'A'; 128 * 1024];
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        body,
    )
        .into_response()
}

async fn sse() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        "event: ping\ndata: 1\n\nevent: pong\ndata: 2\n\n",
    )
        .into_response()
}
