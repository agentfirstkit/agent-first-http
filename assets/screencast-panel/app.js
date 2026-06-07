// afhttp ops panel — live JPEG screencast + timing-preserved input replay.
//
// Two WebSocket flows against the host:
//   /ops/screencast/ws    (inbound)   — binary JPEG frames -> <canvas>
//   /ops/screencast/input (outbound)  — pointer/keyboard events with performance.now() ts
//
// The canvas backing store tracks the actual screencast frame size, but
// pointer coordinates are mapped to the target's CSS pixels using the
// viewport metadata (deviceWidth/deviceHeight) the host sends as a text
// "meta" message. The frame is the viewport scaled to fit
// startScreencast.maxWidth/Height, so frame pixels != CSS pixels whenever
// the viewport isn't exactly 1280×720 — mapping via frame pixels would put
// clicks off by that scale factor.

const status = document.getElementById("status");
const canvas = document.getElementById("screen");
const ctx = canvas.getContext("2d");

ctx.fillStyle = "#111";
ctx.fillRect(0, 0, canvas.width, canvas.height);
ctx.fillStyle = "#888";
ctx.font = "14px sans-serif";
ctx.fillText("connecting…", 12, 24);

const tokenMatch = window.location.search.match(/token=([^&]+)/);
const token = tokenMatch ? decodeURIComponent(tokenMatch[1]) : null;
const tokenQS = token ? `?token=${encodeURIComponent(token)}` : "";

const proto = window.location.protocol === "https:" ? "wss" : "ws";
const screencastUrl = `${proto}://${window.location.host}/ops/screencast/ws${tokenQS}`;
const inputUrl = `${proto}://${window.location.host}/ops/screencast/input${tokenQS}`;

// ---- screencast --------------------------------------------------------

const screencast = new WebSocket(screencastUrl);
screencast.binaryType = "blob";

screencast.onopen = () => {
  status.textContent = "screencast: live";
  window.__opsScreencastOpen = true;
};
screencast.onerror = () => {
  status.textContent = "screencast: error";
};
screencast.onclose = () => {
  status.textContent = "screencast: closed";
  window.__opsScreencastOpen = false;
};
// Latest target viewport metadata (CSS pixels). Sent by the host as a text
// frame whenever it changes; used by canvasCoords() to map operator input to
// the page's CSS pixel space regardless of how the screencast frame is scaled.
let viewportMeta = null;

screencast.onmessage = async (ev) => {
  if (typeof ev.data === "string") {
    // Text frames are JSON: viewport metadata, or error envelopes.
    try {
      const msg = JSON.parse(ev.data);
      if (msg && msg.type === "meta") {
        viewportMeta = msg;
      }
    } catch (e) {
      // Not JSON we understand; ignore for the canvas.
    }
    return;
  }
  try {
    const bmp = await createImageBitmap(ev.data);
    // Match the canvas backing store to the actual frame size for a crisp,
    // unscaled draw. Coordinate mapping no longer depends on this (it uses
    // the CSS deviceWidth/Height from the meta message). Setting .width/.height
    // clears the canvas, so we only do it when dimensions actually change —
    // typically once, on the first frame, then again if the target resizes.
    if (canvas.width !== bmp.width || canvas.height !== bmp.height) {
      canvas.width = bmp.width;
      canvas.height = bmp.height;
    }
    ctx.drawImage(bmp, 0, 0, canvas.width, canvas.height);
    if (bmp.close) bmp.close();
  } catch (e) {
    // Ignore one bad frame; the next one will land.
  }
};

// ---- input replay ------------------------------------------------------

const input = new WebSocket(inputUrl);

input.onopen = () => {
  window.__opsInputOpen = true;
};
input.onclose = () => {
  window.__opsInputOpen = false;
};
input.onerror = () => {
  // status already reflects screencast state; keep this quiet
};

function send(ev) {
  if (input.readyState !== WebSocket.OPEN) return;
  ev.timestamp_ms = performance.now();
  input.send(JSON.stringify(ev));
}

function canvasCoords(e) {
  const rect = canvas.getBoundingClientRect();
  // Map the operator's pointer to the target's CSS pixel space. The displayed
  // canvas fills `rect` and shows exactly the viewport, so the normalized
  // position within `rect` scales by the CSS viewport size (deviceWidth/Height
  // from the host's meta message). Falling back to the frame-pixel size keeps
  // the panel usable before the first meta frame arrives.
  const dw = viewportMeta ? viewportMeta.deviceWidth : canvas.width;
  const dh = viewportMeta ? viewportMeta.deviceHeight : canvas.height;
  return {
    x: ((e.clientX - rect.left) / rect.width) * dw,
    y: ((e.clientY - rect.top) / rect.height) * dh,
  };
}

function pointerButton(e) {
  switch (e.button) {
    case 1: return "middle";
    case 2: return "right";
    case 3: return "back";
    case 4: return "forward";
    default: return "left";
  }
}

canvas.addEventListener("pointermove", (e) => {
  const { x, y } = canvasCoords(e);
  send({ type: "pointer_move", x, y, timestamp_ms: 0 });
});
canvas.addEventListener("pointerdown", (e) => {
  const { x, y } = canvasCoords(e);
  send({ type: "pointer_down", x, y, button: pointerButton(e), timestamp_ms: 0 });
});
canvas.addEventListener("pointerup", (e) => {
  const { x, y } = canvasCoords(e);
  send({ type: "pointer_up", x, y, button: pointerButton(e), timestamp_ms: 0 });
});
canvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  const { x, y } = canvasCoords(e);
  send({ type: "wheel", x, y, dx: e.deltaX, dy: e.deltaY, timestamp_ms: 0 });
}, { passive: false });

function keyModifiers(e) {
  // CDP modifier bits: 1=Alt, 2=Ctrl, 4=Meta, 8=Shift.
  let m = 0;
  if (e.altKey) m |= 1;
  if (e.ctrlKey) m |= 2;
  if (e.metaKey) m |= 4;
  if (e.shiftKey) m |= 8;
  return m;
}

// The paste shortcut (Ctrl/⌘+V) is special-cased: the keystroke can't carry
// the operator's clipboard text to the target browser, so we read it here and
// relay it as Input.insertText instead (see relayPaste). Every other key —
// including shortcuts like Ctrl/⌘+A (select-all) or Ctrl/⌘+R (reload) — is
// relayed normally; the host drops the `text` field whenever Ctrl/⌘ is held
// (see one_char_text), so chromium runs them as shortcuts rather than typing
// the bare letter.
function isPasteCombo(e) {
  return (e.ctrlKey || e.metaKey) && (e.key === "v" || e.key === "V");
}

// Paste. The operator's clipboard never reaches the target browser, so we
// read it here and relay it as Input.insertText (inserted at the focused
// element's caret — click the target field first). We can't rely on the DOM
// `paste` event: it only fires when an *editable* element is focused, but the
// focused element here is the (non-editable) <canvas>, so the browser never
// emits it. Instead we read the clipboard on the Ctrl/⌘+V keydown — a user
// gesture, and 127.0.0.1 is a secure context, so navigator.clipboard is
// allowed (the browser may prompt for clipboard-read permission once).
function relayPaste() {
  if (!navigator.clipboard || !navigator.clipboard.readText) return;
  navigator.clipboard
    .readText()
    .then((text) => {
      if (text) send({ type: "insert_text", text, timestamp_ms: 0 });
    })
    .catch(() => {
      /* permission denied or empty clipboard — nothing to relay */
    });
}

window.addEventListener("keydown", (e) => {
  if (isPasteCombo(e)) {
    e.preventDefault();
    relayPaste();
    return;
  }
  send({
    type: "key_down",
    key: e.key,
    code: e.code,
    modifiers: keyModifiers(e),
    timestamp_ms: 0,
  });
});
window.addEventListener("keyup", (e) => {
  if (isPasteCombo(e)) {
    e.preventDefault();
    return;
  }
  send({
    type: "key_up",
    key: e.key,
    code: e.code,
    modifiers: keyModifiers(e),
    timestamp_ms: 0,
  });
});

// Make the canvas focusable + keyboard-driven.
canvas.tabIndex = 0;
canvas.style.cursor = "crosshair";
