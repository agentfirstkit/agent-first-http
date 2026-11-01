(selector) => {
  const hash = (value) => {
    let h = 2166136261;
    for (let i = 0; i < value.length; i++) {
      h ^= value.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  };
  const attributes = ['role', 'href', 'type', 'disabled', 'hidden', 'aria-hidden',
    'aria-expanded', 'aria-disabled', 'aria-checked', 'aria-selected', 'aria-busy'];
  const visible = (el) => {
    if (!el.getClientRects().length) return false;
    for (let current = el; current; current = current.parentElement) {
      const style = getComputedStyle(current);
      if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') return false;
    }
    return true;
  };
  const measure = (roots) => {
    const parts = [];
    let nodeCount = 0;
    const walk = (el) => {
      if (['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE'].includes(el.tagName)) return;
      nodeCount++;
      parts.push('<', el.tagName);
      for (const key of attributes) {
        if (el.hasAttribute(key)) parts.push('|', key, '=', JSON.stringify(el.getAttribute(key)));
      }
      parts.push('>');
      for (const child of el.children) walk(child);
      parts.push('</', el.tagName, '>');
    };
    for (const root of roots) walk(root);
    const text = roots.map(el => el.innerText || '').join('\n').replace(/\s+/g, ' ').trim();
    return {
      text_len: text.length, text_hash: hash(text), node_count: nodeCount,
      dom_hash: hash(parts.join('')),
      eligible: document.readyState !== 'loading' && roots.length > 0 && roots.every(el =>
        visible(el) && !el.closest('[aria-busy="true"]') && !el.querySelector('[aria-busy="true"]') &&
        (!!(el.innerText || '').trim() || el.matches('iframe,frame') || !!el.querySelector('iframe,frame'))),
      document_ready: document.readyState !== 'loading'
    };
  };
  let roots;
  try { roots = Array.from(document.querySelectorAll(selector)); }
  catch (_) { return {invalid_selector: true}; }
  return {body: measure(document.body ? [document.body] : []), selected: measure(roots)};
}
