// The Astrum website: light and dark, the menus on a phone, copy buttons, "On this page",
// and the search over the documentation. Everything works without it, except the search.
(() => {
  const root = document.documentElement;
  const base = root.dataset.root || '';
  const store = (key, value) => {
    try {
      if (value === undefined) return localStorage.getItem(key);
      localStorage.setItem(key, value);
    } catch (e) {
      return null;
    }
  };

  // ---- light and dark ----
  const dark = () => root.dataset.theme === 'dark' || (!root.dataset.theme && matchMedia('(prefers-color-scheme: dark)').matches);
  for (const button of document.querySelectorAll('.theme-toggle')) {
    const label = () => button.setAttribute('aria-label', dark() ? 'Switch to light' : 'Switch to dark');
    label();
    button.addEventListener('click', () => {
      root.dataset.theme = dark() ? 'light' : 'dark';
      store('astrum-theme', root.dataset.theme);
      label();
    });
  }

  // ---- the menus on a narrow screen ----
  const toggle = (button, target, cls) => {
    if (!button || !target) return;
    button.addEventListener('click', () => {
      const open = target.classList.toggle(cls);
      button.setAttribute('aria-expanded', String(open));
    });
  };
  toggle(document.querySelector('.nav-toggle'), document.getElementById('site-nav'), 'open');
  toggle(document.querySelector('.docs-menu-toggle'), document.querySelector('.docs-sidebar'), 'open');

  // ---- copying ----
  const copy = async (button, text) => {
    try {
      await navigator.clipboard.writeText(text);
      const was = button.textContent;
      button.textContent = 'Copied';
      setTimeout(() => (button.textContent = was), 1400);
    } catch (e) {
      button.textContent = 'Select and copy';
    }
  };
  const canCopy = !!(navigator.clipboard && window.isSecureContext);
  for (const button of document.querySelectorAll('button.copy[data-copy]')) {
    if (!canCopy) button.remove();
    else button.addEventListener('click', () => copy(button, button.dataset.copy));
  }
  if (canCopy) {
    for (const pre of document.querySelectorAll('pre')) {
      if (pre.dataset.lang === 'text') continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'copy';
      button.textContent = 'Copy';
      button.addEventListener('click', () => copy(button, pre.querySelector('code').innerText.replace(/\n$/, '')));
      pre.appendChild(button);
    }
  }

  // ---- a screenshot at full size ----
  for (const img of document.querySelectorAll('.shot img')) {
    img.addEventListener('click', () => {
      const back = document.createElement('div');
      back.className = 'zoomed';
      back.setAttribute('role', 'dialog');
      back.setAttribute('aria-label', img.alt);
      back.tabIndex = -1;
      const big = document.createElement('img');
      big.src = img.currentSrc || img.src;
      big.alt = img.alt;
      back.appendChild(big);
      const close = () => {
        back.remove();
        removeEventListener('keydown', onKey);
      };
      const onKey = (e) => e.key === 'Escape' && close();
      back.addEventListener('click', close);
      addEventListener('keydown', onKey);
      document.body.appendChild(back);
      back.focus();
    });
  }

  // ---- "On this page" follows the reading ----
  const links = [...document.querySelectorAll('.toc a')];
  if (links.length && 'IntersectionObserver' in window) {
    const byId = new Map(links.map((a) => [decodeURIComponent(a.hash.slice(1)), a]));
    const visible = new Set();
    const mark = () => {
      const heads = [...byId.keys()].map((id) => document.getElementById(id)).filter(Boolean);
      let current = heads[0];
      for (const h of heads) if (h.getBoundingClientRect().top < 140) current = h;
      for (const a of links) a.classList.toggle('current', current && a === byId.get(current.id));
    };
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) (e.isIntersecting ? visible.add : visible.delete).call(visible, e.target.id);
      mark();
    });
    for (const id of byId.keys()) {
      const h = document.getElementById(id);
      if (h) io.observe(h);
    }
    addEventListener('scroll', mark, { passive: true });
    mark();
  }

  // ---- search ----
  const box = document.querySelector('.search');
  const input = document.getElementById('search');
  const list = document.getElementById('search-results');
  if (!box || !input || !list || location.protocol === 'file:') return;
  let index = null;
  let picked = -1;
  fetch(base + 'search.json')
    .then((r) => (r.ok ? r.json() : Promise.reject()))
    .then((data) => {
      index = data.map((d) => ({ ...d, lt: d.t.toLowerCase(), lh: d.h.toLowerCase(), lx: d.x.toLowerCase() }));
      box.hidden = false;
      box.classList.add('ready');
    })
    .catch(() => {});

  const escape = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
  const words = (q) => q.toLowerCase().split(/\s+/).filter((w) => w.length > 1 || /\w/.test(w));
  // The words found are marked in the text itself, which is escaped piece by piece after.
  const mark = (text, ws) => {
    const lower = text.toLowerCase();
    const hit = new Array(text.length).fill(false);
    for (const w of ws) for (let at = lower.indexOf(w); at >= 0; at = lower.indexOf(w, at + 1)) hit.fill(true, at, at + w.length);
    let out = '';
    for (let i = 0; i < text.length; ) {
      let j = i;
      while (j < text.length && hit[j] === hit[i]) j++;
      out += hit[i] ? `<mark>${escape(text.slice(i, j))}</mark>` : escape(text.slice(i, j));
      i = j;
    }
    return out;
  };
  const excerpt = (text, ws) => {
    const at = Math.max(0, ...ws.map((w) => text.toLowerCase().indexOf(w)).filter((i) => i >= 0).slice(0, 1));
    const from = Math.max(0, at - 50);
    return (from ? '…' : '') + text.slice(from, from + 150) + (from + 150 < text.length ? '…' : '');
  };
  const show = () => {
    const ws = words(input.value);
    picked = -1;
    if (!index || !ws.length) {
      list.hidden = true;
      input.setAttribute('aria-expanded', 'false');
      return;
    }
    const scored = [];
    for (const d of index) {
      let score = 0;
      let all = true;
      for (const w of ws) {
        const s = (d.lh.includes(w) ? 8 : 0) + (d.lt.includes(w) ? 5 : 0) + (d.lx.includes(w) ? 1 + Math.min(3, d.lx.split(w).length - 2) : 0);
        if (!s) all = false;
        score += s;
      }
      if (all) scored.push([score, d]);
    }
    scored.sort((a, b) => b[0] - a[0]);
    list.innerHTML = scored.length
      ? scored
          .slice(0, 10)
          .map(([, d], i) => `<li><a id="found-${i}" role="option" href="${base}${d.u}"><span class="where">${escape(d.t)}</span>${mark(d.h || d.t, ws)}<span class="what">${mark(excerpt(d.x, ws), ws)}</span></a></li>`)
          .join('')
      : '<li class="none">Nothing found.</li>';
    list.hidden = false;
    input.setAttribute('aria-expanded', 'true');
  };
  const choose = (i) => {
    const items = [...list.querySelectorAll('a')];
    if (!items.length) return;
    picked = (i + items.length) % items.length;
    items.forEach((a, k) => a.setAttribute('aria-selected', String(k === picked)));
    input.setAttribute('aria-activedescendant', items[picked].id);
    items[picked].scrollIntoView({ block: 'nearest' });
  };
  input.addEventListener('input', show);
  input.addEventListener('focus', show);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      choose(picked + 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      choose(picked - 1);
    } else if (e.key === 'Enter') {
      const a = list.querySelectorAll('a')[Math.max(0, picked)];
      if (a) location.href = a.href;
    } else if (e.key === 'Escape') {
      input.value = '';
      show();
    }
  });
  document.addEventListener('click', (e) => {
    if (!box.contains(e.target)) list.hidden = true;
  });
  // "/" goes to the search, as on many documentation sites.
  document.addEventListener('keydown', (e) => {
    if (e.key === '/' && !/input|textarea/i.test(document.activeElement.tagName) && box.classList.contains('ready')) {
      e.preventDefault();
      input.focus();
    }
  });
})();
