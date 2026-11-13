// The screenshots: what each one types and clicks, and what the stand-in model answers.
// Every scene starts in ~/projects/inventory, a small Python project made up for the pictures,
// with a zsh prompt of its own and a few stand-in commands in ~/bin.

import { mkdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// (The scripts write @{X} for the shell's ${X}, which a template string would take for its own.)
const sh = (s) => s.replace(/@\{/g, '${');

const PASSING = sh(String.raw`#!/bin/sh
# A stand-in for pytest: the output of the project's tests, failing until the fix is in.
# Like pytest, it fits what it prints to the width of the terminal.
B="$(printf '\033[1m')"; G="$(printf '\033[32m')"; R="$(printf '\033[31m')"; Y="$(printf '\033[33m')"; N="$(printf '\033[0m')"
W=$(stty size 2>/dev/null | cut -d' ' -f2); [ -n "$W" ] || W=80; [ "$W" -gt 72 ] && W=72
# rule TEXT CHAR COLOR: the text in the middle of a line of that character.
rule() { n=$(( (W - @{#1} - 2) / 2 )); side=$(printf "%@{n}s" | tr ' ' "$2"); echo "$3$side $1 $side$N"; }
# ran FILE DOTS COLORED-DOTS PERCENT COLOR: a file's tests, and how far along that is.
ran() { pad=$(( W - @{#1} - @{#2} - 7 )); printf '%s %s%*s%s\n' "$1" "$3" "$pad" "" "$5[$4]$N"; }
rule "test session starts" = "$B"
echo "platform darwin -- Python 3.12.4, pytest-8.3.2, pluggy-1.5.0"
echo "rootdir: /Users/ada/projects/inventory"
echo "collected 12 items"
echo
if grep -q 'raise OutOfStock' "$HOME/projects/inventory/src/inventory/stock.py"; then
  ran tests/test_orders.py ...... "@{G}......@{N}" " 50%" "$G"
  ran tests/test_stock.py ...... "@{G}......@{N}" "100%" "$G"
  echo
  rule "12 passed in 0.19s" = "$G"
  exit 0
fi
ran tests/test_orders.py ...... "@{G}......@{N}" " 50%" "$G"
ran tests/test_stock.py ....F. "@{G}....@{R}F@{G}.@{N}" "100%" "$R"
echo
rule FAILURES = ""
rule test_reserve_more_than_available _ "$R$B"
echo
echo "    def test_reserve_more_than_available():"
echo "        stock = Stock(\"A-100\", on_hand=5)"
echo "        with pytest.raises(OutOfStock):"
echo ">           stock.reserve(8)"
echo "@{R}@{B}E           Failed: DID NOT RAISE <class 'inventory.stock.OutOfStock'>@{N}"
echo
echo "@{R}@{B}tests/test_stock.py@{N}:31: Failed"
rule "short test summary info" = "$Y"
why="tests/test_stock.py::test_reserve_more_than_available - Failed: DID NOT RAISE <class 'inventory.stock.OutOfStock'>"
echo "@{R}FAILED@{N} $(printf '%s' "$why" | cut -c1-$(( W - 10 )))..."
rule "1 failed, 11 passed in 0.21s" = "$R"
exit 1
`);

const SERVE = sh(String.raw`#!/bin/sh
# A stand-in for the project's web server: some lines of log, then it keeps running.
D="$(printf '\033[2m')"; G="$(printf '\033[32m')"; C="$(printf '\033[36m')"; Y="$(printf '\033[33m')"; R="$(printf '\033[31m')"; N="$(printf '\033[0m')"
echo "@{D}14:02:11@{N} @{G}INFO@{N}  inventory.api  starting on @{C}http://127.0.0.1:8000@{N}"
echo "@{D}14:02:11@{N} @{G}INFO@{N}  inventory.db   connected (sqlite, 3 tables)"
echo "@{D}14:02:14@{N} @{G}INFO@{N}  GET  /stock/A-100           200   4 ms"
echo "@{D}14:02:15@{N} @{G}INFO@{N}  GET  /stock/B-220           200   3 ms"
echo "@{D}14:02:19@{N} @{G}INFO@{N}  POST /orders                201  12 ms"
echo "@{D}14:02:20@{N} @{Y}WARN@{N}  POST /orders/1043/reserve   409   6 ms  only 5 on hand"
echo "@{D}14:02:26@{N} @{G}INFO@{N}  GET  /stock?low=1           200   5 ms"
echo "@{D}14:02:31@{N} @{G}INFO@{N}  GET  /stock/C-007           200   3 ms"
echo "@{D}14:02:33@{N} @{G}INFO@{N}  POST /orders                201  11 ms"
echo "@{D}14:02:34@{N} @{G}INFO@{N}  POST /orders/1044/reserve   200   7 ms"
echo "@{D}14:02:40@{N} @{R}ERROR@{N} GET  /stock/Z-999           404   2 ms  no such item"
echo "@{D}14:02:47@{N} @{G}INFO@{N}  GET  /stock/A-100           200   3 ms"
echo "@{D}14:02:52@{N} @{G}INFO@{N}  POST /orders/1044/ship      200   9 ms"
echo "@{D}14:02:58@{N} @{G}INFO@{N}  GET  /stock?low=1           200   4 ms"
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

// On the machine the "tab on another machine" scene reaches: a prompt that says where it
// is, and stand-ins for the two commands run there.
const FAR_ZSHRC = `unsetopt PROMPT_SP
PROMPT='%F{green}ada@build-box%f %F{blue}%~%f %F{magenta}❯%f '
cd ~/srv/inventory
`;

const SYSTEMCTL = sh(String.raw`#!/bin/sh
B="$(printf '\033[1m')"; G="$(printf '\033[32m')"; N="$(printf '\033[0m')"
echo "@{G}●@{N} inventory.service - Inventory web service"
echo "     Loaded: loaded (/etc/systemd/system/inventory.service; enabled; preset: enabled)"
echo "     Active: @{G}@{B}active (running)@{N} since Mon 2026-11-09 06:12:44 UTC; 3 days ago"
echo "   Main PID: 1482 (uvicorn)"
echo "      Tasks: 5 (limit: 9418)"
echo "     Memory: 61.3M"
echo "        CPU: 4min 12.608s"
echo "     CGroup: /system.slice/inventory.service"
echo "             └─1482 /srv/inventory/.venv/bin/python -m uvicorn inventory.api:app --port 8000"
`);

const JOURNALCTL = sh(String.raw`#!/bin/sh
D="$(printf '\033[2m')"; Y="$(printf '\033[33m')"; N="$(printf '\033[0m')"
for line in "09:41:02 GET  /stock/A-100           200   3 ms" "09:41:07 POST /orders                201  10 ms" "09:41:08 POST /orders/2210/reserve   200   6 ms" "09:41:15 GET  /stock?low=1           200   4 ms" "09:41:21 POST /orders/2210/ship      200   8 ms" "09:41:30 GET  /stock/B-220           200   3 ms"; do
  echo "@{D}Nov 12@{N} $line" | sed "s/^\(.*Nov 12[^ ]* [0-9:]*\)/\1 build-box inventory[1482]:/"
done
echo "@{D}Nov 12@{N} 09:41:34 build-box inventory[1482]: @{Y}POST /orders/2211/reserve   409   5 ms  only 2 on hand@{N}"
exec sleep 100000
`);

export const project = {
  '.zshrc': `setopt PROMPT_SUBST
unsetopt PROMPT_SP
branch() { local b; b=$(git branch --show-current 2>/dev/null) && [ -n "$b" ] && print -n " %F{yellow}$b%f" }
PROMPT='%F{blue}%~%f$(branch) %F{magenta}❯%f '
export CLICOLOR=1 LSCOLORS=ExGxFxdaCxDaDahbadacec
alias ls='ls -G'
`,
  '.gitconfig': '[user]\n\tname = Ada\n\temail = ada@example.com\n[core]\n\tpager = cat\n[color]\n\tui = auto\n[init]\n\tdefaultBranch = main\n',
  '.vimrc': 'syntax on\nset number laststatus=2 noswapfile viminfo= ruler\nset statusline=%f\\ %m%=%l,%c\\ \\ %P\n',
  'bin/pytest': PASSING,
  'bin/serve': SERVE,
  'bin/npm': NPM,
  'projects/inventory/README.md': `# inventory

Stock levels and orders for the warehouse, as a small web service.

    make dev      start the service on port 8000
    make test     run the tests

A reservation holds stock for an order until it ships or is cancelled.
`,
  'projects/inventory/pyproject.toml': '[project]\nname = "inventory"\nversion = "0.3.1"\nrequires-python = ">=3.11"\ndependencies = ["fastapi", "uvicorn"]\n\n[tool.pytest.ini_options]\ntestpaths = ["tests"]\n',
  'projects/inventory/Makefile': 'dev:\n\tserve\n\ntest:\n\tpytest\n\n.PHONY: dev test\n',
  'projects/inventory/docs/api.md': '# API\n\n- `GET /stock/{sku}`: what is on hand, reserved and available\n- `POST /orders`: place an order\n- `POST /orders/{id}/reserve`: hold stock for it\n',
  'projects/inventory/scripts/dev.sh': '#!/bin/sh\nexec serve\n',
  'projects/inventory/src/inventory/__init__.py': 'from .stock import Stock, OutOfStock\n',
  'projects/inventory/src/inventory/api.py': `"""The web service."""

from fastapi import FastAPI, HTTPException

from .stock import OutOfStock, Stock

app = FastAPI(title="inventory")
shelves: dict[str, Stock] = {}


@app.get("/stock/{sku}")
def stock(sku: str) -> dict:
    item = shelves.get(sku)
    if item is None:
        raise HTTPException(404, f"no such item: {sku}")
    return {"sku": sku, "on_hand": item.on_hand, "available": item.available}


@app.post("/stock/{sku}/reserve")
def reserve(sku: str, quantity: int) -> dict:
    try:
        shelves[sku].reserve(quantity)
    except OutOfStock as e:
        raise HTTPException(409, str(e))
    return {"sku": sku, "reserved": quantity}
`,
  'projects/inventory/src/inventory/orders.py': '"""Orders, and the stock they hold."""\n\nfrom .stock import Stock\n\n\ndef place(stock: Stock, quantity: int) -> None:\n    stock.reserve(quantity)\n',
  'projects/inventory/src/inventory/report.py': `"""What is running low."""

from .stock import Stock


def low(shelves: dict[str, Stock]) -> list[Stock]:
    return [s for s in shelves.values() if s.on_hand < 10]
`,
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
  'projects/inventory/tests/test_report.py': 'from inventory.report import low\n',
  'projects/website/index.html': '<!doctype html>\n',
  'projects/website/package.json': '{ "name": "website", "version": "2.1.0", "scripts": { "build": "vite build" } }\n',
  'notes/todo.md': '- ship 0.3.1\n- write the release notes\n',
};

// The project's history, made with git itself at fixed dates, so that `git log` is the
// same in every run; and what has been changed since the last commit.
export const history = [
  { when: '2026-10-02T09:14:00', message: 'Stock levels and reservations', paths: ['README.md', 'pyproject.toml', 'src/inventory/__init__.py', 'src/inventory/stock.py'] },
  { when: '2026-10-06T15:40:00', message: 'Orders hold stock until they ship', paths: ['src/inventory/orders.py', 'tests/test_orders.py'] },
  { when: '2026-10-13T11:02:00', message: 'The web service: stock and reservations', paths: ['src/inventory/api.py', 'docs/api.md'] },
  { when: '2026-10-21T16:27:00', message: 'Report what is running low', paths: ['src/inventory/report.py', 'tests/test_report.py'], tag: 'v0.3.0' },
  { when: '2026-11-03T10:05:00', message: 'A Makefile, and a script for the dev server', paths: ['Makefile', 'scripts/dev.sh'] },
  { when: '2026-11-09T14:51:00', message: 'A test for reserving more than there is', paths: ['tests/test_stock.py'] },
];

export const dirty = {
  'src/inventory/report.py': `"""What is running low."""

from .stock import Stock


def low(shelves: dict[str, Stock], below: int = 10) -> list[Stock]:
    """Items with fewer than \`below\` available, fewest first."""
    running_low = [s for s in shelves.values() if s.available < below]
    return sorted(running_low, key=lambda s: s.available)
`,
};

/// Prefix, then a key.
const cmd = (key) => ['key:ctrl+b', `key:${key}`];
const newTab = [...cmd('c'), 'wait:900'];
/// A command typed at the prompt, and a moment for the shell to be ready for the next.
const run = (command, wait = 450) => [`line:${command}`, `wait:${wait}`];
/// Open the agent panel and ask it something.
const ask = (question) => ['key:ctrl+space', 'wait:500', `text:${question}`, 'key:enter'];

// What the stand-in model answers, by the words of the question.
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

const ABOUT = {
  match: 'what is this project',
  steps: [
    { say: '', tool: ['read_file', { path: 'README.md' }] },
    { say: 'A small web service for a warehouse: it keeps stock levels and the orders that hold them. The code is in `src/inventory`, the tests in `tests`; `make test` runs them.' },
  ],
};

const REWRITE = {
  match: 'more than is reserved',
  steps: [
    { say: 'All three belong in `stock.py`.', tool: ['read_file', { path: 'src/inventory/stock.py' }] },
    {
      say: 'A stock level can no longer start below zero, `reserve()` refuses more than is available, and `release()` more than is reserved. Each raises now, instead of quietly going wrong:',
      tool: [
        'write_file',
        {
          path: 'src/inventory/stock.py',
          content: `"""Stock levels for the warehouse."""


class OutOfStock(Exception):
    """More was asked for than is available."""


class Stock:
    def __init__(self, sku: str, on_hand: int = 0):
        if on_hand < 0:
            raise ValueError("on_hand must not be negative")
        self.sku = sku
        self.on_hand = on_hand
        self.reserved = 0

    @property
    def available(self) -> int:
        return self.on_hand - self.reserved

    def reserve(self, quantity: int) -> None:
        if quantity < 0:
            raise ValueError("quantity must be positive")
        if quantity > self.available:
            raise OutOfStock(f"only {self.available} left")
        self.reserved += quantity

    def release(self, quantity: int) -> None:
        if quantity > self.reserved:
            raise ValueError(f"only {self.reserved} reserved")
        self.reserved -= quantity
`,
        },
      ],
    },
  ],
};

// A provider that turns the request away: the agent stops, and says why.
const REFUSED = {
  match: 'summarize the API',
  steps: [{ delay: 600, fail: [400, 'registry.ollama.ai/library/qwen3-coder:30b does not support tools'] }],
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
    { say: 'Three open issues touch reservations:\n\n- **#1043** orders can reserve more stock than there is\n- **#1038** the low-stock report misses reserved items\n- **#1021** releasing part of a reservation\n\n#1043 is what the failing test is about. Shall I comment on it once the fix is in?' },
  ],
};

const COMMENT = {
  match: 'comment on #1043',
  steps: [
    {
      say: "I'll say what was wrong and what changed.",
      tool: ['issues__add_comment', { issue: 1043, body: 'Found it: `reserve()` only refused a negative quantity and never looked at what was available.\n\nIt now raises `OutOfStock` when more is asked for than there is, and `tests/test_stock.py` covers it.' }],
    },
  ],
};

const SKILL = (name, description, body = '') => `---\nname: ${name}\ndescription: ${description}\n---\n\n${body}\n`;

/// Each MCP server is started by a name on the PATH, as an installed one would be.
const mcpServers = {
  setup(sb) {
    const server = new URL('./mcp-server.mjs', import.meta.url).pathname;
    for (const name of ['issues', 'docs']) sb.write(`bin/${name}-mcp`, `#!/bin/sh\nexec "${process.execPath}" "${server}" ${name}\n`, 0o755);
  },
  config: {
    more: () => '[mcp.issues]\ncommand = "issues-mcp"\nallow = ["list_*", "get_*", "search_*"]\n\n[mcp.docs]\ncommand = "docs-mcp"\n',
  },
};

export const scenes = [
  // The front page: three tabs with an agent each, two panes, and a change waiting for a yes.
  {
    name: 'hero',
    cols: 150,
    rows: 38,
    autonomy: 'safe',
    // (A question is sent with what is on the screen, so the one about the project is looked for first.)
    scripts: [ABOUT, FIX, BUILD, TODO],
    config: { agent: 'allow = ["npm run build"]' },
    actions: [
      'until:❯',
      ...newTab,
      ...run('cd ~/projects/website'),
      ...ask('build the site and tell me if it is any smaller'),
      'until:transforming',
      ...newTab,
      ...run('cd ~/notes'),
      ...ask('what is left on my todo list?'),
      ...cmd('1'),
      'line:pytest',
      'until:1 failed',
      ...cmd('-'),
      'wait:800',
      'line:serve',
      'until:14:02:58',
      ...cmd('up'),
      'wait:400',
      ...ask('what is this project?'),
      'until:warehouse',
      'wait:1500',
      'wait:400',
      'text:why does test_reserve_more_than_available fail?',
      'key:enter',
      'until:Waiting for your answer',
      'wait:800',
    ],
  },
  // A command asked about, with the panel below the shell (a narrower terminal).
  {
    name: 'approval',
    cols: 120,
    rows: 36,
    autonomy: 'safe',
    scripts: [FIX],
    actions: ['until:❯', ...run('git status -sb'), 'line:pytest', 'until:1 failed', ...ask('why does test_reserve_more_than_available fail?'), 'until:Waiting for your answer', 'wait:600', 'key:y', 'until:Run pytest', 'wait:800'],
  },
  // A file change shown as a diff, with the choice to go through it part by part.
  {
    name: 'diff',
    cols: 150,
    rows: 40,
    scripts: [REWRITE],
    actions: ['until:❯', ...run('cat tests/test_stock.py'), 'line:pytest', 'until:1 failed', ...run('git status -sb'), ...ask('a stock level should not start below zero, reserve() should refuse more than is available, and release() more than is reserved'), 'until:Waiting for your answer', 'wait:800'],
  },
  // A question typed at the prompt: one answered, the next being typed.
  {
    name: 'ask',
    cols: 120,
    rows: 27,
    scripts: [ASK],
    // (The note that the line went to the agent fades; the picture is taken after.)
    actions: ['until:❯', ...run('git log --oneline --decorate'), ...run('git status -sb'), ...run('ls'), 'line:# which files here take the most space?', 'until:one level down', 'wait:5000', 'text:# and how do I see what is free on this disk?', 'wait:500'],
  },
  // Every tab's agent: one asking, one at work, one finished, one stopped, one with nothing.
  {
    name: 'agents',
    cols: 120,
    rows: 27,
    scripts: [TESTS, BUILD, TODO, REFUSED],
    config: { agent: 'allow = ["npm run build"]' },
    actions: [
      'until:❯',
      ...ask('run the tests and tell me what fails'),
      'until:Waiting for your answer',
      ...newTab,
      ...run('cd ~/projects/website'),
      ...ask('build the site and tell me if it is any smaller'),
      'until:transforming',
      ...newTab,
      ...run('cd ~/notes'),
      ...ask('what is left on my todo list?'),
      ...newTab,
      ...run('cd ~/projects/inventory/docs'),
      ...ask('summarize the API for the release notes'),
      ...newTab,
      ...run('cd ~/projects/inventory'),
      ...run('git log --oneline --decorate'),
      'line:serve',
      'until:14:02:58',
      'wait:2500',
      ...cmd('A'),
      'wait:1200',
    ],
  },
  // An answer made with an MCP server's tool, then what /mcp says of the servers.
  {
    name: 'mcp',
    cols: 120,
    rows: 34,
    scripts: [ISSUES],
    ...mcpServers,
    actions: ['until:❯', 'line:pytest', 'until:1 failed', ...ask('are there open issues about reservations?'), 'until:Shall I comment', 'wait:500', 'text:/mcp', 'key:enter', 'wait:1500'],
  },
  // A tool of an MCP server asked about, with what it would be given.
  {
    name: 'mcp-tool',
    cols: 120,
    rows: 36,
    scripts: [ISSUES, COMMENT],
    ...mcpServers,
    actions: ['until:❯', 'line:pytest', 'until:1 failed', ...ask('are there open issues about reservations?'), 'until:Shall I comment', 'wait:500', 'text:yes, comment on #1043 now', 'key:enter', 'until:Waiting for your answer', 'wait:800'],
  },
  // A skill of the project's, as a file and as /skills lists it.
  {
    name: 'skills',
    cols: 120,
    rows: 29,
    setup(sb) {
      const write = (dir, name, description, body = 'Steps to follow.') => {
        mkdirSync(join(dir, name), { recursive: true });
        writeFileSync(join(dir, name, 'SKILL.md'), SKILL(name, description, body));
      };
      // Your own, a shared folder named in skill_dirs, and the project's.
      write(join(sb.cfg, 'skills'), 'release-notes', 'Write release notes from the commits since the last tag, grouped by what changed for users.');
      write(join(sb.cfg, 'skills'), 'python-tests', 'Run and fix pytest suites: how to read failures, rerun one test, and keep fixtures small.');
      write(join(sb.home, 'shared-skills'), 'pdf-forms', 'Fill in PDF forms and pull the text out of PDFs with the scripts this skill brings.');
      write(join(sb.project, '.agents', 'skills'), 'db-migrations', 'How this project changes its database: write a migration, try it on a copy, then apply it.', '1. Write the migration in `migrations/`, numbered after the last one.\n2. Try it on a copy: `scripts/try-migration.sh`.\n3. Apply it, and add what it changed to `docs/schema.md`.');
    },
    config: { agent: 'skill_dirs = ["~/shared-skills"]' },
    actions: ['until:❯', ...run('ls ~/.config/astrum/skills'), ...run('ls .agents/skills'), ...run('cat .agents/skills/db-migrations/SKILL.md'), 'key:ctrl+space', 'wait:600', 'text:/skills', 'key:enter', 'wait:1500'],
  },
  {
    name: 'settings',
    cols: 104,
    rows: 28,
    actions: ['until:❯', ...run('git status -sb'), 'line:pytest', 'until:1 failed', ...cmd('.'), 'wait:1200'],
  },
  // The first of the cards shown the first time Astrum is used.
  {
    name: 'walkthrough',
    cols: 104,
    rows: 24,
    actions: ['until:❯', ...run('git log --oneline --decorate'), ...run('git status -sb'), ...run('ls'), ...cmd('.'), 'wait:500', 'key:up', 'key:up', 'wait:300', 'key:enter', 'wait:1000'],
  },
  {
    name: 'themes',
    cols: 132,
    rows: 32,
    // In the dark picture the picker shows another theme than the one in use, as it does while you choose.
    frameTheme: { dark: 'tokyo-night' },
    actions: (variant) => ['until:❯', ...cmd('|'), 'wait:800', ...run('vim src/inventory/api.py', 1200), ...cmd('left'), 'wait:300', 'line:pytest', 'until:1 failed', ...cmd('s'), 'wait:700', ...(variant.kind === 'dark' ? ['key:down', 'key:down', 'wait:700'] : ['wait:300'])],
  },
  // The models the providers offer, to choose from.
  {
    name: 'models',
    cols: 104,
    rows: 26,
    models: ['qwen3-coder:30b', 'deepseek-r2:14b', 'gemma4:12b', 'llama4:8b', 'mistral-small:24b'],
    actions: ['until:❯', ...run('git status -sb'), 'line:pytest', 'until:1 failed', ...cmd('m'), 'wait:1500'],
  },
  {
    name: 'menu',
    cols: 132,
    rows: 32,
    actions: ['until:❯', ...run('vim src/inventory/stock.py', 1200), ...cmd('|'), 'wait:800', 'line:pytest', 'until:1 failed', ...cmd('left'), 'wait:300', ...cmd('space'), 'wait:1000'],
  },
  // Three tabs, the first split three ways: an editor, a test run and a server's log.
  {
    name: 'panes',
    cols: 144,
    rows: 36,
    actions: ['until:❯', ...newTab, ...run('cd ~/projects/website'), ...newTab, ...run('cd ~/notes'), ...cmd('1'), ...run('vim src/inventory/api.py', 1200), ...cmd('|'), 'wait:800', 'line:pytest', 'until:1 failed', ...cmd('-'), 'wait:800', 'line:serve', 'until:14:02:58', 'wait:500'],
  },
  // Words looked for in what has scrolled by.
  {
    name: 'find',
    cols: 120,
    rows: 28,
    actions: ['until:❯', ...run('git log --stat', 700), ...cmd('/'), 'wait:400', 'text:reserv', 'wait:800'],
  },
  // A tab on another machine: a stand-in for ssh runs Astrum's own far side in a home of
  // its own in the sandbox, so the tab, its mark and its shell are the real thing.
  {
    name: 'remote',
    cols: 120,
    rows: 21,
    setup(sb) {
      const far = join(sb.root, 'far');
      sb.farRuntime = `${sb.runtime}f`;
      for (const dir of ['home/.local/bin', 'home/bin', 'home/srv/inventory', 'cfg']) mkdirSync(join(far, dir), { recursive: true });
      symlinkSync(sb.astrum, join(far, 'home/.local/bin/astrum'));
      writeFileSync(join(far, 'home/.zshrc'), FAR_ZSHRC);
      writeFileSync(join(far, 'home/bin/systemctl'), SYSTEMCTL, { mode: 0o755 });
      writeFileSync(join(far, 'home/bin/journalctl'), JOURNALCTL, { mode: 0o755 });
      // (The command is the last word; it is run in that home, as on a machine of its own,
      // which is given a name, since it has none but this machine's.)
      writeFileSync(
        join(far, 'ssh'),
        `#!/bin/sh\nfor last; do :; done\nexport HOME='${far}/home' ASTRUM_RUNTIME_DIR='${sb.farRuntime}' ASTRUM_CONFIG_DIR='${far}/cfg' ASTRUM_KEYCHAIN_FILE='${far}/keychain.json'\nexport ASTRUM_HOLD_LINGER_MS=300 ASTRUM_HOLD_HOST=build-box PATH="$HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"\nunset ASTRUM ASTRUM_SESSION ASTRUM_TAB ASTRUM_DATA_DIR\ncd "$HOME" && exec /bin/sh -c "$last"\n`,
        { mode: 0o755 },
      );
    },
    env: (sb) => ({ ASTRUM_SSH: join(sb.root, 'far', 'ssh') }),
    // (Its shells are kept there when the session here ends: that is the point of it.)
    after: async (sb, run) => {
      await run('pkill', ['-f', `${sb.root}/far/`]).catch(() => {});
      rmSync(sb.farRuntime, { recursive: true, force: true });
    },
    actions: ['until:❯', ...run('git status -sb'), ...cmd('R'), 'until:New tab on another machine', 'text:build-box', 'key:enter', 'until:ada@build-box', 'wait:600', ...run('systemctl status inventory', 500), 'line:journalctl -fu inventory', 'until:only 2 on hand', 'wait:800'],
  },
];
