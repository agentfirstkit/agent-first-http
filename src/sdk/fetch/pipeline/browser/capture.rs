//! Page/DOM/body capture helpers for the browser path: location and snapshot
//! reads, rendered HTML / inner text / screenshot, and response-body decoding.

use serde_json::Value;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::content::ContentCapture;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::shared::error::{Error, ErrorCode};

use super::cdp_send;

#[derive(Debug, Clone)]
pub(super) struct PageSnapshot {
    pub(super) url: String,
    pub(super) title: String,
    pub(super) ready_state: String,
    pub(super) text: String,
    pub(super) html: String,
}

impl PageSnapshot {
    pub(super) fn has_dom_content(&self) -> bool {
        !self.html.trim().is_empty()
            && self.html.trim() != "<html><head></head><body></body></html>"
    }
}

pub(super) async fn capture_location(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<Option<String>, Error> {
    let doc = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "location.href",
            "returnByValue": true,
        }),
        "capture_location",
        deadline,
    )
    .await?;
    Ok(doc["result"]["value"].as_str().map(str::to_string))
}

pub(super) async fn capture_page_snapshot(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<PageSnapshot, Error> {
    let doc = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": PAGE_SNAPSHOT_JS,
            "returnByValue": true,
        }),
        "capture_page_snapshot",
        deadline,
    )
    .await?;
    let s = doc["result"]["value"].as_str().unwrap_or("{}");
    let v: Value = serde_json::from_str(s).unwrap_or(Value::Null);
    Ok(PageSnapshot {
        url: v["url"].as_str().unwrap_or("").to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        ready_state: v["ready_state"].as_str().unwrap_or("").to_string(),
        text: v["text"].as_str().unwrap_or("").to_string(),
        html: v["html"].as_str().unwrap_or("").to_string(),
    })
}

const PAGE_SNAPSHOT_JS: &str = r#"(() => {
  const text = document.body ? document.body.innerText : '';
  const html = document.documentElement ? document.documentElement.outerHTML : '';
  return JSON.stringify({
    url: location.href,
    title: document.title || '',
    ready_state: document.readyState || '',
    text: text.slice(0, 200000),
    html: html.slice(0, 200000)
  });
})()"#;

pub(super) async fn capture_response_body(
    conn: &Connection,
    session_id: &str,
    request_id: &str,
    deadline: &FetchDeadline,
) -> Result<Vec<u8>, Error> {
    let resp = cdp_send(
        conn,
        session_id,
        "Network.getResponseBody",
        &serde_json::json!({"requestId": request_id}),
        "capture_body",
        deadline,
    )
    .await?;
    decode_response_body(request_id, &resp)
}

pub(super) fn decode_response_body(
    request_id: &str,
    resp: &serde_json::Value,
) -> Result<Vec<u8>, Error> {
    let body_str = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let base64_encoded = resp
        .get("base64Encoded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if base64_encoded {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(body_str)
            .map_err(|e| {
                Error::new(
                    ErrorCode::ArtifactCaptureFailed,
                    format!("base64 decode for {request_id}: {e}"),
                )
            })
    } else {
        Ok(body_str.as_bytes().to_vec())
    }
}

pub(super) async fn navigation_status_from_performance(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Option<u16> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "(() => { const nav = performance.getEntriesByType('navigation')[0]; return nav && Number.isFinite(nav.responseStatus) ? nav.responseStatus : 0; })()",
            "returnByValue": true,
        }),
        "capture_status",
        deadline,
    )
    .await
    .ok()?;
    let status = r["result"]["value"].as_u64()?;
    if (100..=599).contains(&status) {
        Some(status as u16)
    } else {
        None
    }
}

pub(super) async fn capture_outer_html(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    if let Ok(html) = capture_outer_html_via_runtime(conn, session_id, deadline).await {
        if !html.is_empty() {
            return Ok(html);
        }
    }
    let dom_outer = async {
        let doc = cdp_send(
            conn,
            session_id,
            "DOM.getDocument",
            &serde_json::json!({"depth": -1, "pierce": true}),
            "capture_rendered_html",
            deadline,
        )
        .await?;
        let node_id = doc["root"]["nodeId"].as_i64().ok_or_else(|| {
            Error::new(ErrorCode::CdpError, "DOM.getDocument: missing root nodeId")
        })?;
        let outer = cdp_send(
            conn,
            session_id,
            "DOM.getOuterHTML",
            &serde_json::json!({"nodeId": node_id}),
            "capture_rendered_html",
            deadline,
        )
        .await?;
        outer["outerHTML"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::new(ErrorCode::CdpError, "DOM.getOuterHTML: missing outerHTML"))
    }
    .await;

    match dom_outer {
        Ok(html) if !html.is_empty() => Ok(html),
        Err(e) => Err(e),
        _ => Ok(String::new()),
    }
}

async fn capture_outer_html_via_runtime(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "document.documentElement ? document.documentElement.outerHTML : ''",
            "returnByValue": true,
        }),
        "capture_rendered_html",
        deadline,
    )
    .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

pub(super) async fn capture_inner_text(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "document.body ? document.body.innerText : ''",
            "returnByValue": true,
        }),
        "capture_text",
        deadline,
    )
    .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

pub(super) async fn capture_content(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<ContentCapture, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": CONTENT_CAPTURE_JS,
            "returnByValue": true,
            "awaitPromise": true,
        }),
        "capture_content",
        deadline,
    )
    .await?;
    let s = r["result"]["value"].as_str().unwrap_or("{}");
    let v: Value = serde_json::from_str(s).map_err(|e| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!("content: parse json: {e}"),
        )
    })?;
    Ok(ContentCapture {
        markdown: v["markdown"].as_str().unwrap_or("").to_string(),
        json: v.get("content").cloned().unwrap_or(Value::Null),
    })
}

const CONTENT_CAPTURE_JS: &str = r###"(() => {
  const TEXT_NODE = 3;
  const ELEMENT_NODE = 1;
  const DOCUMENT_NODE = 9;
  const FRAGMENT_NODE = 11;
  const MAX_TEXT_CHARS = 200000;
  const MAX_LINKS = 1000;
  const MAX_ACTIONS = 500;
  const MAX_ITEMS = 300;
  const MAX_TABLES = 100;

  const warnings = [];
  const flow = [];
  const sections = [];
  const links = [];
  const linkKeys = new Set();
  const actions = [];
  const tables = [];
  const items = [];
  const itemKeys = new Set();
  let paragraph = [];
  let currentSectionIndex = -1;
  let emittedTextChars = 0;

  function lowerTag(el) {
    return (el && el.tagName ? el.tagName : '').toLowerCase();
  }

  function truncate(value, max) {
    value = String(value || '');
    return value.length > max ? value.slice(0, max - 1) + '…' : value;
  }

  function normalizeText(value) {
    let text = String(value || '')
      .replace(/\u00a0/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
    for (let i = 0; i < 3; i++) {
      text = text
        .replace(/([€$£¥])\s+(\d+)\s*\.\s*(\d{1,2})/g, '$1$2.$3')
        .replace(/(\d+)\s*\.\s*(\d{1,2})\s+([€$£¥])/g, '$1.$2$3')
        .replace(/(\d+)\s+(\d{2})\s*([€$£¥])\s*\/\s*(month|mo\.?|monthly)/ig, '$3$1.$2 / $4');
    }
    return text;
  }

  function isHiddenElement(el) {
    const tag = lowerTag(el);
    if (!tag) return false;
    if (['script', 'style', 'noscript', 'template', 'meta', 'link'].includes(tag)) return true;
    if (el.hidden || el.getAttribute('aria-hidden') === 'true') return true;
    if (tag === 'input' && String(el.type || '').toLowerCase() === 'hidden') return true;
    try {
      const view = el.ownerDocument && el.ownerDocument.defaultView;
      const style = view ? view.getComputedStyle(el) : getComputedStyle(el);
      if (style && (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse')) {
        return true;
      }
    } catch (_) {}
    return false;
  }

  function childrenFor(node) {
    if (!node) return [];
    if (node.nodeType === DOCUMENT_NODE) return Array.from(node.childNodes || []);
    if (node.nodeType === FRAGMENT_NODE) return Array.from(node.childNodes || []);
    if (node.nodeType !== ELEMENT_NODE) return [];
    const tag = lowerTag(node);
    if (tag === 'slot' && typeof node.assignedNodes === 'function') {
      const assigned = node.assignedNodes({flatten: true});
      if (assigned && assigned.length) return Array.from(assigned);
    }
    if (node.shadowRoot) return Array.from(node.shadowRoot.childNodes || []);
    return Array.from(node.childNodes || []);
  }

  function deepText(node, limit = 5000) {
    const parts = [];
    let chars = 0;
    const seen = new Set();
    function visit(n) {
      if (!n || chars >= limit || seen.has(n)) return;
      seen.add(n);
      if (n.nodeType === TEXT_NODE) {
        const text = normalizeText(n.nodeValue || '');
        if (text) {
          parts.push(text);
          chars += text.length + 1;
        }
        return;
      }
      if (n.nodeType !== ELEMENT_NODE && n.nodeType !== DOCUMENT_NODE && n.nodeType !== FRAGMENT_NODE) return;
      if (n.nodeType === ELEMENT_NODE) {
        if (isHiddenElement(n)) return;
        const tag = lowerTag(n);
        if (tag === 'img') {
          const alt = normalizeText(n.getAttribute('alt') || n.getAttribute('title') || '');
          if (alt) {
            parts.push(alt);
            chars += alt.length + 1;
          }
        }
      }
      for (const child of childrenFor(n)) visit(child);
    }
    visit(node);
    return truncate(normalizeText(parts.join(' ')), limit);
  }

  function firstHeadingText(node) {
    let found = '';
    const seen = new Set();
    function visit(n) {
      if (!n || found || seen.has(n)) return;
      seen.add(n);
      if (n.nodeType !== ELEMENT_NODE && n.nodeType !== DOCUMENT_NODE && n.nodeType !== FRAGMENT_NODE) return;
      if (n.nodeType === ELEMENT_NODE) {
        if (isHiddenElement(n)) return;
        if (/^h[1-6]$/.test(lowerTag(n))) {
          found = deepText(n, 200);
          return;
        }
      }
      for (const child of childrenFor(n)) visit(child);
    }
    visit(node);
    return found;
  }

  function currentSectionHeading() {
    return currentSectionIndex >= 0 ? sections[currentSectionIndex].heading : 'Page';
  }

  function ensureSection() {
    if (currentSectionIndex >= 0) return;
    sections.push({heading: 'Page', level: 1, text: [], links: []});
    currentSectionIndex = 0;
  }

  function flushParagraph() {
    const text = normalizeText(paragraph.join(' '));
    paragraph = [];
    if (!text || emittedTextChars >= MAX_TEXT_CHARS) return;
    const prev = flow.length ? flow[flow.length - 1] : null;
    if (prev && prev.kind === 'paragraph' && prev.text === text) return;
    const clipped = truncate(text, Math.max(0, MAX_TEXT_CHARS - emittedTextChars));
    if (!clipped) return;
    flow.push({kind: 'paragraph', text: clipped});
    ensureSection();
    sections[currentSectionIndex].text.push(clipped);
    emittedTextChars += clipped.length + 1;
  }

  function addText(text) {
    text = normalizeText(text);
    if (!text) return;
    paragraph.push(text);
  }

  function addHeading(level, text) {
    text = normalizeText(text);
    if (!text) return;
    flushParagraph();
    currentSectionIndex = sections.length;
    sections.push({heading: text, level, text: [], links: []});
    flow.push({kind: 'heading', level, text});
  }

  function absoluteUrl(raw) {
    try { return new URL(raw, location.href).href; } catch (_) { return ''; }
  }

  function linkHref(el) {
    const attr = el.getAttribute('href');
    if (attr === null) return '';
    const raw = attr.trim();
    const lower = raw.toLowerCase();
    if (!raw || raw === '#' || lower.startsWith('javascript:')) return '';
    return raw;
  }

  function registrableish(host) {
    const parts = String(host || '').toLowerCase().split('.').filter(Boolean);
    if (parts.length <= 2) return parts.join('.');
    return parts.slice(-2).join('.');
  }

  function sameSite(abs) {
    try {
      const url = new URL(abs);
      const here = new URL(location.href);
      return registrableish(url.hostname) === registrableish(here.hostname);
    } catch (_) {
      return false;
    }
  }

  function hasAncestor(el, tagName) {
    let cur = el;
    const wanted = tagName.toLowerCase();
    while (cur && cur.nodeType === ELEMENT_NODE) {
      if (lowerTag(cur) === wanted) return true;
      cur = cur.parentElement;
    }
    return false;
  }

  function linkKind(text, abs, el) {
    const joined = `${text || ''} ${abs || ''}`.toLowerCase();
    if (/login|log in|sign in|signin|account/.test(joined)) return 'login';
    if (/signup|sign up|register|create account/.test(joined)) return 'signup';
    if (/pricing|prices?|kosten|preis/.test(joined)) return 'pricing';
    if (/docs?|documentation|tutorial|guide|manual|api/.test(joined)) return 'docs';
    if (/explore|plan|product|server|cloud|vps|package/.test(joined)) return 'product_detail';
    if (hasAncestor(el, 'footer')) return 'footer';
    if (hasAncestor(el, 'nav')) return 'nav';
    return sameSite(abs) ? 'unknown' : 'external';
  }

  function addSectionLink(link) {
    ensureSection();
    const section = sections[currentSectionIndex];
    if (!section.links.some(l => l.absolute_url === link.absolute_url && l.text === link.text)) {
      section.links.push(link);
    }
  }

  function addLink(el, explicitSource) {
    if (links.length >= MAX_LINKS) return null;
    const raw = linkHref(el);
    if (!raw) return null;
    const abs = absoluteUrl(raw);
    if (!abs) return null;
    const text = normalizeText(
      deepText(el, 500) ||
      el.getAttribute('aria-label') ||
      el.getAttribute('title') ||
      raw
    );
    const sourceSection = explicitSource || currentSectionHeading();
    const link = {
      text: truncate(text || abs, 240),
      url: raw,
      absolute_url: abs,
      same_site: sameSite(abs),
      visible: !isHiddenElement(el),
      source_section: sourceSection,
      kind: linkKind(text, abs, el)
    };
    const key = `${link.absolute_url}\n${link.text}`;
    if (!linkKeys.has(key)) {
      linkKeys.add(key);
      links.push(link);
    }
    addSectionLink(link);
    return link;
  }

  function addAction(el) {
    if (actions.length >= MAX_ACTIONS) return;
    const tag = lowerTag(el);
    const text = normalizeText(
      deepText(el, 500) ||
      el.getAttribute('aria-label') ||
      el.getAttribute('title') ||
      el.value ||
      ''
    );
    if (!text) return;
    const kind = tag === 'a' ? 'link' : (tag === 'input' ? String(el.type || 'input') : (el.getAttribute('role') || tag));
    const action = {
      text: truncate(text, 240),
      kind,
      visible: !isHiddenElement(el),
      source_section: currentSectionHeading()
    };
    if (tag === 'a') {
      const raw = linkHref(el);
      const abs = raw ? absoluteUrl(raw) : '';
      if (abs) action.absolute_url = abs;
    }
    if (!actions.some(a => a.kind === action.kind && a.text === action.text && a.absolute_url === action.absolute_url)) {
      actions.push(action);
    }
  }

  function collectLinksIn(root) {
    const out = [];
    const seen = new Set();
    function visit(n) {
      if (!n || seen.has(n) || out.length >= 50) return;
      seen.add(n);
      if (n.nodeType !== ELEMENT_NODE && n.nodeType !== DOCUMENT_NODE && n.nodeType !== FRAGMENT_NODE) return;
      if (n.nodeType === ELEMENT_NODE) {
        if (isHiddenElement(n)) return;
        if (lowerTag(n) === 'a') {
          const raw = linkHref(n);
          if (!raw) {
            for (const child of childrenFor(n)) visit(child);
            return;
          }
          const abs = absoluteUrl(raw);
          if (abs) {
            const text = normalizeText(deepText(n, 500) || n.getAttribute('aria-label') || n.getAttribute('title') || raw);
            out.push({
              text: truncate(text || abs, 240),
              url: raw,
              absolute_url: abs,
              same_site: sameSite(abs),
              visible: !isHiddenElement(n),
              source_section: currentSectionHeading(),
              kind: linkKind(text, abs, n)
            });
          }
        }
      }
      for (const child of childrenFor(n)) visit(child);
    }
    visit(root);
    return out;
  }

  function firstLine(text) {
    const chunks = normalizeText(text).split(/(?<=[.!?])\s+| {2,}/).filter(Boolean);
    return truncate(chunks[0] || text, 120);
  }

  function maybeCollectItem(el, context) {
    if (items.length >= MAX_ITEMS || context.inItem) return false;
    const tag = lowerTag(el);
    const signature = `${el.id || ''} ${el.className || ''} ${el.getAttribute('role') || ''}`;
    const candidate = tag === 'article' || tag === 'li' || /card|product|plan|pricing|package|server|tile|box/i.test(signature);
    if (!candidate || tag === 'body' || tag === 'main') return false;
    const text = deepText(el, 1200);
    if (text.length < 20 || text.length > 1200) return false;
    const title = firstHeadingText(el) || firstLine(text);
    const key = `${title}\n${text}`;
    if (itemKeys.has(key)) return true;
    itemKeys.add(key);
    items.push({
      kind: 'card',
      title: truncate(title, 160),
      text: [text],
      links: collectLinksIn(el),
      source_section: currentSectionHeading()
    });
    return true;
  }

  function isBlock(tag) {
    return /^(address|article|aside|blockquote|body|dd|details|dialog|div|dl|dt|fieldset|figcaption|figure|footer|form|header|hr|li|main|nav|ol|p|pre|section|summary|ul)$/.test(tag);
  }

  function extractTable(el) {
    const rows = [];
    for (const tr of Array.from(el.querySelectorAll('tr'))) {
      if (isHiddenElement(tr)) continue;
      const cells = Array.from(tr.querySelectorAll('th,td'))
        .filter(cell => !isHiddenElement(cell))
        .map(cell => deepText(cell, 1000))
        .filter(Boolean);
      if (cells.length) rows.push(cells);
      if (rows.length >= 100) break;
    }
    if (!rows.length) return null;
    const caption = deepText(el.querySelector('caption'), 300);
    const table = {
      caption,
      rows,
      source_section: currentSectionHeading()
    };
    tables.push(table);
    return table;
  }

  function addTable(el) {
    if (tables.length >= MAX_TABLES) return;
    flushParagraph();
    const table = extractTable(el);
    if (table) flow.push({kind: 'table', index: tables.length - 1});
  }

  function walkIframe(el, context) {
    flushParagraph();
    const src = absoluteUrl(el.getAttribute('src') || el.src || '');
    const previousSectionIndex = currentSectionIndex;
    try {
      const doc = el.contentDocument;
      if (doc && doc.body) {
        addHeading(3, `Frame: ${src || 'same-origin iframe'}`);
        walk(doc.body, {...context, inItem: false});
        flushParagraph();
        currentSectionIndex = previousSectionIndex;
      } else {
        warnings.push({code: 'iframe_unavailable', detail: `iframe content unavailable: ${src || '(no src)'}`});
      }
    } catch (err) {
      warnings.push({code: 'iframe_cross_origin', detail: `cross-origin iframe not traversed: ${src || String(err)}`});
    }
  }

  function walk(node, context = {inItem: false}) {
    if (!node || emittedTextChars >= MAX_TEXT_CHARS) return;
    if (node.nodeType === TEXT_NODE) {
      addText(node.nodeValue || '');
      return;
    }
    if (node.nodeType !== ELEMENT_NODE && node.nodeType !== DOCUMENT_NODE && node.nodeType !== FRAGMENT_NODE) return;

    if (node.nodeType === ELEMENT_NODE) {
      if (isHiddenElement(node)) return;
      const tag = lowerTag(node);
      if (tag === 'br') {
        flushParagraph();
        return;
      }
      if (tag === 'iframe') {
        walkIframe(node, context);
        return;
      }
      if (/^h[1-6]$/.test(tag)) {
        addHeading(Number(tag.slice(1)), deepText(node, 1000));
        return;
      }
      if (tag === 'table') {
        addTable(node);
        return;
      }
      if (tag === 'a') addLink(node);
      const role = String(node.getAttribute('role') || '').toLowerCase();
      if (tag === 'button' || role === 'button' || (tag === 'input' && /^(button|submit|reset)$/i.test(node.type || ''))) {
        addAction(node);
      }
      if (tag === 'img') {
        addText(node.getAttribute('alt') || node.getAttribute('title') || '');
      }
      const block = isBlock(tag);
      if (block) flushParagraph();
      const isItem = maybeCollectItem(node, context);
      const childContext = isItem ? {...context, inItem: true} : context;
      for (const child of childrenFor(node)) walk(child, childContext);
      if (block) flushParagraph();
      return;
    }

    for (const child of childrenFor(node)) walk(child, context);
  }

  function escapeMd(value) {
    return String(value || '').replace(/\[/g, '\\[').replace(/\]/g, '\\]');
  }

  function escapeTableCell(value) {
    return normalizeText(value).replace(/\|/g, '\\|');
  }

  function renderTable(table) {
    if (!table || !table.rows || !table.rows.length) return [];
    const width = Math.max(...table.rows.map(row => row.length));
    const rows = table.rows.map(row => {
      const filled = row.slice();
      while (filled.length < width) filled.push('');
      return filled.map(escapeTableCell);
    });
    const header = rows[0];
    const body = rows.slice(1);
    const out = [];
    if (table.caption) out.push(`**${escapeMd(table.caption)}**`, '');
    out.push(`| ${header.join(' | ')} |`);
    out.push(`| ${header.map(() => '---').join(' | ')} |`);
    for (const row of body) out.push(`| ${row.join(' | ')} |`);
    return out;
  }

  walk(document.body || document.documentElement || document);
  flushParagraph();

  if (!flow.length) warnings.push({code: 'content_empty', detail: 'no visible composed content captured'});
  if (emittedTextChars >= MAX_TEXT_CHARS) warnings.push({code: 'content_truncated', detail: `content text capped at ${MAX_TEXT_CHARS} characters`});

  const page = {
    url: location.href,
    final_url: location.href,
    title: document.title || '',
    ready_state: document.readyState || '',
    capture: 'browser_composed_content'
  };

  const md = [];
  md.push(`# ${escapeMd(page.title || page.final_url || 'Page Content')}`);
  md.push('');
  md.push(`URL: ${page.url}`);
  md.push(`Final URL: ${page.final_url}`);
  if (page.title) md.push(`Title: ${page.title}`);
  md.push(`Ready State: ${page.ready_state || 'unknown'}`);
  md.push('');
  md.push('## Visible Content');
  md.push('');
  if (!flow.length) {
    md.push('_No visible composed content captured._');
  } else {
    for (const entry of flow) {
      if (entry.kind === 'heading') {
        const level = Math.min(6, Math.max(2, Number(entry.level || 1) + 1));
        md.push(`${'#'.repeat(level)} ${escapeMd(entry.text)}`);
        md.push('');
      } else if (entry.kind === 'paragraph') {
        md.push(entry.text);
        md.push('');
      } else if (entry.kind === 'table') {
        md.push(...renderTable(tables[entry.index]));
        md.push('');
      }
    }
  }

  if (links.length) {
    md.push('## Links');
    md.push('');
    for (const link of links.slice(0, 200)) {
      const scope = link.same_site ? 'same-site' : 'external';
      const meta = [link.kind, link.kind === scope ? '' : scope, link.source_section ? `source: ${link.source_section}` : '']
        .filter(Boolean)
        .join(', ');
      md.push(`- [${escapeMd(link.text || link.absolute_url)}](${link.absolute_url})${meta ? ` — ${meta}` : ''}`);
    }
    if (links.length > 200) md.push(`- … ${links.length - 200} more links omitted`);
    md.push('');
  }

  if (actions.length) {
    md.push('## Actions');
    md.push('');
    for (const action of actions.slice(0, 100)) {
      md.push(`- ${escapeMd(action.text)} — ${action.kind}${action.source_section ? `, source: ${action.source_section}` : ''}`);
    }
    if (actions.length > 100) md.push(`- … ${actions.length - 100} more actions omitted`);
    md.push('');
  }

  if (warnings.length) {
    md.push('## Warnings');
    md.push('');
    for (const warning of warnings) md.push(`- ${warning.code}: ${warning.detail}`);
    md.push('');
  }

  const content = {
    schema_version: 1,
    page,
    sections,
    items,
    tables,
    links,
    actions,
    warnings
  };

  return JSON.stringify({
    markdown: md.join('\n').replace(/\n{3,}/g, '\n\n').slice(0, 300000),
    content
  });
})()"###;

pub(super) async fn capture_screenshot(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<Vec<u8>, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Page.captureScreenshot",
        &serde_json::json!({"format": "png", "captureBeyondViewport": false}),
        "capture_screenshot",
        deadline,
    )
    .await?;
    let b64 = r["data"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::CdpError, "captureScreenshot: missing data"))?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| {
            Error::new(
                ErrorCode::ArtifactCaptureFailed,
                format!("screenshot base64 decode: {e}"),
            )
        })
}
