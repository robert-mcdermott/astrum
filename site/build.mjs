#!/usr/bin/env node
// Builds the Astrum website from site/pages, and checks it. See site/README.md.
//
//   node site/build.mjs                 build into target/site
//   node site/build.mjs --out DIR       build into DIR
//   node site/build.mjs --serve         build, then serve it at http://127.0.0.1:8977/astrum/ (--port N)
//   node site/build.mjs --publish       build into ../astrum-site, the gh-pages worktree
//   --astrum PATH                       the Astrum binary to ask (default: target/release/astrum)

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, posix, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { esc, render, slug } from './markdown.mjs';
import { manual } from './roff.mjs';

const SITE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(SITE, '..');
const URL_BASE = 'https://robert-mcdermott.github.io/astrum/';
const PATH_BASE = '/astrum/';
const GITHUB = 'https://github.com/robert-mcdermott/astrum';
const INSTALL = "curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/robert-mcdermott/astrum/main/install.sh | sh";
const MARKER = '.astrum-site-build';

// ---- what was asked ---------------------------------------------------------------

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const option = (name) => {
  const at = argv.indexOf(name);
  return at >= 0 ? argv[at + 1] : undefined;
};
for (const a of argv) {
  if (a.startsWith('--') && !['--out', '--serve', '--publish', '--astrum', '--port'].includes(a)) fail(`unknown option ${a}`);
}

function fail(why) {
  console.error(`site: ${why}`);
  process.exit(1);
}

const publish = flag('--publish');
const out = resolve(publish ? join(REPO, '..', 'astrum-site') : (option('--out') ?? join(REPO, 'target', 'site')));

// ---- what Astrum says -------------------------------------------------------------

function astrumData() {
  const bin = resolve(option('--astrum') ?? join(REPO, 'target', 'release', 'astrum'));
  if (!existsSync(bin)) fail(`there is no ${relative(REPO, bin)}: run \`cargo build --release\` first, or name one with --astrum`);
  // A sandbox of its own, so nothing of the user's settings shows in the lists.
  const t = mkdtempSync(join(tmpdir(), 'astrum-site-'));
  const env = {
    ...process.env,
    HOME: t,
    ASTRUM_CONFIG_DIR: join(t, 'config'),
    ASTRUM_DATA_DIR: join(t, 'data'),
    ASTRUM_RUNTIME_DIR: join(t, 'run'),
    ASTRUM_KEYCHAIN_FILE: join(t, 'keychain'),
    ASTRUM_SERVICE_MANAGER: 'none',
    ASTRUM_CLIPBOARD_TOOL: 'none',
    ASTRUM_OPEN_TOOL: 'none',
  };
  mkdirSync(env.ASTRUM_CONFIG_DIR);
  const run = (...args) => execFileSync(bin, args, { env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  try {
    const version = run('--version').trim().split(/\s+/).pop();
    const keys = JSON.parse(run('keys', '--json'));
    const themes = JSON.parse(run('themes', '--json'));
    run('config', '--init');
    const sample = readFileSync(join(env.ASTRUM_CONFIG_DIR, 'config.toml'), 'utf8');
    const man = run('man');
    return { version, keys, themes, sample, man };
  } finally {
    rmSync(t, { recursive: true, force: true });
  }
}

// ---- the pages ------------------------------------------------------------------

/// A page's front matter (`key: value` lines between `---`) and the rest.
function frontMatter(text, file) {
  const m = /^---\n([\s\S]*?)\n---\n/.exec(text.replace(/\r\n/g, '\n'));
  if (!m) fail(`${file} has no front matter (a title and a description between --- lines)`);
  const meta = {};
  for (const line of m[1].split('\n')) {
    const kv = /^([a-z_]+):\s*(.*)$/.exec(line);
    if (kv) meta[kv[1]] = kv[2].trim();
  }
  for (const key of ['title', 'description']) if (!meta[key]) fail(`${file} has no ${key}`);
  return { meta, body: text.replace(/\r\n/g, '\n').slice(m[0].length) };
}

const ICON = {
  search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>',
  sun: '<svg class="icon-sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="4.5"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>',
  moon: '<svg class="icon-moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5Z"/></svg>',
  menu: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16"/></svg>',
};

function layout({ path, title, description, body, current, root, data, noindex = false, hash }) {
  const full = path === 'index.html' ? 'Astrum: a terminal that keeps your sessions, with an agent beside you' : `${title} · Astrum`;
  const canonical = URL_BASE + (path === 'index.html' ? '' : path);
  const nav = [
    ['docs', 'docs/index.html', 'Documentation'],
    ['install', 'docs/install.html', 'Install'],
    ['changelog', 'docs/changelog.html', 'Changelog'],
  ]
    .map(([key, href, label]) => `<a href="${root}${href}"${current === key ? ' aria-current="page"' : ''}>${label}</a>`)
    .join('');
  const v = (file) => `${root}assets/${file}?v=${hash[file]}`;
  return `<!doctype html>
<html lang="en" data-root="${root}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(full)}</title>
<meta name="description" content="${esc(description)}">
<meta name="color-scheme" content="light dark">
<meta name="theme-color" content="#f6f7fc" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#0b0c16" media="(prefers-color-scheme: dark)">
${noindex ? '<meta name="robots" content="noindex">' : `<link rel="canonical" href="${canonical}">`}
<meta property="og:type" content="website">
<meta property="og:site_name" content="Astrum">
<meta property="og:title" content="${esc(path === 'index.html' ? 'Astrum' : title)}">
<meta property="og:description" content="${esc(description)}">
<meta property="og:url" content="${canonical}">
<meta property="og:image" content="${URL_BASE}images/og.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<link rel="icon" href="${root}assets/logo/astrum-logo.svg" type="image/svg+xml">
<link rel="icon" href="${root}assets/logo/astrum-logo-32.png" sizes="32x32" type="image/png">
<link rel="apple-touch-icon" href="${root}assets/logo/astrum-logo-256.png">
<link rel="stylesheet" href="${v('site.css')}">
<script>try{var t=localStorage.getItem('astrum-theme');if(t==='light'||t==='dark')document.documentElement.dataset.theme=t}catch(e){}</script>
<script src="${v('site.js')}" defer></script>
</head>
<body>
<a class="skip-link" href="#main">Skip to the content</a>
<header class="site-header">
<div class="container">
<a class="brand" href="${root}index.html" aria-label="Astrum, the front page"><img class="on-light" src="${root}assets/logo/astrum-wordmark-on-light.svg" alt="" width="148" height="40"><img class="on-dark" src="${root}assets/logo/astrum-wordmark-on-dark.svg" alt="" width="148" height="40"></a>
<nav class="site-nav" id="site-nav" aria-label="The site">${nav}<a href="${GITHUB}">GitHub</a></nav>
<div class="header-actions">
<div class="search" role="search"${data ? '' : ' hidden'}>
${ICON.search}
<input type="search" id="search" placeholder="Search the documentation" aria-label="Search the documentation" autocomplete="off" aria-controls="search-results" aria-expanded="false">
<ul class="search-results" id="search-results" role="listbox" aria-label="What was found" hidden></ul>
</div>
<button class="icon-button theme-toggle" type="button" aria-label="Switch between light and dark">${ICON.moon}${ICON.sun}</button>
<button class="icon-button nav-toggle" type="button" aria-label="The site's menu" aria-expanded="false" aria-controls="site-nav">${ICON.menu}</button>
</div>
</div>
</header>
${body}
<footer class="site-footer">
<div class="container">
<div class="footer-grid">
<div class="footer-about">
<img class="on-light" src="${root}assets/logo/astrum-wordmark-on-light.svg" alt="Astrum" width="148" height="40" style="height:28px;width:auto">
<p>A persistent terminal with tabs, mouse support and a built-in AI agent. Open source, under the Apache License 2.0.</p>
</div>
<div><p class="menu-title">Using Astrum</p><ul><li><a href="${root}docs/getting-started.html">Getting started</a></li><li><a href="${root}docs/tabs-and-panes.html">Tabs and panes</a></li><li><a href="${root}docs/mouse-and-keys.html">The mouse and the keys</a></li><li><a href="${root}docs/settings.html">Settings</a></li></ul></div>
<div><p class="menu-title">The agent</p><ul><li><a href="${root}docs/agent.html">The agent</a></li><li><a href="${root}docs/providers.html">Providers and models</a></li><li><a href="${root}docs/mcp.html">MCP servers</a></li><li><a href="${root}docs/several-agents.html">Several agents at once</a></li></ul></div>
<div><p class="menu-title">The project</p><ul><li><a href="${GITHUB}">Source code</a></li><li><a href="${GITHUB}/releases">Releases</a></li><li><a href="${GITHUB}/issues">Report a problem</a></li><li><a href="${root}docs/changelog.html">Changelog</a></li></ul></div>
</div>
<div class="footer-bottom">Astrum ${esc(data?.version ?? '')} · <a href="${GITHUB}/blob/main/LICENSE">Apache License 2.0</a> · The screenshots on this site were made by Astrum itself.</div>
</div>
</footer>
</body>
</html>
`;
}

// ---- what Astrum makes ----------------------------------------------------------

const keyWord = { left: '←', right: '→', up: '↑', down: '↓', pageup: 'PageUp', pagedown: 'PageDown', space: 'Space', tab: 'Tab', enter: 'Enter', esc: 'Esc', backspace: 'Backspace', delete: 'Delete', home: 'Home', end: 'End', backtab: 'Shift+Tab' };

/// A key as it is printed on the keyboard: `ctrl+b` is Ctrl+B.
function keyName(spec) {
  const parts = spec.split('+');
  const mods = parts.length > 1;
  return parts
    .map((p, i) => {
      if (keyWord[p]) return keyWord[p];
      if (i < parts.length - 1 || p.length > 1) return p.charAt(0).toUpperCase() + p.slice(1);
      return mods ? p.toUpperCase() : p;
    })
    .join('+');
}

function keysTable({ prefix, actions }) {
  const kbd = (k) => `<kbd>${esc(k)}</kbd>`;
  const one = (spec) => (/(^|\+)(ctrl|alt)\+/i.test(spec) ? kbd(keyName(spec)) : `${kbd(keyName(prefix))} ${kbd(keyName(spec))}`);
  const rows = actions.map((a) => {
    const what = a.what.charAt(0).toUpperCase() + a.what.slice(1);
    const keys = a.keys.length ? a.keys.map(one).join(' or ') : 'none';
    return `<tr><td>${esc(what)}</td><td>${keys}</td><td><code>${esc(a.name)}</code></td></tr>`;
  });
  return `<div class="table"><table>\n<thead><tr><th>What it does</th><th>Keys</th><th>Its name in <code>[keys]</code></th></tr></thead>\n<tbody>\n${rows.join('\n')}\n</tbody></table></div>`;
}

const codeText = (s) => esc(s).replace(/`([^`]+)`/g, '<code>$1</code>');
const sentence = (s) => {
  const t = s.trim();
  if (!t) return t;
  return t.charAt(0).toUpperCase() + t.slice(1) + (/[.:)]$/.test(t) ? '' : '.');
};

/// The settings in the starter file: those at the top, those under [agent], and the tables
/// of their own, each with its default and what its comment says it does.
export function parseSample(sample) {
  const top = [];
  const agent = [];
  const tables = [];
  let section = 'top';
  let prose = [];
  let afterKey = false;
  let last = null;
  const value = (s) => {
    // A value: a string, a list, or a word; what follows it may be a comment.
    let end;
    if (s.startsWith('"')) end = s.indexOf('"', 1) + 1;
    else if (s.startsWith('[')) end = s.indexOf(']') + 1;
    else end = s.search(/\s|$/);
    const rest = s.slice(end).trim();
    return [s.slice(0, end), rest.startsWith('#') ? rest.replace(/^#\s*/, '') : ''];
  };
  for (const line of sample.split('\n')) {
    if (!line.trim()) {
      prose = [];
      afterKey = false;
      last = null;
      continue;
    }
    // A comment that goes on from the line above.
    if (/^\s+#/.test(line) && last) {
      last.comment += ' ' + line.replace(/^\s+#\s*/, '');
      continue;
    }
    const header = /^(#\s*)?\[([^\]]+)\]/.exec(line);
    if (header) {
      last = null;
      if (!header[1] && header[2] === 'agent') {
        section = 'agent';
      } else {
        section = 'table';
        const name = header[2].split('.')[0];
        if (!tables.some((t) => t.name === name)) tables.push({ name, given: header[2], about: prose.join(' ') });
      }
      prose = [];
      continue;
    }
    const kv = /^(#\s)?([a-z_]+)\s*=\s*(.+)$/.exec(line);
    if (kv && section !== 'table') {
      const [val, comment] = value(kv[3]);
      const setting = { name: kv[2], set: !kv[1], value: val, comment, about: prose.join(' ') };
      (section === 'agent' ? agent : top).push(setting);
      last = setting;
      afterKey = true;
      continue;
    }
    last = null;
    if (line.startsWith('#')) {
      if (afterKey) {
        prose = [];
        afterKey = false;
      }
      const text = line.replace(/^#\s?/, '');
      if (text) prose.push(text);
    }
  }
  return { top, agent, tables };
}

const TABLES = {
  themes: ['[themes.<name>]', 'themes.html#a-theme-of-your-own'],
  keys: ['[keys]', 'mouse-and-keys.html'],
  providers: ['[providers.<name>]', 'providers.html'],
  models: ['[models."<id>"]', 'providers.html'],
  mcp: ['[mcp.<name>]', 'mcp.html'],
};

function settingsTables(sample, problems) {
  const { top, agent, tables } = parseSample(sample);
  const row = (s) => {
    let comment = s.comment || '';
    let given = null;
    comment = comment.replace(/\s*\(default: ([^)]+)\)/, (_, d) => ((given = d), ''));
    const parts = comment.split(/;\s+/).filter((p) => {
      const d = /^defaults?(?::| to)\s*(.+)$/.exec(p);
      if (d) given = d[1];
      return !d;
    });
    const about = parts.join('; ') || s.about;
    if (!about) problems.push(`the setting \`${s.name}\` has no comment in the starter file (src/config.rs, SAMPLE)`);
    let what = codeText(sentence(about));
    let fallback;
    if (s.set) fallback = `<code>${esc(s.value)}</code>`;
    else if (given) fallback = codeText(given);
    else {
      fallback = 'not set';
      what += ` For example <code>${esc(s.value)}</code>.`;
    }
    return `<tr><td><code>${esc(s.name)}</code></td><td>${fallback}</td><td>${what}</td></tr>`;
  };
  const table = (rows) => `<div class="table"><table>\n<thead><tr><th>Setting</th><th>Default</th><th>What it does</th></tr></thead>\n<tbody>\n${rows.map(row).join('\n')}\n</tbody></table></div>`;
  const others = tables.map((t) => {
    const known = TABLES[t.name];
    if (!known) {
      problems.push(`the starter file has a table [${t.name}] that site/build.mjs does not know where to link`);
      return '';
    }
    return `<tr><td><code>${esc(known[0])}</code></td><td>${codeText(sentence(t.about))} <a href="${known[1]}">More</a></td></tr>`;
  });
  return [
    '<h3 id="settings-top">At the top of the file</h3>',
    table(top),
    '<h3 id="settings-agent">Under <code>[agent]</code></h3>',
    table(agent),
    '<h3 id="settings-tables">Tables of their own</h3>',
    `<div class="table"><table>\n<thead><tr><th>Table</th><th>What it holds</th></tr></thead>\n<tbody>\n${others.join('\n')}\n</tbody></table></div>`,
  ].join('\n');
}

function themeCards(themes) {
  const cards = themes.map((t) => {
    const c = t.colors;
    if (!c.bg) {
      return `<div class="theme-card plain"><div class="bar"><span>1 zsh</span><span>≡ menu</span></div><div class="screen"><div>~/code ❯ ls</div><div>src  README.md  Cargo.toml</div><div>Your terminal's own colors, as they are.</div></div><div class="name">${esc(t.label)} <code>${esc(t.name)}</code></div></div>`;
    }
    const s = (color) => `color:${color}`;
    const a = t.ansi;
    const swatches = a.filter(Boolean).length === 16 ? `<div class="swatches" aria-hidden="true">${a.map((x) => `<span style="background:${x}"></span>`).join('')}</div>` : '';
    return `<div class="theme-card"><div class="bar" style="background:${c.bar};${s(c.subtext)}"><span><span style="${s(c.accent)}">●</span> <span style="${s(c.text)}">1 zsh</span>  2 logs</span><span style="${s(c.accent)}">≡ menu</span></div><div class="screen" style="background:${c.term_bg};${s(c.term_fg)}"><div><span style="${s(a[2] ?? c.green)}">~/code</span> <span style="${s(a[5] ?? c.accent)}">❯</span> ls</div><div><span style="${s(a[4] ?? c.blue)}">src</span>  README.md  <span style="${s(a[3] ?? c.yellow)}">Cargo.toml</span></div><div><span style="${s(a[1] ?? c.red)}">error:</span> not found</div>${swatches}</div><div class="name">${esc(t.label)} <code>${esc(t.name)}</code></div></div>`;
  });
  return `<div class="themes">\n${cards.join('\n')}\n</div>`;
}

function changelogHtml() {
  const text = readFileSync(join(REPO, 'CHANGELOG.md'), 'utf8');
  const from = text.search(/^## /m);
  // Links to the repository's own files go to GitHub.
  const md = text.slice(from).replace(/\]\((?!https?:|#|mailto:)([^)]+)\)/g, `](${GITHUB}/blob/main/$1)`);
  return render(md).html;
}

function installBox() {
  return `<div class="install-box"><code><span class="prompt" aria-hidden="true">$ </span>${esc(INSTALL)}</code><button class="copy" type="button" data-copy="${esc(INSTALL)}">Copy</button></div>`;
}

/// The width and height of a PNG or WebP picture.
function imageSize(file) {
  const b = readFileSync(file);
  if (b.readUInt32BE(0) === 0x89504e47) return [b.readUInt32BE(16), b.readUInt32BE(20)];
  if (b.toString('ascii', 0, 4) === 'RIFF' && b.toString('ascii', 8, 12) === 'WEBP') {
    const kind = b.toString('ascii', 12, 16);
    if (kind === 'VP8X') return [1 + b.readUIntLE(24, 3), 1 + b.readUIntLE(27, 3)];
    if (kind === 'VP8 ') return [b.readUInt16LE(26) & 0x3fff, b.readUInt16LE(28) & 0x3fff];
    if (kind === 'VP8L') {
      const bits = b.readUInt32LE(21);
      return [1 + (bits & 0x3fff), 1 + ((bits >> 14) & 0x3fff)];
    }
  }
  throw new Error(`${file} is neither PNG nor WebP`);
}

function shot(name, alt, caption, root, problems, eager = false) {
  const pics = ['dark', 'light'].map((kind) => {
    const file = join(SITE, 'images', `${name}-${kind}.webp`);
    if (!existsSync(file)) {
      problems.push(`there is no screenshot ${relative(REPO, file)}: take it with \`node site/capture/capture.mjs --only ${name}\``);
      return '';
    }
    const [w, h] = imageSize(file);
    return `<img class="shot-${kind}" src="${root}images/${name}-${kind}.webp" width="${w}" height="${h}" alt="${esc(alt)}"${eager ? ' fetchpriority="high"' : ' loading="lazy"'} decoding="async">`;
  });
  return `<figure><div class="shot">${pics.join('')}</div>${caption ? `<figcaption>${esc(caption)}</figcaption>` : ''}</figure>`;
}

/// The comments in a code block, dimmed.
function highlight(html) {
  return html.replace(/<pre data-lang="(toml|bash|sh)"><code>([\s\S]*?)<\/code><\/pre>/g, (_, lang, code) => {
    const lines = code.split('\n').map((line) => {
      if (lang !== 'toml') return /^\s*#/.test(line) ? `<span class="comment">${line}</span>` : line;
      let inString = false;
      const s = line.replace(/&quot;/g, '\u0002');
      for (let i = 0; i < s.length; i++) {
        if (s[i] === '\u0002') inString = !inString;
        else if (s[i] === '#' && !inString) return (s.slice(0, i) + `<span class="comment">${s.slice(i)}</span>`).replace(/\u0002/g, '&quot;');
      }
      return line;
    });
    return `<pre data-lang="${lang}"><code>${lines.join('\n')}</code></pre>`;
  });
}

/// A page's Markdown, with what Astrum makes put in.
function expand(md, file, ctx) {
  const shots = md.replace(/^\{\{shot ([^}]+)\}\}\s*$/gm, (_, inner) => {
    const [name, alt, caption] = inner.split('|').map((s) => s.trim());
    if (!alt) ctx.problems.push(`${file}: the screenshot ${name} has no description`);
    ctx.shotsUsed.push(name);
    return `\n${shot(name, alt ?? '', caption ?? '', ctx.root, ctx.problems)}\n`;
  });
  let html = render(shots).html;
  const blocks = {
    keys: () => keysTable(ctx.data.keys),
    settings: () => settingsTables(ctx.data.sample, ctx.problems),
    sample: () => `<pre data-lang="toml"><code>${esc(ctx.data.sample.trimEnd())}</code></pre>`,
    themes: () => themeCards(ctx.data.themes),
    manual: () => manual(ctx.data.man),
    changelog: () => changelogHtml(),
    install: () => installBox(),
  };
  html = html.replace(/<p>\{\{([a-z]+)\}\}<\/p>/g, (whole, name) => {
    if (!blocks[name]) {
      ctx.problems.push(`${file}: there is no {{${name}}}`);
      return whole;
    }
    return blocks[name]();
  });
  html = html.replace(/\{\{version\}\}/g, esc(ctx.data.version));
  for (const left of html.match(/\{\{[^}]*\}\}/g) ?? []) ctx.problems.push(`${file}: ${left} is not something the build knows`);
  return highlight(html);
}

/// The second-level headings of a page, for "On this page".
const headingsOf = (html) => [...html.matchAll(/<h2 id="([^"]+)">([\s\S]*?)<\/h2>/g)].map((m) => [m[1], m[2].replace(/<[^>]+>/g, '')]);

function tocHtml(headings) {
  if (headings.length < 2) return '<aside class="toc" aria-hidden="true"></aside>';
  return `<aside class="toc"><p class="menu-title" id="toc-title">On this page</p><nav aria-labelledby="toc-title"><ul>${headings.map(([id, t]) => `<li><a href="#${id}">${t}</a></li>`).join('')}</ul></nav></aside>`;
}

function sidebar(menu, pages, current) {
  const sections = menu
    .map((s) => `<p class="menu-title">${esc(s.section)}</p><ul>${s.pages.map((p) => `<li><a href="${p}.html"${p === current ? ' aria-current="page"' : ''}>${esc(pages.get(p).meta.title)}</a></li>`).join('')}</ul>`)
    .join('');
  return `<aside class="docs-sidebar"><button class="btn docs-menu-toggle" type="button" aria-expanded="false" aria-controls="docs-nav">${ICON.menu} The documentation</button><nav id="docs-nav" aria-label="The documentation"><p class="menu-title"><a href="index.html"${current === 'index' ? ' aria-current="page"' : ''}>Overview</a></p>${sections}</nav></aside>`;
}

// ---- checks ---------------------------------------------------------------------

function luminance(hex) {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a, b) {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

/// A color token as #rrggbb, an rgba() one laid over `under`.
function solid(value, under) {
  if (/^#[0-9a-f]{6}$/i.test(value)) return value;
  const m = /rgba\((\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\)/.exec(value);
  if (!m || !under) return null;
  const alpha = Number(m[4]);
  const base = [1, 3, 5].map((i) => parseInt(under.slice(i, i + 2), 16));
  return '#' + [1, 2, 3].map((i, k) => Math.round(Number(m[i]) * alpha + base[k] * (1 - alpha)).toString(16).padStart(2, '0')).join('');
}

/// Text in the site's own colors must be easy to read (WCAG AA: 4.5 to 1), light and dark.
function checkColors(css, problems) {
  const block = (selector) => {
    const at = css.indexOf(selector);
    const body = css.slice(css.indexOf('{', at) + 1, css.indexOf('}', at));
    return Object.fromEntries([...body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]));
  };
  const light = block(':root {');
  const dark = { ...light, ...block(':root[data-theme="dark"] {') };
  const pairs = [
    ['text', 'bg'], ['text-dim', 'bg'], ['text-faint', 'bg'], ['text-dim', 'bg-soft'], ['text-faint', 'bg-soft'], ['text', 'surface'],
    ['text-dim', 'surface'], ['text-faint', 'surface'], ['text-faint', 'surface-2'], ['accent', 'bg'], ['accent', 'surface'], ['accent', 'bg-soft'],
    ['accent2', 'bg'], ['accent2', 'bg-soft'], ['code-text', 'code-bg'], ['code-dim', 'code-bg'], ['accent', 'accent-soft'], ['text-dim', 'accent-soft'],
  ];
  for (const [name, tokens] of [['light', light], ['dark', dark]]) {
    for (const [fg, bg] of pairs) {
      const back = solid(tokens[bg], tokens.bg);
      const front = solid(tokens[fg], back);
      if (!front || !back) {
        problems.push(`site.css: cannot read --${fg} or --${bg} (${name})`);
        continue;
      }
      const ratio = contrast(front, back);
      if (ratio < 4.5) problems.push(`site.css: --${fg} on --${bg} is ${ratio.toFixed(2)} to 1 in the ${name} colors; text needs 4.5`);
    }
    for (const stop of (tokens['spectrum-button'] ?? '').match(/#[0-9a-f]{6}/gi) ?? []) {
      const ratio = contrast('#0b0c16', stop);
      if (ratio < 4.5) problems.push(`site.css: a button's text on ${stop} is ${ratio.toFixed(2)} to 1; it needs 4.5`);
    }
  }
}

/// Every page: its links, anchors and pictures lead somewhere on the site, nothing is loaded
/// from elsewhere, and the basics of accessibility are there.
function checkPages(files, problems) {
  const ids = new Map();
  for (const [path, content] of files) {
    if (path.endsWith('.html')) ids.set(path, new Set([...String(content).matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
  }
  for (const [path, content] of files) {
    if (!path.endsWith('.html')) continue;
    const html = String(content);
    const where = (what) => problems.push(`${path}: ${what}`);
    const seen = new Set();
    for (const m of html.matchAll(/\sid="([^"]+)"/g)) {
      if (seen.has(m[1])) where(`the id "${m[1]}" is used twice`);
      seen.add(m[1]);
    }
    if (!/<html lang="en"/.test(html)) where('no lang');
    if (!/<title>[^<]+<\/title>/.test(html)) where('no title');
    const h1 = html.match(/<h1[\s>]/g)?.length ?? 0;
    if (h1 !== 1) where(`${h1} main headings (<h1>), not one`);
    let level = 1;
    for (const m of html.matchAll(/<h([1-6])[\s>]/g)) {
      const l = Number(m[1]);
      if (l > level + 1) where(`a heading skips from <h${level}> to <h${l}>`);
      level = l;
    }
    for (const m of html.matchAll(/<img\b[^>]*>/g)) if (!/\salt="/.test(m[0])) where(`a picture has no alt: ${m[0].slice(0, 80)}`);
    for (const m of html.matchAll(/<button\b[^>]*>([\s\S]*?)<\/button>/g)) {
      if (!/aria-label="[^"]+"/.test(m[0]) && !m[1].replace(/<svg[\s\S]*?<\/svg>/g, '').replace(/<[^>]+>/g, '').trim()) where('a button has no name');
    }
    for (const m of html.matchAll(/<(script|img|link|source|iframe)\b[^>]*\s(?:src|href)="(https?:)?\/\/[^"]*"/g)) {
      if (!/rel="canonical"/.test(m[0])) where(`loads something from elsewhere: ${m[0].slice(0, 100)}`);
    }
    const dir = posix.dirname(path);
    for (const m of html.matchAll(/\s(href|src)="([^"]*)"/g)) {
      let url = m[2].replace(/&amp;/g, '&');
      if (/^(https?:|mailto:)/.test(url)) continue;
      if (url === '') {
        where(`an empty ${m[1]}`);
        continue;
      }
      let [target, anchor] = url.split('#');
      target = target.split('?')[0];
      let file;
      if (!target) file = path;
      else if (target.startsWith(PATH_BASE)) file = target.slice(PATH_BASE.length);
      else if (target.startsWith('/')) {
        where(`a link outside the site: ${url}`);
        continue;
      } else file = posix.normalize(posix.join(dir, target));
      if (file === '' || file.endsWith('/')) file += 'index.html';
      if (!files.has(file)) {
        where(`leads nowhere: ${url}`);
        continue;
      }
      if (anchor && !ids.get(file)?.has(anchor)) where(`no #${anchor} on ${file}: ${url}`);
    }
  }
}

// ---- the build ------------------------------------------------------------------

function listFiles(dir, base = dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = join(dir, e.name);
    if (e.name.startsWith('.')) return [];
    return e.isDirectory() ? listFiles(p, base) : [relative(base, p).split('\\').join('/')];
  });
}

function textOf(html) {
  return html
    .replace(/<(script|style)[\s\S]*?<\/\1>/g, ' ')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&nbsp;/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/\s+/g, ' ')
    .trim();
}

function build() {
  const problems = [];
  const data = astrumData();
  const files = new Map();
  const hash = {};
  for (const file of listFiles(join(SITE, 'assets'))) {
    const bytes = readFileSync(join(SITE, 'assets', file));
    files.set(`assets/${file}`, bytes);
    hash[file] = createHash('sha256').update(bytes).digest('hex').slice(0, 10);
  }
  for (const file of listFiles(join(SITE, 'images'))) files.set(`images/${file}`, readFileSync(join(SITE, 'images', file)));
  if (!files.has('images/og.png')) problems.push('there is no images/og.png (the picture shown when a link is shared): run site/capture/capture.mjs');
  checkColors(String(files.get('assets/site.css')), problems);

  // The README and this site name the same one-line install.
  if (!readFileSync(join(REPO, 'README.md'), 'utf8').includes(INSTALL)) problems.push('README.md does not have the one-line install that site/build.mjs shows');

  const menu = JSON.parse(readFileSync(join(SITE, 'menu.json'), 'utf8'));
  const order = menu.flatMap((s) => s.pages);
  const pages = new Map();
  const docsDir = join(SITE, 'pages', 'docs');
  for (const file of readdirSync(docsDir).filter((f) => f.endsWith('.md'))) {
    const name = file.slice(0, -3);
    pages.set(name, frontMatter(readFileSync(join(docsDir, file), 'utf8'), `pages/docs/${file}`));
    if (!order.includes(name)) problems.push(`pages/docs/${file} is not in site/menu.json`);
  }
  for (const name of order) if (!pages.has(name)) fail(`site/menu.json names ${name}, but there is no pages/docs/${name}.md`);

  const search = [];
  const shotsUsed = [];
  const current = (name) => (name === 'install' ? 'install' : name === 'changelog' ? 'changelog' : 'docs');
  order.forEach((name, i) => {
    const { meta, body } = pages.get(name);
    const ctx = { root: '../', data, problems, shotsUsed };
    const html = expand(body, `pages/docs/${name}.md`, ctx);
    const headings = headingsOf(html);
    const prev = order[i - 1];
    const next = order[i + 1];
    const pager = `<nav class="doc-pager" aria-label="The pages before and after">${prev ? `<a class="prev" href="${prev}.html"><span>Before</span>${esc(pages.get(prev).meta.title)}</a>` : ''}${next ? `<a class="next" href="${next}.html"><span>Next</span>${esc(pages.get(next).meta.title)}</a>` : ''}</nav>`;
    const main = `<main class="doc" id="main">\n<h1>${esc(meta.title)}</h1>\n<p class="doc-lede">${esc(meta.description)}</p>\n${html}\n${pager}\n</main>`;
    const body2 = `<div class="docs-layout">${sidebar(menu, pages, name)}\n${main}\n${tocHtml(headings)}</div>`;
    const path = `docs/${name}.html`;
    files.set(path, layout({ path, title: meta.title, description: meta.description, body: body2, current: current(name), root: '../', data, hash }));
    // For the search: the page, and each of its sections.
    const parts = html.split(/(?=<h2 id=")/);
    parts.forEach((part, k) => {
      const h = /^<h2 id="([^"]+)">([\s\S]*?)<\/h2>/.exec(part);
      const text = textOf(part.replace(/^<h2[\s\S]*?<\/h2>/, ''));
      if (!h && !text) return;
      search.push({ t: meta.title, h: h ? textOf(h[2]) : '', u: `docs/${name}.html${h ? `#${h[1]}` : ''}`, x: k === 0 && !h ? `${meta.description} ${text}`.slice(0, 1500) : text.slice(0, 1500) });
    });
  });

  // The documentation's first page: every page, by section.
  const overview = menu
    .map((s) => `<h2 id="${slug(s.section)}">${esc(s.section)}</h2>\n<div class="cards">${s.pages.map((p) => `<a class="card link-card" href="${p}.html"><span class="card-title">${esc(pages.get(p).meta.title)}</span><p>${esc(pages.get(p).meta.description)}</p></a>`).join('')}</div>`)
    .join('\n');
  const docsMain = `<main class="doc" id="main">\n<h1>Documentation</h1>\n<p class="doc-lede">Everything Astrum does and how to use it, a page per subject. New to it? Start with <a href="getting-started.html">getting started</a>.</p>\n${overview}\n</main>`;
  files.set('docs/index.html', layout({ path: 'docs/index.html', title: 'Documentation', description: 'The full user documentation for Astrum: tabs and panes, sessions, the agent, settings and themes.', body: `<div class="docs-layout">${sidebar(menu, pages, 'index')}\n${docsMain}\n${tocHtml(headingsOf(overview))}</div>`, current: 'docs', root: '../', data, hash }));

  for (const special of ['index', '404']) {
    const { meta, body } = frontMatter(readFileSync(join(SITE, 'pages', `${special}.html`), 'utf8'), `pages/${special}.html`);
    const root = special === '404' ? PATH_BASE : '';
    let html = body
      .replace(/\{\{shot ([^}]+)\}\}/g, (_, inner) => {
        const [name, alt, caption] = inner.split('|').map((s) => s.trim());
        shotsUsed.push(name);
        return shot(name, alt ?? '', caption ?? '', root, problems, name === 'hero');
      })
      .replace(/\{\{install\}\}/g, installBox())
      .replace(/\{\{version\}\}/g, esc(data.version))
      .replace(/\{\{root\}\}/g, root)
      .replace(/\{\{themes-count\}\}/g, String(data.themes.filter((t) => !t.flat).length));
    for (const left of html.match(/\{\{[^}]*\}\}/g) ?? []) problems.push(`pages/${special}.html: ${left} is not something the build knows`);
    const path = `${special}.html`;
    files.set(path, layout({ path, title: meta.title, description: meta.description, body: html, current: special === 'index' ? 'home' : '', root, data, noindex: special === '404', hash }));
  }

  // Screenshots that no page shows are left out.
  for (const file of [...files.keys()].filter((f) => /^images\/.+-(light|dark)\.webp$/.test(f))) {
    const name = file.slice(7).replace(/-(light|dark)\.webp$/, '');
    if (!shotsUsed.includes(name)) files.delete(file);
  }

  files.set('search.json', JSON.stringify(search));
  files.set('sitemap.xml', `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${[...files.keys()].filter((f) => f.endsWith('.html') && f !== '404.html').sort().map((f) => `<url><loc>${URL_BASE}${f === 'index.html' ? '' : f}</loc></url>`).join('\n')}\n</urlset>\n`);
  files.set('.nojekyll', '');

  checkPages(files, problems);
  return { files, problems, data };
}

// ---- where it goes ----------------------------------------------------------------

function git(dir, ...args) {
  return execFileSync('git', ['-C', dir, ...args], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

/// Only a folder this build made, an empty one, or the gh-pages worktree is emptied.
function prepare(dir) {
  if (publish) {
    if (!existsSync(join(dir, '.git'))) fail(`${dir} is not the gh-pages worktree: see "Publishing" in site/README.md`);
    let branch;
    try {
      branch = git(dir, 'symbolic-ref', '--short', 'HEAD');
      if (resolve(git(dir, 'rev-parse', '--show-toplevel')) !== resolve(dir)) fail(`${dir} is inside another repository`);
    } catch (e) {
      fail(`${dir} is not a git worktree: ${e.message}`);
    }
    if (branch !== 'gh-pages') fail(`${dir} is on the branch ${branch}, not gh-pages`);
    for (const name of readdirSync(dir)) if (name !== '.git' && name !== 'CNAME') rmSync(join(dir, name), { recursive: true, force: true });
    return;
  }
  if (existsSync(dir)) {
    const there = readdirSync(dir);
    if (there.length && !there.includes(MARKER)) fail(`${dir} has other things in it; build into an empty folder, or one this build made`);
    rmSync(dir, { recursive: true, force: true });
  }
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, MARKER), 'Made by site/build.mjs, which empties this folder each time it builds here.\n');
}

function write(dir, files) {
  for (const [path, content] of files) {
    const target = join(dir, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, content);
  }
}

function serve(dir) {
  const port = Number(option('--port') ?? 8977);
  const types = { html: 'text/html; charset=utf-8', css: 'text/css', js: 'text/javascript', json: 'application/json', svg: 'image/svg+xml', png: 'image/png', webp: 'image/webp', txt: 'text/plain; charset=utf-8', xml: 'application/xml' };
  createServer((req, res) => {
    let url;
    try {
      url = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    } catch {
      res.writeHead(400);
      return res.end();
    }
    if (!url.startsWith(PATH_BASE)) {
      res.writeHead(302, { location: PATH_BASE });
      return res.end();
    }
    let file = resolve(dir, '.' + url.slice(PATH_BASE.length - 1));
    if (file !== resolve(dir) && !file.startsWith(resolve(dir) + sep)) {
      res.writeHead(403);
      return res.end();
    }
    if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
    let status = 200;
    if (!existsSync(file)) {
      // GitHub Pages serves a file without its .html, and 404.html for what is not there.
      if (existsSync(file + '.html')) file += '.html';
      else {
        file = join(dir, '404.html');
        status = 404;
      }
    }
    res.writeHead(status, { 'content-type': types[file.split('.').pop()] ?? 'application/octet-stream', 'cache-control': 'no-store' });
    res.end(readFileSync(file));
  })
    .on('error', (e) => fail(e.code === 'EADDRINUSE' ? `port ${port} is taken; choose another with --port` : e.message))
    .listen(port, '127.0.0.1', () => console.log(`site: serving ${relative(process.cwd(), dir) || dir} at http://127.0.0.1:${port}${PATH_BASE}`));
}

const { files, problems, data } = build();
if (problems.length) {
  console.error(`site: ${problems.length} thing${problems.length === 1 ? '' : 's'} to fix:`);
  for (const p of problems) console.error(`  - ${p}`);
  process.exit(1);
}
prepare(out);
write(out, files);
const pageCount = [...files.keys()].filter((f) => f.endsWith('.html')).length;
console.log(`site: ${pageCount} pages for Astrum ${data.version}, checked, in ${relative(process.cwd(), out) || out}`);
if (flag('--serve')) serve(out);
