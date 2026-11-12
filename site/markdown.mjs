// The Markdown the documentation pages are written in, made into HTML. Only what the pages use:
// headings (each given an id, from its text or `{#id}` after it), paragraphs, lists (a level of
// nesting), code blocks, tables, block quotes, and inline `code`, **bold**, *emphasis* and
// [links](…). A line that begins with a block-level tag (`<figure>`, `<div>`…) is passed through as
// it is, up to a blank line; one that begins with `<kbd>` or another inline tag is ordinary text.

/// The tags that open a block of HTML of its own.
const RAW = /^<\/?(figure|div|p|table|details|summary|section|aside|picture|pre|ul|ol|dl|h[1-6]|hr|blockquote|nav|img|video|br)\b/i;

export const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

export function slug(text) {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, '')
    .replace(/&[a-z]+;/g, '')
    .replace(/[`*_~]/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

export function inline(text) {
  const codes = [];
  let s = text.replace(/`([^`]+)`/g, (_, code) => {
    codes.push(`<code>${esc(code)}</code>`);
    return `\u0000${codes.length - 1}\u0000`;
  });
  // (Raw tags in a line are kept: <kbd>, <br>, <a …>.)
  const tags = [];
  s = s.replace(/<\/?[a-z][^>]*>/g, (tag) => {
    tags.push(tag);
    return `\u0001${tags.length - 1}\u0001`;
  });
  s = esc(s)
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => `<a href="${href}">${label}</a>`)
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*\w])\*([^*\s][^*]*)\*(?=[^*\w]|$)/g, '$1<em>$2</em>');
  s = s.replace(/\u0001(\d+)\u0001/g, (_, i) => tags[i]);
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => codes[i]);
}

/// The HTML, and the page's second-level headings as [id, text] (for "On this page").
export function render(md) {
  const lines = md.replace(/\r\n/g, '\n').split('\n');
  const out = [];
  const headings = [];
  const used = new Set();
  let i = 0;
  const heading = (level, raw) => {
    let text = raw.trim();
    let id = null;
    const given = /\s*\{#([a-z0-9-]+)\}$/.exec(text);
    if (given) {
      id = given[1];
      text = text.slice(0, given.index);
    }
    id = id ?? slug(text);
    let unique = id;
    for (let n = 2; used.has(unique); n++) unique = `${id}-${n}`;
    used.add(unique);
    if (level === 2) headings.push([unique, text.replace(/`/g, '')]);
    return `<h${level} id="${unique}">${inline(text)}</h${level}>`;
  };
  const isBlockStart = (l) => /^(#{1,6}\s|```|>\s?|\s*[-*]\s|\s*\d+\.\s|\|)/.test(l) || RAW.test(l);
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    let m;
    if ((m = /^(#{2,4})\s+(.*)$/.exec(line))) {
      out.push(heading(m[1].length, m[2]));
      i++;
      continue;
    }
    if ((m = /^```(\S*)/.exec(line))) {
      const lang = m[1];
      const body = [];
      i++;
      while (i < lines.length && !lines[i].startsWith('```')) body.push(lines[i++]);
      i++;
      out.push(`<pre${lang ? ` data-lang="${esc(lang)}"` : ''}><code>${esc(body.join('\n'))}</code></pre>`);
      continue;
    }
    if (RAW.test(line)) {
      // Raw HTML: up to a blank line.
      const block = [];
      while (i < lines.length && lines[i].trim()) block.push(lines[i++]);
      out.push(block.join('\n'));
      continue;
    }
    if (/^>\s?/.test(line)) {
      const block = [];
      while (i < lines.length && /^>\s?/.test(lines[i])) block.push(lines[i++].replace(/^>\s?/, ''));
      out.push(`<blockquote>${render(block.join('\n')).html}</blockquote>`);
      continue;
    }
    if (line.startsWith('|')) {
      const rows = [];
      while (i < lines.length && lines[i].startsWith('|')) rows.push(lines[i++]);
      const cells = (r) => r.replace(/^\||\|$/g, '').split(/(?<!\\)\|/).map((c) => inline(c.trim().replace(/\\\|/g, '|')));
      const [head, , ...body] = rows;
      out.push(`<div class="table"><table>\n<thead><tr>${cells(head).map((c) => `<th>${c}</th>`).join('')}</tr></thead>\n<tbody>\n${body.map((r) => `<tr>${cells(r).map((c) => `<td>${c}</td>`).join('')}</tr>`).join('\n')}\n</tbody></table></div>`);
      continue;
    }
    if ((m = /^(\s*)([-*]|\d+\.)\s+/.exec(line))) {
      const ordered = /\d/.test(m[2]);
      const items = [];
      while (i < lines.length) {
        const l = lines[i];
        const item = (ordered ? /^(\d+\.)\s+(.*)$/ : /^([-*])\s+(.*)$/).exec(l);
        if (item) {
          items.push([item[2]]);
          i++;
        } else if (/^\s{2,}\S/.test(l) && items.length) {
          items[items.length - 1].push(l.replace(/^\s{2,4}/, ''));
          i++;
        } else if (!l.trim() && i + 1 < lines.length && items.length) {
          // A blank line goes on with the same list, or with what is indented under an item.
          const next = lines[i + 1];
          const sameKind = ordered ? /^\d+\.\s+/.test(next) : /^[-*]\s+/.test(next);
          if (sameKind) {
            i++;
          } else if (/^\s{2,}\S/.test(next)) {
            items[items.length - 1].push('');
            i++;
          } else break;
        } else break;
      }
      const rendered = items.map((parts) => {
        const text = parts.join('\n');
        // An item with lines that are a list or a block of their own is rendered whole.
        const nested = parts.slice(1).some((p) => /^([-*]|\d+\.)\s|^```/.test(p)) || parts.includes('');
        return `<li>${nested ? render(text).html : inline(parts.join(' '))}</li>`;
      });
      out.push(`<${ordered ? 'ol' : 'ul'}>\n${rendered.join('\n')}\n</${ordered ? 'ol' : 'ul'}>`);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !(para.length && isBlockStart(lines[i]))) para.push(lines[i++]);
    out.push(`<p>${inline(para.join(' '))}</p>`);
  }
  return { html: out.join('\n'), headings };
}
