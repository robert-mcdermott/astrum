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
- **Any model.** Ollama and every OpenAI-compatible endpoint, Anthropic, and
  Amazon Bedrock. Switch with `/model`.
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

The same session can be open in several terminal windows at once. It takes
the size of the window you used last; the others show as much of it as fits.

Colors: terminals that support 24-bit color get each theme's exact colors.
Others (such as the built-in macOS Terminal on older systems) get the nearest
match from the 256-color palette. If colors look reduced over SSH, where
terminals often cannot announce 24-bit support, set `truecolor = true` in the
config.

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
| `bedrock` | AWS credentials: a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`, or access keys in the environment, or a profile in `~/.aws` |

Any other OpenAI-compatible endpoint (a local model server, a hosted or
company gateway) can be added in the config file.

Credentials are read from the environment when a session's background server
starts. After setting or changing one, restart the session: `astrum kill`,
then `astrum`. `/providers` shows which providers have credentials.

To pick a model, type `/model` and choose from the list, or name it directly:
`/model anthropic/claude-haiku-5-5`. The choice is remembered. To make one the
default for every session, set `provider` and `model` under `[agent]` in the
config file.

#### Anthropic

1. Create an API key in the Claude Console.
2. Set it in the shell you start Astrum from. To keep it for new terminals,
   put the line in your shell's startup file:

   ```bash
   export ANTHROPIC_API_KEY=sk-ant-...
   ```

3. Start (or restart) the session, press `Ctrl+Space`, and type `/model`.
   Models from this provider are labelled `anthropic`.

The model list comes from your account, so new models appear without an
Astrum update. On models that support it, the model's reasoning is shown as a
running summary on the status line while it thinks.

For Anthropic's Fable, Opus and Sonnet 5.x models, Astrum opts in to
server-side fallback: if a safety classifier declines a request, the API
re-runs it on another model instead of stopping.

To use a different variable name or a gateway in front of the API:

```toml
[providers.anthropic]
kind = "anthropic"
api_key_env = "MY_ANTHROPIC_KEY"
# base_url = "https://gateway.example.com"
```

#### Amazon Bedrock

Bedrock needs AWS credentials and a region. Point Astrum at them in the
config file:

```toml
[providers.bedrock]
kind = "bedrock"
region = "us-west-2"
profile = "my-profile"     # a profile from ~/.aws/credentials or ~/.aws/config
```

Then start (or restart) the session and type `/model`. Models from this
provider are labelled `bedrock`.

Astrum looks for credentials in this order and uses the first it finds:

1. A Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`.
2. `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (with `AWS_SESSION_TOKEN`
   if set) in the environment. These are skipped when the provider names a
   `profile`.
3. An AWS profile: the provider's `profile` setting, else `AWS_PROFILE`, else
   `default`. Access keys in the profile, a `credential_process`, single
   sign-on and assumed roles all work. Single sign-on and assumed roles are
   resolved through the AWS CLI, which must be installed; run
   `aws sso login --profile my-profile` first.

The region comes from the provider's `region` setting, then `AWS_REGION` or
`AWS_DEFAULT_REGION`, then the profile's own region, and is `us-east-1` if
none of those is set. Set it explicitly if your models are enabled elsewhere.

Things to know:

- **Model names.** `/model` lists the models your account can call in that
  region. Most current models are offered through cross-region ids that start
  with a geography, such as `us.anthropic.claude-haiku-5-5` or
  `global.anthropic.claude-sonnet-5-5`; use those ids as listed.
- **Which models work.** Astrum uses Bedrock's Converse API, so models from
  any vendor work as long as they support streaming with tool use. One that
  does not is refused by Bedrock with a message saying so.
- **Permissions.** The credentials need `bedrock:InvokeModelWithResponseStream`
  to run the agent, and `bedrock:ListInferenceProfiles` and
  `bedrock:ListFoundationModels` to fill the `/model` list. Without the
  listing permissions, name models yourself with
  `models = ["us.anthropic.claude-haiku-5-5"]` under `[providers.bedrock]`.
- **Temporary credentials.** Astrum re-reads profile credentials every ten
  minutes, so refreshed ones are picked up without a restart. When they
  expire, Bedrock's error appears in the agent panel; refresh them and ask
  again.

## Configuration

```bash
astrum config --init
```

writes a commented `~/.config/astrum/config.toml`. Everything is optional:

```toml
theme = "tokyo-night"
theme_terminal = false     # true: recolor terminal content with the theme too
prefix = "ctrl+b"
agent_key = "ctrl+space"   # the key that switches between shell and agent
scrollback = 10000
# truecolor = true         # default: detected per terminal

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

## Troubleshooting

- **`cargo` or `astrum` not found.** `~/.cargo/bin` is not on your `PATH`; add
  `. "$HOME/.cargo/env"` to your shell's startup file.
- **`Ctrl+Space` does something else.** macOS uses it to switch input sources
  when several keyboard layouts are enabled, and some shell plugins bind it.
  Pick another key with `agent_key` in the config, for example
  `agent_key = "ctrl+backslash"` or `agent_key = "f2"`.
- **Colors look reduced.** Astrum did not detect 24-bit color support, which is
  common over SSH. Set `truecolor = true` in the config.
- **Cannot select text in a full-screen program.** Hold `Shift` while dragging.
- **A new API key is not picked up.** The background server reads the
  environment when it starts. Run `astrum kill`, then `astrum`.
- **Bedrock models are missing from `/model`, or requests are refused.** Check
  that `region` under `[providers.bedrock]` is the region where your models are
  enabled; without it Astrum uses your profile's region or `us-east-1`. The
  bottom line of the `/model` list names any provider it could not reach.
- **Something looks stuck.** `astrum ls` shows running sessions and
  `astrum kill` stops one. The server's log is in `/tmp/astrum-<uid>/`.

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
| `src/agent/` | providers (including AWS signing and stream decoding), tools, command-safety policy, the turn loop |
| `shell/` | shell-integration scripts (embedded in the binary) |
| `tests/e2e.rs` | end-to-end tests through a pseudo-terminal |

## Development

```bash
cargo test
```

This runs the unit tests and the end-to-end tests in `tests/e2e.rs`, which
start the real binary on a pseudo-terminal and type at it the way a terminal
app does. They use their own HOME, config and runtime directories and a
stand-in model server, so they need no network and leave your sessions alone.

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
