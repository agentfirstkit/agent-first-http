(() => {
  const NODE_LIMIT = 100;
  const SCAN_LIMIT = 2000;
  const SEMANTIC_SELECTOR = [
    'a[href]', 'button', 'input', 'textarea', 'select', 'summary', 'iframe',
    '[role]', '[tabindex]', '[contenteditable="true"]'
  ].join(',');

  const trim = (s, n = 160) => (s || '').replace(/\s+/g, ' ').trim().slice(0, n);
  const queryAll = (root, selector) => {
    try {
      return Array.from(root.querySelectorAll(selector));
    } catch (_) {
      return [];
    }
  };
  const queryOne = (root, selector) => {
    try {
      return root.querySelector(selector);
    } catch (_) {
      return null;
    }
  };
  const cssPath = (el) => {
    if (!el || !el.tagName) return null;
    if (el.id) return `#${CSS.escape(el.id)}`;
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === Node.ELEMENT_NODE && parts.length < 4) {
      let part = cur.tagName.toLowerCase();
      if (cur.classList && cur.classList.length) {
        part += '.' + Array.from(cur.classList).slice(0, 2).map(c => CSS.escape(c)).join('.');
      }
      const parent = cur.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(x => x.tagName === cur.tagName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(cur) + 1})`;
      }
      parts.unshift(part);
      cur = parent;
    }
    return parts.join(' > ');
  };
  const shadowHint = (chain, local) => {
    if (!local) return null;
    if (!chain.length) return local;
    return chain.concat([local]).join(' >> shadow >> ');
  };
  const selectorUnique = (root, chain, local) => {
    if (!local) return null;
    let scope = root;
    for (const hostSelector of chain) {
      if (queryAll(scope, hostSelector).length !== 1) return false;
      const host = queryOne(scope, hostSelector);
      if (!host || !host.shadowRoot) return false;
      scope = host.shadowRoot;
    }
    return queryAll(scope, local).length === 1;
  };
  const labelFor = (root, el) => {
    if (!el.id) return '';
    const label = queryOne(root, `label[for="${CSS.escape(el.id)}"]`);
    return label ? trim(label.innerText || label.textContent) : '';
  };
  const roleFor = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button') return 'button';
    if (tag === 'select') {
      const size = Number(el.getAttribute('size') || '0');
      if (el.multiple || size > 1) return 'listbox';
      return 'combobox';
    }
    if (tag === 'textarea') return 'textbox';
    if (tag === 'iframe') return 'iframe';
    if (tag === 'input') {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (type === 'submit' || type === 'button') return 'button';
      return 'textbox';
    }
    return tag;
  };
  const actionsFor = (el) => {
    const tag = el.tagName.toLowerCase();
    const type = (el.getAttribute('type') || '').toLowerCase();
    if (tag === 'input' && ['checkbox', 'radio'].includes(type)) return ['check', 'focus'];
    if (tag === 'input' || tag === 'textarea') return ['fill', 'focus'];
    if (tag === 'select') return ['select', 'focus'];
    if (tag === 'a' || tag === 'button' || el.getAttribute('role') === 'button') return ['click', 'focus'];
    if (el.hasAttribute('contenteditable')) return ['fill', 'focus'];
    return ['click'];
  };
  const isVisible = (el, rect, style) =>
    !!(rect.width || rect.height || el.getClientRects().length) &&
    style.visibility !== 'hidden' && style.display !== 'none';
  const isSemantic = (el) => {
    try {
      return el.matches(SEMANTIC_SELECTOR);
    } catch (_) {
      return false;
    }
  };
  const childDocument = (iframe) => {
    try {
      const doc = iframe.contentDocument;
      if (doc && doc.documentElement) return doc;
    } catch (_) {
      return null;
    }
    return null;
  };

  const state = {
    nodes: [],
    forms: [],
    frames: [{frame_id: 'main', url: location.href}],
    elementRefs: new WeakMap(),
    iframeIds: new WeakMap(),
    nextNode: 0,
    nextForm: 0,
    nextFrame: 0,
    scanned: 0,
    truncated: null,
    focused_ref: null
  };
  const truncate = (reason) => {
    if (!state.truncated) {
      state.truncated = {
        reason,
        node_limit: NODE_LIMIT,
        scan_limit: SCAN_LIMIT,
        scanned: state.scanned,
        emitted_nodes: state.nodes.length
      };
    }
  };
  const countScan = () => {
    if (state.scanned >= SCAN_LIMIT) {
      truncate('scan_limit_exceeded');
      return false;
    }
    state.scanned += 1;
    return true;
  };
  const canEmit = () => {
    if (state.nodes.length >= NODE_LIMIT) {
      truncate('node_limit_exceeded');
      return false;
    }
    return true;
  };
  const frameForIframe = (el) => {
    const existing = state.iframeIds.get(el);
    if (existing) return existing;
    const frame_id = `iframe-${state.nextFrame++}`;
    let url = el.src || '';
    const doc = childDocument(el);
    if (doc && doc.location && doc.location.href) {
      url = doc.location.href;
    }
    state.iframeIds.set(el, frame_id);
    state.frames.push({frame_id, url});
    return frame_id;
  };
  const emitNode = (el, ctx) => {
    if (state.elementRefs.has(el)) return state.elementRefs.get(el);
    if (!canEmit()) return null;
    const rect = el.getBoundingClientRect();
    const style = getComputedStyle(el);
    const ref = `obs-${state.nextNode++}`;
    const tag = el.tagName.toLowerCase();
    const localSelector = cssPath(el);
    const selectorHint = shadowHint(ctx.shadowChain, localSelector);
    const type = tag === 'input' ? (el.getAttribute('type') || 'text').toLowerCase() : null;
    const frameRef = tag === 'iframe' ? frameForIframe(el) : null;
    const focused = ctx.root.activeElement === el;
    if (focused) state.focused_ref = ref;
    const name = trim(el.getAttribute('aria-label')) || trim(el.getAttribute('alt')) ||
      trim(el.getAttribute('title')) || labelFor(ctx.queryRoot, el) || trim(el.innerText || el.textContent);
    state.elementRefs.set(el, ref);
    state.nodes.push({
      ref,
      frame_id: ctx.frameId,
      role: roleFor(el),
      name: name || null,
      text: trim(el.innerText || el.textContent) || null,
      visible: isVisible(el, rect, style),
      enabled: !(el.disabled || el.getAttribute('aria-disabled') === 'true'),
      bbox: {x: rect.x, y: rect.y, width: rect.width, height: rect.height},
      actions: actionsFor(el),
      href: el.href || null,
      src: el.src || null,
      frame_ref: frameRef,
      input_type: type,
      checked: (type === 'checkbox' || type === 'radio') ? !!el.checked : null,
      selected: tag === 'option' ? !!el.selected : null,
      focused,
      value_redacted: ('value' in el && String(el.value || '').length > 0) ? true : null,
      selector_hint: selectorHint,
      selector_hint_unique: selectorUnique(ctx.queryRoot, ctx.shadowChain, localSelector)
    });
    return ref;
  };
  const collectForms = (root) => {
    for (const form of queryAll(root, 'form')) {
      const field_refs = Array.from(form.elements || [])
        .map(el => state.elementRefs.get(el) || null)
        .filter(Boolean);
      state.forms.push({
        ref: `form-${state.nextForm++}`,
        action: form.action || null,
        field_refs
      });
    }
  };
  const walkRoot = (root, ctx) => {
    const ownerDocument = root.nodeType === Node.DOCUMENT_NODE ? root : root.ownerDocument;
    const start = root.nodeType === Node.DOCUMENT_NODE ? (root.body || root.documentElement) : root;
    if (!ownerDocument || !start || state.truncated) return;
    const walker = ownerDocument.createTreeWalker(start, NodeFilter.SHOW_ELEMENT);
    while (walker.nextNode()) {
      if (!countScan() || state.truncated) break;
      const el = walker.currentNode;
      const pointer = getComputedStyle(el).cursor === 'pointer';
      if (isSemantic(el) || pointer) emitNode(el, ctx);
      if (state.truncated) break;
      if (el.shadowRoot) {
        const hostSelector = cssPath(el);
        if (hostSelector) {
          walkRoot(el.shadowRoot, {
            frameId: ctx.frameId,
            root: el.shadowRoot,
            queryRoot: ctx.queryRoot,
            shadowChain: ctx.shadowChain.concat([hostSelector])
          });
        }
      }
      if (state.truncated) break;
      if (el.tagName && el.tagName.toLowerCase() === 'iframe') {
        const frameId = frameForIframe(el);
        const doc = childDocument(el);
        if (doc) {
          walkRoot(doc, {
            frameId,
            root: doc,
            queryRoot: doc,
            shadowChain: []
          });
        }
      }
      if (state.truncated) break;
    }
    collectForms(root);
  };

  walkRoot(document, {
    frameId: 'main',
    root: document,
    queryRoot: document,
    shadowChain: []
  });
  return JSON.stringify({
    nodes: state.nodes,
    forms: state.forms,
    frames: state.frames,
    focused_ref: state.focused_ref,
    truncated: state.truncated
  });
})()
