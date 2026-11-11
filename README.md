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
| Talk to the agent | click the `SHELL` chip | `Ctrl+Space`, or `# question` at the prompt |
| Explain or fix a failed command | click `explain` or `fix` in the status bar | `Ctrl+B` then `e` / `f` |
| Have the agent watch a running command | click `watch` in the status bar | `Ctrl+B` then `o` |
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
- Reading is never asked about: the agent can read files, find them by name
  and search their text on its own. Its searches skip version-control,
  dependency and build directories (`.git`, `node_modules`, `target` and the
  like).
- `!command` runs a shell command directly without involving the model.
- You can keep typing while the agent works. What you send is held just above
  the prompt and handed to the agent at its next step, with the result of
  whatever it was doing, so you can correct or add to a task without stopping
  it. `Backspace` on an empty prompt takes the last waiting message back.
- `Esc` stops the agent (and interrupts the command it was running). Messages
  still waiting return to the prompt.

### Secrets

What the agent gathers for the model (the output of commands, the files it
reads, what is on the terminal) is checked for secrets first, and each one
is replaced with a note such as `[redacted: GitHub token]`. Your terminal
still shows the real output; only the copy sent to the model changes, and
the panel says how many were hidden. It looks for:

- keys and tokens with a recognizable shape (cloud and source-hosting
  access keys, API keys, private key blocks, signed web tokens);
- passwords in URLs and in `Authorization` headers;
- lines that set something named like a secret (`DB_PASSWORD=...`,
  `--api-key=...`), as in an `env` listing or a `.env` file;
- the API keys Astrum itself is using for your providers, anywhere.

It is deliberately cautious about code, so that `token = get_token()` stays
readable, which means a secret that looks like an ordinary word in an
ordinary assignment can get through. Treat it as a safety net, not a
guarantee, and note that what you type to the agent yourself is sent as
written. `redact_secrets = false` under `[agent]` turns it off.

### Asking without leaving the shell

You do not have to switch to the agent panel to ask something. At the shell
prompt, start a line with `#` and a space:

```
# why is this directory so large
```

Press Enter and the line goes to the agent instead of the shell. The panel
opens to show its work, and the keyboard stays in the shell. If the agent is
already busy, the question waits for its next step, like anything typed in
the panel.

This needs the shell integration (zsh, bash and fish have it), and it only
applies to a line you type at a prompt, not to input for a running program.
Lines such as `#!/bin/sh` or `##` are left alone. Under `[agent]` in the
config file, `ask_prefix` chooses another prefix, and `ask_prefix = ""`
turns this off.

With `natural_language = true` there, a line that reads as plain language
goes to the agent without any prefix: several words, none of the shell's own
punctuation, starting with a word such as "how", "why" or "please", and
either ending in a question mark or not starting with the name of a program
on your machine. It is off by default because it is a guess. When it guesses
wrong, start the line with a space to make the shell run it as typed.

Astrum works out what you typed from the keys you pressed. For a line you
edited along the way (history, completion, cursor keys) it reads the line
from the screen, which works when the prompt has not changed since you began
typing. If it cannot tell, the line goes to the shell as usual.

**When a command fails**, the status bar shows its exit code with two
offers, `explain` and `fix`. Click one, or press `Ctrl+B` then `e` or `f`.
The agent is given the command, its exit code and its output, and either
tells you what went wrong or sets about fixing it (asking first, as your
autonomy level requires). The offer goes away once the agent has seen that
failure or you run something else. `/explain` and `/fix` do the same from
the panel, and work on the last command even when it succeeded.

**While a command of your own is running**, the status bar offers `watch`
(`Ctrl+B` then `o`, or `/watch`). The agent then follows that command and
tells you how it went when it ends, so you can leave a long build or
download and be told about the result. `/watch then run the tests` adds what
to do afterwards. It will not interrupt your command, and stopping the agent
with `Esc` leaves the command running.

How much it may do on its own (`Shift+Tab`, `/autonomy`, or click the chip):

| Level | Behavior |
| --- | --- |
| **ask first** (default) | Asks before every command and file change. |
| **auto read-only** | Runs look-only commands (`ls`, `git status`, `df`, …) unasked; asks for anything that changes things. |
| **autopilot** | Runs commands itself. Still asks for destructive ones (`sudo`, `rm -rf` on broad paths, force-push, disk tools, piping downloads into a shell). Lasts until the session ends unless set in the config file. |

When it asks, you can approve, approve for the rest of the session, edit the
command first, or decline and tell it what to do instead.

Two lists under `[agent]` in the config file adjust this for particular
commands:

```toml
[agent]
allow = ["cargo test", "git commit", "npm run *test*"]
deny = ["git push", "terraform apply"]
```

An entry is the opening words of a command, so `git push` covers
`git push origin main`; `*` stands for anything. A command on the allow list
runs without asking at any level, provided everything else on the same line
is allowed too and nothing is redirected to a file. A command on the deny
list is never run, on autopilot included: the agent is told it is ruled out
and that it should not look for another route. The deny list is checked
first, and it sees through `sudo`, `xargs`, `sh -c "..."` and the like.
Destructive commands still ask even when allow-listed. These lists steer an
agent that is trying to follow them; they are not a sandbox.

A change to a file is shown as a diff before anything is written: the lines
going out in red, the lines coming in green, with a few unchanged lines
around each change. A long diff scrolls with `PgUp` / `PgDn` or the mouse
wheel. When a change touches several places you can also choose **Review
each change** and go through them one at a time, applying some and skipping
others; the file is written once you have answered them all, and the agent
is told which parts you declined. If the file changes on disk while the
question is open, nothing is written and the agent is told to look again.

Slash commands: `/model`, `/theme`, `/autonomy`, `/clear`, `/resume`,
`/conversations`, `/explain`, `/fix`, `/watch`, `/compact`, `/usage`,
`/copy`, `/providers`, `/rename`, `/config`, `/help`.

### Conversations

Each tab has its own conversation, and conversations are saved as they
happen, so they outlive the session: after `astrum kill`, a reboot or a crash,
start Astrum again and pick up where you left off.

- `/resume` reopens the most recent conversation from the directory your
  shell is in. If there is none from there, it reopens your most recent one
  from anywhere and says so. `/resume words` does the same among the
  conversations whose first prompt contains those words.
- `/conversations` lists every saved conversation, newest first. Type to
  filter, Enter to open, `Del` twice to delete one.
- `/clear` starts a new conversation; the old one stays in the list.

A resumed conversation continues in the tab's current shell and directory. If
it was cut off in the middle of a turn, whatever had not finished is marked as
interrupted and the agent is told so.

Conversations are stored in `~/.local/share/astrum/conversations/`, one file
each, readable only by you. They contain what you asked, what the agent
replied and the output of the commands it ran. A conversation that has not
been used for 90 days is deleted; change that with `keep_conversations_days`
under `[agent]` in the config file (`0` keeps them forever). To keep nothing
on disk at all, set `save_conversations = false` there.

### Long conversations

A model can only take in so much at once, and every request re-sends the
conversation so far, so a long one gets slow and expensive and eventually
does not fit. Compacting fixes that: the model writes a summary of the
conversation (what you asked for, what was done, the current state, what
comes next) and the summary takes the conversation's place.

- `/compact` does it now. `/compact keep the exact error messages` tells the
  summary what to take particular care over.
- It also happens by itself when the conversation reaches 80% of the model's
  context or 200,000 tokens, whichever comes first, even in the middle of a
  task, and the agent carries on from the summary. If a provider refuses a request as too long, Astrum compacts and
  sends it again.

The summary is shown in the panel when it is made, so you can see what the
agent kept, and everything above it stays in view for you to scroll back to;
only what is sent to the model changes. Rules files are not part of the
conversation and are unaffected.

The 200,000-token cap is there because every message re-sends the whole
conversation, so a model that can take a million tokens gets slow and
expensive long before it is full. Under `[agent]` in the config file,
`compact_limit` changes the cap (`0` removes it, leaving only the 80% rule)
and `auto_compact = false` leaves compacting to `/compact`. The 80% rule
relies on Astrum knowing the model's context size; see below.

### Tokens

The top of the agent panel shows, for the conversation in that tab, how full
the model's context is and how many tokens the conversation has used: sent to
the model (in), how much of that the provider served from its prompt cache
(cached), received (out), and the total. `/usage` gives the same breakdown in
full. The totals are saved with the conversation. Astrum does not turn them
into a cost: prices differ by provider and change, so check your provider's
own usage page for that.

How full the context is depends on how much the model accepts. Astrum
finds that out as follows, using the first that applies:

1. A `context_window` you set in the config file:

   ```toml
   [models."my-local-model"]
   context_window = 32000
   ```

   Name the table `"provider/model"` to cover one provider only.
2. What the provider reports, asked once per model when you first use it:
   Anthropic describes each model's input limit, and Ollama running on the
   same machine lists it for the models it forwards to a hosted service.
3. Otherwise an assumption: 128,000 tokens for OpenAI-compatible models and
   200,000 for Anthropic and Amazon Bedrock.

A model that Ollama runs on your own machine falls under 3 even though
Ollama reports a size for it, because Ollama serves a local model with the
context it was configured to give it, which can be much less than the
model's maximum. Set `context_window` for those. A figure that is too low
only means compacting earlier than necessary; one that is too high means the
conversation will not be compacted in time by itself.

### Rules

Standing instructions for the agent go in plain Markdown files named
`AGENTS.md`:

- `~/.config/astrum/AGENTS.md` holds your own rules, for every project: how
  you like things done, tools you prefer, things never to touch.
- An `AGENTS.md` in a project holds that project's rules: how to build and
  test it, its conventions. Astrum reads the one in the shell's current
  directory and those in the directories above it, up to the top of the
  repository (the directory holding `.git`), or up to your home directory
  when there is no repository.

The files are read at the start of every turn, so an edit applies from your
next message, and the agent panel says which files it is following. When
rules disagree, the file closest to the current directory wins, and what you
ask for in the conversation wins over all of them. Short, specific rules work
best; each file is limited to 32 KB.

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
`snap:LABEL`. `ASTRUM_CONFIG_DIR` keeps test settings away from your own, and
`ASTRUM_DATA_DIR` does the same for saved conversations.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
