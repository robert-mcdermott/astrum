// The manual page (doc/astrum.1) as HTML: only the few man(7) macros it uses. Sections become
// headings, `.TP` items a definition list, and the font macros bold and italic text.

import { esc, slug } from './markdown.mjs';

const SPECIAL = { eq: '≡', em: '—', en: '–', bu: '•', aq: "'", dq: '"', lq: '“', rq: '”', co: '©', mu: '×' };

/// Escapes and font changes within a line, made safe for HTML.
function text(s) {
  let out = '';
  const open = [];
  const close = () => {
    while (open.length) out += `</${open.pop()}>`;
  };
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c !== '\\') {
      out += esc(c);
      continue;
    }
    const n = s[++i];
    if (n === '-' || n === '\\' || n === '.' || n === ' ' || n === '&') out += n === '&' ? '' : esc(n === '\\' ? '\\' : n);
    else if (n === 'e') out += '\\';
    else if (n === '(') {
      const name = s.slice(i + 1, i + 3);
      i += 2;
      out += SPECIAL[name] ?? '';
    } else if (n === 'f') {
      const f = s[++i];
      close();
      if (f === 'B') {
        out += '<strong>';
        open.push('strong');
      } else if (f === 'I') {
        out += '<em>';
        open.push('em');
      }
    } else out += esc(n ?? '');
  }
  close();
  return out;
}

/// A macro's arguments, with "quoted strings" kept whole.
function args(rest) {
  const out = [];
  const re = /"((?:[^"]|"")*)"|(\S+)/g;
  let m;
  while ((m = re.exec(rest))) out.push(m[1] !== undefined ? m[1].replace(/""/g, '"') : m[2]);
  return out;
}

const FONT = { B: 'strong', I: 'em', R: null };

function font(tag, s) {
  return tag ? `<${tag}>${text(s)}</${tag}>` : text(s);
}

const title = (s) => s.charAt(0) + s.slice(1).toLowerCase();

export function manual(src) {
  const out = [];
  let para = [];
  let list = false; // inside a <dl>
  let want = null; // 'term' after .TP; 'body' once the term is written
  let link = null;
  const flush = () => {
    if (!para.length) return;
    const html = para.join(' ');
    para = [];
    if (want === 'term') {
      out.push(`<dt>${html}</dt>`);
      want = 'body';
    } else if (want === 'body') {
      out.push(`<dd>${html}</dd>`);
      want = null;
    } else out.push(`<p>${html}</p>`);
  };
  const endList = () => {
    flush();
    if (want === 'body') want = null;
    if (list) out.push('</dl>');
    list = false;
  };
  const add = (html) => {
    para.push(html);
    // The term of a .TP item is one line.
    if (want === 'term') flush();
  };
  for (const raw of src.replace(/\r\n/g, '\n').split('\n')) {
    if (raw.startsWith('.\\"') || raw.startsWith("'\\\"")) continue;
    if (!raw.startsWith('.')) {
      if (raw.trim()) add(text(raw));
      continue;
    }
    const [, name, rest = ''] = /^\.(\S+)\s*(.*)$/.exec(raw) ?? [];
    const a = args(rest);
    switch (name) {
      case 'TH':
        break;
      case 'SH':
      case 'SS': {
        endList();
        const t = title(a.join(' '));
        const level = name === 'SH' ? 2 : 3;
        out.push(`<h${level} id="${slug(t)}">${esc(t)}</h${level}>`);
        break;
      }
      case 'PP':
      case 'LP':
      case 'P':
        endList();
        break;
      case 'TP':
        flush();
        if (!list) out.push('<dl class="manual">');
        list = true;
        want = 'term';
        break;
      case 'B':
      case 'I':
        add(font(FONT[name], a.join(' ')));
        break;
      case 'BR':
      case 'RB':
      case 'BI':
      case 'IB':
      case 'IR':
      case 'RI':
        add(a.map((s, i) => font(FONT[name[i % 2]], s)).join(''));
        break;
      case 'UR':
        link = a[0];
        para.push(`<a href="${esc(link)}">`);
        break;
      case 'UE':
        para[para.length - 1] += `</a>${text(a.join(' '))}`;
        link = null;
        break;
      default:
        throw new Error(`the manual page uses .${name}, which site/roff.mjs does not know`);
    }
  }
  endList();
  // (An <a> opened by .UR is joined to the following words without a space.)
  return out.join('\n').replace(/<a href="([^"]*)"> /g, '<a href="$1">');
}
