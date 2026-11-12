#!/usr/bin/env node
// Takes the website's screenshots: each scene in site/capture/scenes.mjs is played in a sandbox
// of its own (home, settings, data and sockets all in a temporary folder, a stand-in model, no
// key or setting of yours), drawn by Astrum's headless client, framed as a terminal window and
// photographed by headless Chrome, once in a light theme and once in a dark one.
//
//   node site/capture/capture.mjs [--only hero,panes] [--astrum target/release/astrum]

import { execFile } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { launch } from './cdp.mjs';
import { scenes, project } from './scenes.mjs';
import { standIn } from './stand-in.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..');
const IMAGES = join(REPO, 'site', 'images');
const WORK = join(REPO, 'target', 'capture');

const argv = process.argv.slice(2);
const option = (name) => {
  const at = argv.indexOf(name);
  return at >= 0 ? argv[at + 1] : undefined;
};
const bin = resolve(option('--astrum') ?? join(REPO, 'target', 'release', 'astrum'));
if (!existsSync(bin)) {
  console.error('capture: build Astrum first: cargo build --release');
  process.exit(1);
}
const only = option('--only')?.split(',');
const VARIANTS = [
  { kind: 'dark', theme: 'astrum' },
  { kind: 'light', theme: 'catppuccin-latte' },
];

const run = (file, args, opts) =>
  new Promise((ok, no) => execFile(file, args, { maxBuffer: 64 << 20, ...opts }, (err, stdout, stderr) => (err ? no(Object.assign(err, { stdout, stderr })) : ok(stdout))));

const themesDir = mkdtempSync(join(tmpdir(), 'astrum-cap-cfg-'));
const themes = JSON.parse(await run(bin, ['themes', '--json'], { env: { PATH: '/usr/bin:/bin', HOME: themesDir, ASTRUM_CONFIG_DIR: themesDir } }));
rmSync(themesDir, { recursive: true, force: true });
const colorsOf = (name) => themes.find((t) => t.name === name).colors;

/// A sandbox: a home with a project in it, and Astrum's folders, all new.
function sandbox(tag) {
  // (The real path: the shell shows ~ only for a directory under $HOME as it is spelled.)
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'astrum-cap-')));
  const home = join(root, 'home');
  const sb = {
    root,
    home,
    // (Under the home, so that what Astrum shows of them reads ~/.config/astrum.)
    cfg: join(home, '.config', 'astrum'),
    // (Socket paths have to be short.)
    runtime: `/tmp/astrum-cap-${process.pid}-${tag}`,
    project: join(home, 'projects', 'inventory'),
    write(path, text, mode) {
      const file = join(home, path);
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, text);
      if (mode) chmodSync(file, mode);
    },
  };
  mkdirSync(sb.cfg, { recursive: true });
  for (const [path, text] of Object.entries(project)) sb.write(path, text, path.startsWith('bin/') ? 0o755 : undefined);
  sb.env = {
    HOME: home,
    USER: 'ada',
    LOGNAME: 'ada',
    PATH: [join(home, 'bin'), dirname(process.execPath), '/usr/bin', '/bin', '/usr/sbin', '/sbin'].join(':'),
    SHELL: '/bin/zsh',
    TERM: 'xterm-256color',
    LANG: 'en_US.UTF-8',
    LC_ALL: 'en_US.UTF-8',
    TMPDIR: join(root, 'tmp'),
    ASTRUM_CONFIG_DIR: sb.cfg,
    ASTRUM_DATA_DIR: join(home, '.local', 'share', 'astrum'),
    ASTRUM_RUNTIME_DIR: sb.runtime,
    ASTRUM_KEYCHAIN_FILE: join(root, 'keychain.json'),
    ASTRUM_SERVICE_MANAGER: 'none',
    ASTRUM_CLIPBOARD_TOOL: 'none',
    ASTRUM_OPEN_TOOL: 'none',
    ASTRUM_NOTIFY: 'none',
  };
  mkdirSync(sb.env.TMPDIR);
  return sb;
}

async function play(scene, variant) {
  const sb = sandbox(`${scene.name.slice(0, 6)}${variant.kind[0]}`);
  const model = await standIn(scene.scripts ?? []);
  const config = [
    `theme = "${variant.theme}"`,
    scene.config?.top ?? '',
    '',
    '[agent]',
    'provider = "ollama"',
    `model = "${model.model}"`,
    `autonomy = "${scene.autonomy ?? 'ask'}"`,
    scene.config?.agent ?? '',
    '',
    '[providers.ollama]',
    'kind = "openai"',
    `base_url = "http://127.0.0.1:${model.port}/v1"`,
    '',
    scene.config?.more?.(sb) ?? '',
  ].join('\n');
  writeFileSync(join(sb.cfg, 'config.toml'), config);
  writeFileSync(join(sb.cfg, 'state.toml'), 'welcomed = true\n');
  scene.setup?.(sb);
  const html = join(sb.root, 'screen.html');
  const actions = typeof scene.actions === 'function' ? scene.actions(variant) : scene.actions;
  const args = ['debug-snapshot', '-s', 'default', '--cols', String(scene.cols ?? 150), '--rows', String(scene.rows ?? 40), '--wait-ms', '1200', ...actions.flatMap((a) => ['--do', a]), '--html', html];
  let text;
  try {
    text = await run(bin, args, { cwd: sb.project, env: sb.env, timeout: 240000 });
  } finally {
    await run(bin, ['kill', '-s', 'default', '--forget'], { env: sb.env }).catch(() => {});
    model.close();
  }
  mkdirSync(WORK, { recursive: true });
  writeFileSync(join(WORK, `${scene.name}-${variant.kind}.txt`), text);
  if (text.includes('--- gave up waiting')) throw new Error(`${scene.name} (${variant.kind}): ${text.slice(0, 4000)}`);
  const page = readFileSync(html, 'utf8');
  // (A program a scene left running may still be writing as its shell is closed.)
  for (const dir of [sb.root, sb.runtime]) {
    try {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
    } catch (e) {
      console.error(`capture: could not remove ${dir}: ${e.message}`);
    }
  }
  return page;
}

/// The screen, in a plain terminal window.
function frame(page, variant, title, theme = variant.theme) {
  const c = colorsOf(theme);
  const rows = /<pre>([\s\S]*)<\/pre>/.exec(page)[1].replace(/color:#d0d0d8/g, `color:${c.term_fg}`).replace(/background:#14141c/g, `background:${c.term_bg}`);
  const chrome = variant.kind === 'dark' ? { bg: '#1f2133', fg: '#a9aed1', dot: '#3d4160', line: '#2b2e48' } : { bg: '#dfe2ec', fg: '#4c4f69', dot: '#b4b8cc', line: '#ccd0da' };
  return `<!doctype html><meta charset="utf-8"><style>
html,body{margin:0;background:#888}
#shot{display:inline-block;background:${c.term_bg}}
.title{height:32px;display:flex;align-items:center;justify-content:center;position:relative;font:500 13px -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;color:${chrome.fg};background:${chrome.bg};border-bottom:1px solid ${chrome.line}}
.dots{position:absolute;left:14px;top:11px;display:flex;gap:8px}.dots i{display:block;width:11px;height:11px;border-radius:50%;background:${chrome.dot}}
pre{margin:0;padding:10px 14px 14px;font:13.5px/1.18 Menlo,"DejaVu Sans Mono",monospace;background:${c.term_bg};color:${c.term_fg};font-variant-ligatures:none}
pre span{display:inline-block;width:1ch;height:1.18em;vertical-align:top;overflow:visible;white-space:pre}
</style><div id="shot"><div class="title"><div class="dots"><i></i><i></i><i></i></div>${title}</div><pre>${rows}</pre></div>`;
}

async function shoot(chrome, html, file, opts = {}) {
  const page = join(WORK, 'page.html');
  writeFileSync(page, html);
  const bytes = await chrome.shoot(pathToFileURL(page).href, { width: 1800, height: 1200, selector: '#shot', ...opts });
  writeFileSync(file, bytes);
  return bytes.length;
}

mkdirSync(IMAGES, { recursive: true });
mkdirSync(WORK, { recursive: true });
const chrome = await launch();
let failed = false;
try {
  for (const scene of scenes.filter((s) => !only || only.includes(s.name))) {
    for (const variant of VARIANTS) {
      try {
        const page = await play(scene, variant);
        const file = join(IMAGES, `${scene.name}-${variant.kind}.webp`);
        const size = await shoot(chrome, frame(page, variant, scene.title ?? 'astrum — ~/projects/inventory', scene.frameTheme?.[variant.kind]), file);
        console.log(`capture: ${scene.name}-${variant.kind}.webp (${Math.round(size / 1024)} KB)`);
      } catch (e) {
        failed = true;
        console.error(`capture: ${scene.name} (${variant.kind}) failed: ${e.message}${e.stderr ? `\n${e.stderr}` : ''}`);
      }
    }
  }
  if (!only || only.includes('og')) {
    const og = readFileSync(join(HERE, 'og.html'), 'utf8').replace(/\{\{logo\}\}/g, pathToFileURL(join(REPO, 'site', 'assets', 'logo')).href).replace(/\{\{images\}\}/g, pathToFileURL(IMAGES).href);
    const size = await shoot(chrome, og, join(IMAGES, 'og.png'), { width: 1200, height: 630, selector: '#card', format: 'png', scale: 1 });
    console.log(`capture: og.png (${Math.round(size / 1024)} KB)`);
  }
} finally {
  await chrome.close();
}
process.exit(failed ? 1 : 0);
