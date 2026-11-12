// The screenshots: what each one types and clicks, and what the stand-in model answers.
// Every scene starts in ~/projects/inventory, a small Python project made up for the pictures,
// with a zsh prompt of its own and a few stand-in commands in ~/bin.

import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// (The scripts write @{X} for the shell's ${X}, which a template string would take for its own.)
const sh = (s) => s.replace(/@\{/g, '${');

const PASSING = sh(String.raw`#!/bin/sh
# A stand-in for pytest: the output of the project's tests, failing until the fix is in.
B="$(printf '\033[1m')"; G="$(printf '\033[32m')"; R="$(printf '\033[31m')"; Y="$(printf '\033[33m')"; N="$(printf '\033[0m')"
echo "@{B}========================= test session starts ==========================@{N}"
echo "platform darwin -- Python 3.12.4, pytest-8.3.2, pluggy-1.5.0"
echo "rootdir: /Users/ada/projects/inventory"
echo "collected 12 items"
echo
if grep -q 'raise OutOfStock' "$HOME/projects/inventory/src/inventory/stock.py"; then
  echo "tests/test_orders.py @{G}......@{N}                                      @{G}[ 50%]@{N}"
  echo "tests/test_stock.py @{G}......@{N}                                       @{G}[100%]@{N}"
  echo
  echo "@{G}========================== @{B}12 passed@{N}@{G} in 0.19s ==========================@{N}"
  exit 0
fi
echo "tests/test_orders.py @{G}......@{N}                                      @{G}[ 50%]@{N}"
echo "tests/test_stock.py @{G}....@{R}F@{G}.@{N}                                       @{R}[100%]@{N}"
echo
echo "=============================== FAILURES ==============================="
echo "@{R}@{B}___________________ test_reserve_more_than_available ___________________@{N}"
echo
echo "    def test_reserve_more_than_available():"
echo "        stock = Stock(\"A-100\", on_hand=5)"
echo "        with pytest.raises(OutOfStock):"
echo ">           stock.reserve(8)"
echo "@{R}@{B}E           Failed: DID NOT RAISE <class 'inventory.stock.OutOfStock'>@{N}"
echo
echo "@{R}@{B}tests/test_stock.py@{N}:31: Failed"
echo "@{Y}======================= short test summary info ========================@{N}"
echo "@{R}FAILED@{N} tests/test_stock.py::@{B}test_reserve_more_than_available@{N} - Fai..."
echo "@{R}===================== @{B}1 failed@{N}@{R}, 11 passed in 0.21s =====================@{N}"
exit 1
`);

const SERVE = sh(String.raw`#!/bin/sh
# A stand-in for the project's web server: a few lines of log, then it keeps running.
D="$(printf '\033[2m')"; G="$(printf '\033[32m')"; C="$(printf '\033[36m')"; Y="$(printf '\033[33m')"; N="$(printf '\033[0m')"
echo "@{D}14:02:11@{N} @{G}INFO@{N}  inventory.api  starting on @{C}http://127.0.0.1:8000@{N}"
echo "@{D}14:02:11@{N} @{G}INFO@{N}  inventory.db   connected (sqlite, 3 tables)"
echo "@{D}14:02:14@{N} @{G}INFO@{N}  GET  /stock/A-100           200   4 ms"
echo "@{D}14:02:15@{N} @{G}INFO@{N}  GET  /stock/B-220           200   3 ms"
echo "@{D}14:02:19@{N} @{G}INFO@{N}  POST /orders                201  12 ms"
echo "@{D}14:02:20@{N} @{Y}WARN@{N}  POST /orders/1043/reserve   409   6 ms  only 5 on hand"
echo "@{D}14:02:26@{N} @{G}INFO@{N}  GET  /stock?low=1           200   5 ms"
exec sleep 100000
`);

const NPM = sh(String.raw`#!/bin/sh
# A stand-in for npm run build: a build that is still going when the picture is taken.
D="$(printf '\033[2m')"; G="$(printf '\033[32m')"; N="$(printf '\033[0m')"
echo
echo "> website@2.1.0 build"
echo "> vite build"
echo
echo "@{G}vite v5.4.2@{N} building for production..."
echo "transforming (412) @{D}src/components/StockTable.tsx@{N}"
exec sleep 100000
`);

export const project = {
  '.zshrc': `PROMPT='%F{blue}%~%f %F{magenta}❯%f '
export CLICOLOR=1 LSCOLORS=ExGxFxdaCxDaDahbadacec
alias ls='ls -G'
unsetopt PROMPT_SP
`,
  'bin/pytest': PASSING,
  'bin/serve': SERVE,
  'bin/npm': NPM,
  'projects/inventory/README.md': '# inventory\n\nStock levels and orders for the warehouse, as a small web service.\n',
  'projects/inventory/pyproject.toml': '[project]\nname = "inventory"\nversion = "0.3.1"\nrequires-python = ">=3.11"\n',
  'projects/inventory/src/inventory/__init__.py': 'from .stock import Stock, OutOfStock\n',
  'projects/inventory/src/inventory/orders.py': '"""Orders, and the stock they hold."""\n\nfrom .stock import Stock\n\n\ndef place(stock: Stock, quantity: int) -> None:\n    stock.reserve(quantity)\n',
  'projects/inventory/src/inventory/stock.py': `"""Stock levels for the warehouse."""


class OutOfStock(Exception):
    """More was asked for than is available."""


class Stock:
    def __init__(self, sku: str, on_hand: int = 0):
        self.sku = sku
        self.on_hand = on_hand
        self.reserved = 0

    @property
    def available(self) -> int:
        return self.on_hand - self.reserved

    def reserve(self, quantity: int) -> None:
        if quantity < 0:
            raise ValueError("quantity must be positive")
        self.reserved += quantity

    def release(self, quantity: int) -> None:
        self.reserved = max(0, self.reserved - quantity)
`,
  'projects/inventory/tests/test_stock.py': `import pytest

from inventory import OutOfStock, Stock


def test_reserve_more_than_available():
    stock = Stock("A-100", on_hand=5)
    with pytest.raises(OutOfStock):
        stock.reserve(8)
`,
  'projects/inventory/tests/test_orders.py': 'from inventory.orders import place\n',
  'projects/website/index.html': '<!doctype html>\n',
  'notes/todo.md': '- ship 0.3.1\n',
};

/// Prefix, then a key.
const cmd = (key) => ['key:ctrl+b', `key:${key}`];
const newTab = [...cmd('c'), 'wait:900'];

// The question every agent scene starts from, and how the agent answers it.
const FIX = {
  match: 'test_reserve_more_than_available',
  steps: [
    { say: "I'll look at `reserve()` first.", tool: ['read_file', { path: 'src/inventory/stock.py' }] },
    {
      say: "The test asks for 8 of an item with 5 on hand and expects `OutOfStock`, but `reserve()` only rejects a negative quantity. It never compares the request with `available`, so it reserves stock that isn't there.\n\nThe fix is to refuse what is more than is available:",
      tool: ['edit_file', { path: 'src/inventory/stock.py', old_text: '            raise ValueError("quantity must be positive")\n        self.reserved += quantity', new_text: '            raise ValueError("quantity must be positive")\n        if quantity > self.available:\n            raise OutOfStock(f"only {self.available} left")\n        self.reserved += quantity' }],
    },
    { say: 'The change is in. Now the tests:', tool: ['run_command', { command: 'pytest tests/test_stock.py', explanation: 'Run the stock tests again to check the fix.' }] },
    { say: 'All 12 tests pass. `reserve()` now raises `OutOfStock` when more is asked for than is available, and the error says how many are left.' },
  ],
};

const TESTS = {
  match: 'run the tests',
  steps: [{ say: "I'll run the suite.", tool: ['run_command', { command: 'pytest -q', explanation: 'Run the whole test suite.' }] }],
};

const BUILD = {
  match: 'build the site',
  steps: [{ say: "I'll build it and compare the bundle with the last one.", tool: ['run_command', { command: 'npm run build', explanation: 'Build the site for production.' }] }, { wait: true }],
};

const TODO = {
  match: 'todo list',
  steps: [
    { delay: 1500, say: '', tool: ['read_file', { path: 'todo.md' }] },
    { say: 'Two things are left:\n\n- ship 0.3.1, with the fix for reservations\n- write the release notes' },
  ],
};

const ASK = {
  match: 'take the most space',
  steps: [
    {
      say: "To list the ten largest files under this folder, biggest first:\n\n```\nfind . -type f -exec du -h {} + | sort -rh | head -n 10\n```\n\n`du -h` prints each file's size in K, M or G, `sort -rh` puts the largest first, and `head` keeps ten. For folders rather than files, `du -sh * | sort -rh` does the same one level down.",
    },
  ],
};

const ISSUES = {
  match: 'issues about reserv',
  steps: [
    { say: '', tool: ['issues__search_issues', { query: 'reserve' }] },
    { say: 'Three open issues touch reservations:\n\n- **#1043** orders can reserve more stock than there is: the bug you just fixed\n- **#1038** the low-stock report misses reserved items\n- **#1021** releasing part of a reservation\n\nShall I comment on #1043 with the fix?' },
  ],
};

const SKILL = (name, description, body = '') => `---\nname: ${name}\ndescription: ${description}\n---\n\n${body}\n`;

export const scenes = [
  {
    name: 'hero',
    cols: 160,
    rows: 42,
    autonomy: 'safe',
    scripts: [FIX],
    actions: ['until:❯', ...newTab, 'line:cd ~/projects/website', ...newTab, 'line:serve', 'until:low=1', ...cmd('1'), 'line:pytest', 'until:1 failed', 'key:ctrl+space', 'wait:600', 'text:why does test_reserve_more_than_available fail?', 'key:enter', 'until:Waiting for your answer', 'wait:800'],
  },
  {
    name: 'approval',
    cols: 160,
    rows: 42,
    autonomy: 'safe',
    scripts: [FIX],
    actions: ['until:❯', 'line:pytest', 'until:1 failed', 'key:ctrl+space', 'wait:600', 'text:why does test_reserve_more_than_available fail?', 'key:enter', 'until:Waiting for your answer', 'wait:600', 'key:y', 'until:Run pytest', 'wait:800'],
  },
  {
    name: 'ask',
    cols: 160,
    rows: 30,
    scripts: [ASK],
    // (The note that the line went to the agent fades; the picture is taken after.)
    actions: ['until:❯', 'line:cat README.md', 'wait:300', 'line:ls', 'wait:300', 'line:# which files here take the most space?', 'until:one level down', 'wait:5000'],
  },
  {
    name: 'agents',
    cols: 160,
    rows: 40,
    scripts: [TESTS, BUILD, TODO],
    config: { agent: 'allow = ["npm run build"]' },
    actions: [
      'until:❯',
      'key:ctrl+space',
      'wait:500',
      'text:run the tests and tell me what fails',
      'key:enter',
      'until:Waiting for your answer',
      ...newTab,
      'line:cd ~/projects/website',
      'key:ctrl+space',
      'wait:500',
      'text:build the site and tell me if it is any smaller',
      'key:enter',
      'until:transforming',
      ...newTab,
      'line:cd ~/notes',
      'key:ctrl+space',
      'wait:500',
      'text:what is left on my todo list?',
      'key:enter',
      ...newTab,
      'line:serve',
      'wait:3500',
      ...cmd('A'),
      'wait:1200',
    ],
  },
  {
    name: 'mcp',
    cols: 160,
    rows: 40,
    scripts: [ISSUES],
    setup(sb) {
      // Each server is started by a name on the PATH, as an installed one would be.
      const server = new URL('./mcp-server.mjs', import.meta.url).pathname;
      for (const name of ['issues', 'docs']) sb.write(`bin/${name}-mcp`, `#!/bin/sh\nexec "${process.execPath}" "${server}" ${name}\n`, 0o755);
    },
    config: {
      more: () => '[mcp.issues]\ncommand = "issues-mcp"\nallow = ["list_*", "get_*", "search_*"]\n\n[mcp.docs]\ncommand = "docs-mcp"\n',
    },
    actions: ['until:❯', 'key:ctrl+space', 'wait:500', 'text:are there open issues about reservations?', 'key:enter', 'until:Shall I comment', 'wait:500', 'text:/mcp', 'key:enter', 'wait:1500'],
  },
  {
    name: 'skills',
    cols: 160,
    rows: 40,
    setup(sb) {
      const write = (dir, name, description) => {
        mkdirSync(join(dir, name), { recursive: true });
        writeFileSync(join(dir, name, 'SKILL.md'), SKILL(name, description, 'Steps to follow.'));
      };
      // Your own, a shared folder named in skill_dirs, and the project's.
      write(join(sb.cfg, 'skills'), 'release-notes', 'Write release notes from the commits since the last tag, grouped by what changed for users.');
      write(join(sb.cfg, 'skills'), 'python-tests', 'Run and fix pytest suites: how to read failures, rerun one test, and keep fixtures small.');
      write(join(sb.home, 'shared-skills'), 'pdf-forms', 'Fill in PDF forms and pull the text out of PDFs with the scripts this skill brings.');
      write(join(sb.project, '.agents', 'skills'), 'db-migrations', 'How this project changes its database: write a migration, try it on a copy, then apply it.');
    },
    config: { agent: 'skill_dirs = ["~/shared-skills"]' },
    actions: ['until:❯', 'key:ctrl+space', 'wait:600', 'text:/skills', 'key:enter', 'wait:1500'],
  },
  {
    name: 'settings',
    cols: 150,
    rows: 40,
    actions: ['until:❯', 'line:ls', 'wait:300', ...cmd('.'), 'wait:1200'],
  },
  {
    name: 'themes',
    cols: 150,
    rows: 40,
    // In the dark picture the picker shows another theme than the one in use, as it does while you choose.
    frameTheme: { dark: 'tokyo-night' },
    actions: (variant) => ['until:❯', 'line:pytest', 'until:1 failed', ...cmd('s'), 'wait:700', ...(variant.kind === 'dark' ? ['key:down', 'key:down', 'wait:700'] : ['wait:300'])],
  },
  {
    name: 'menu',
    cols: 150,
    rows: 40,
    actions: ['until:❯', 'line:pytest', 'until:1 failed', ...cmd('space'), 'wait:1000'],
  },
  {
    name: 'panes',
    actions: ['until:❯', ...newTab, 'line:cd ~/projects/website', ...newTab, 'line:cd ~/notes', ...cmd('1'), 'line:cat src/inventory/stock.py', ...cmd('|'), 'wait:700', 'line:pytest', 'until:1 failed', ...cmd('-'), 'wait:700', 'line:serve', 'until:low=1', 'wait:500'],
  },
];
