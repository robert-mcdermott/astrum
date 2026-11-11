# Astrum

A persistent terminal with tabs, mouse support and a built-in AI agent. It runs
inside whatever terminal app you already use, locally or over SSH, as a single
self-contained binary.

- **Feels like your normal shell.** Run `astrum` and you are in your usual
  zsh/bash/fish with your own prompt and colors, plus a tab bar.
- **Sessions survive.** Close the window, come back later, run `astrum` again:
  every tab, running program and scrollback is still there.
- **Easy for everyone.** Click tabs, click `+`, click `✕`, scroll with the
  wheel, drag to select and copy. Keyboard shortcuts exist for people who want them.
- **An agent that shares your shell.** Press `Ctrl+Space` and ask. It reads what
  is on screen, proposes commands and runs them in your real session, where you
  can watch and take over. You decide how much it may do on its own.
- **Any model.** Ollama and every OpenAI-compatible endpoint, plus Anthropic.
  Switch with `/model`. (Amazon Bedrock is planned for v0.1.0.)
- **Themes.** Fourteen built in, with live preview: `/theme`.

Astrum does not depend on any other program at run time. See
[ROADMAP.md](ROADMAP.md) for where it is going and what is not done yet.

## Install

Requires a Rust toolchain (1.85 or newer).

```bash
cargo install --path . --locked
```

`--locked` builds with the exact dependency versions recorded in `Cargo.lock`,
the ones Astrum is tested with. Or build without installing; the binary is
`target/release/astrum`:

```bash
cargo build --release --locked
```

If `cargo` or `astrum` is "not found" afterwards, `~/.cargo/bin` is not on
your `PATH`; add `. "$HOME/.cargo/env"` to your shell's startup file.

## Use

```bash
astrum
```

| Do this | With the mouse | With the keyboard |
| --- | --- | --- |
| New tab | click `+` | `Ctrl+B` then `c` |
| Switch tab | click it (or wheel over the tab bar) | `Ctrl+B` then `n` / `p` / `1`–`9` |
| Close tab | click its `✕`, or middle-click | `Ctrl+B` then `x`, or `exit` in the shell |
| Rename tab | | `Ctrl+B` then `,` |
| Scroll back | mouse wheel | `Shift+PageUp` / `Shift+PageDown` |
| Copy text | drag (copies on release); double-click a word | |
| Talk to the agent | click the `SHELL` chip | `Ctrl+Space` |
| Leave, keep everything running | close the window | `Ctrl+B` then `d` |
| Help | click `? help` | `Ctrl+B` then `?` |

Programs that use the mouse themselves (vim, htop) receive it as usual; hold
`Shift` while dragging to select text inside them.

Sessions:

```bash
astrum -s work
```

```bash
astrum ls
```

```bash
astrum kill -s work
```

## The agent

`Ctrl+Space` moves the keyboard between the shell and the agent panel. The
status bar always shows which one you are typing into.

- Type a request and press Enter. `Shift+Enter` (or `Ctrl+J`) adds a line.
- The agent's commands run in the terminal pane above, in your own shell, so
  `cd`, environment variables and virtualenvs carry over, and you see the real
  output. If something asks for a password, press `Ctrl+Space` and type it yourself.
- `!command` runs a shell command directly without involving the model.
- `Esc` stops the agent (and interrupts the command it was running).

How much it may do on its own (`Shift+Tab`, `/autonomy`, or click the chip):

| Level | Behavior |
| --- | --- |
| **ask first** (default) | Asks before every command and file change. |
| **auto read-only** | Runs look-only commands (`ls`, `git status`, `df`, …) unasked; asks for anything that changes things. |
| **autopilot** | Runs commands itself. Still asks for destructive ones (`sudo`, `rm -rf` on broad paths, force-push, disk tools, piping downloads into a shell). Lasts until the session ends unless set in the config file. |

When it asks, you can approve, approve for the rest of the session, edit the
command first, or decline and tell it what to do instead.

Slash commands: `/model`, `/theme`, `/autonomy`, `/clear`, `/copy`,
`/providers`, `/rename`, `/config`, `/help`.

### Models

`/model` lists what your providers offer. Out of the box Astrum knows about:

| Provider | Needs |
| --- | --- |
| `ollama` | Ollama running on `localhost:11434` |
| `openai` | `OPENAI_API_KEY` |
| `anthropic` | `ANTHROPIC_API_KEY` |

Any other OpenAI-compatible endpoint (a local model server, a hosted or
company gateway) can be added in the config file. Set API keys in the environment
before the session starts; the background server reads them when it launches.

For Anthropic's Fable, Opus and Sonnet 5.x models, Astrum opts in to
server-side fallback: if a safety classifier declines a request, the API
re-runs it on another model instead of stopping.

## Configuration

```bash
astrum config --init
```

writes a commented `~/.config/astrum/config.toml`. Everything is optional:

```toml
theme = "tokyo-night"
theme_terminal = false     # true: recolor terminal content with the theme too
prefix = "ctrl+b"
agent_key = "ctrl+space"
scrollback = 10000

[agent]
provider = "ollama"
model = "deepseek-v4.1-flash:cloud"
autonomy = "ask"           # ask | safe | auto
instructions = "Prefer ripgrep over grep."

[providers.work]
kind = "openai"
base_url = "https://llm.example.com/v1"
api_key_env = "WORK_LLM_KEY"
models = ["my-model"]
```

Choices made inside Astrum (`/model`, `/theme`, autonomy) are remembered in
`state.toml` next to it. Config changes apply when a session starts
(`astrum kill`, then `astrum`).

Themes: `astrum`, `nebula`, `tokyo-night`, `catppuccin`, `catppuccin-latte`,
`gruvbox`, `nord`, `dracula`, `rose-pine`, `kanagawa`, `everforest`,
`one-dark`, `solarized`, and `terminal` (uses your terminal app's own colors).

## How it works

`astrum` is a thin client. The first run starts a background server per
session that owns the shells (PTYs), emulates each terminal, runs the agent,
and draws the whole interface; the client forwards your keys and mouse and
writes the frames it gets back. Closing the client leaves the server and your
shells running.

A small shell hook (zsh, bash, fish) marks where each command starts and ends
and reports the working directory. That is how new tabs open in the same
folder, and how the agent gets exactly the output and exit code of a command.
The hook is loaded automatically and leaves your prompt and config files alone.

Source layout:

| Path | What |
| --- | --- |
| `src/client.rs`, `src/proto.rs` | thin client and wire protocol |
| `src/server/` | session server: event loop, tabs, input routing, agent orchestration |
| `src/pane.rs`, `src/osc.rs`, `src/input.rs` | PTY + emulation, shell-integration scanner, key/mouse encoding |
| `src/ui/` | rendering: tab bar, pane, agent panel, pickers |
| `src/agent/` | providers, tools, command-safety policy, the turn loop |
| `shell/` | shell-integration scripts (embedded in the binary) |

## Development

```bash
cargo test
```

`astrum debug-snapshot` is a headless client for testing without a terminal:
it attaches to a session, plays scripted input, and prints the resulting
screen (optionally as HTML with colors).

```bash
ASTRUM_CONFIG_DIR=/tmp/astrum-test-cfg astrum debug-snapshot -s test --do 'line:echo hi' --do 'key:ctrl+space' --do 'wait:300' --html /tmp/snap.html
```

Actions: `text:`, `line:`, `key:ctrl+b`, `paste:`, `click:COL,ROW`,
`drag:C,R-C,R`, `scrollup:C,R`, `resize:COLSxROWS`, `wait:MS`, `until:TEXT`,
`snap:LABEL`. `ASTRUM_CONFIG_DIR` keeps test settings away from your own.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
