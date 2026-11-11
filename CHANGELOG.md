# Changelog

Notable changes to Astrum, newest first. Each release corresponds to one wave
in [ROADMAP.md](ROADMAP.md).

## 0.2.0 — unreleased

Agent depth: conversations that last, ways to ask without leaving the shell,
and more tools with more safeguards around them.

### Conversations and context

- Conversations are saved as they happen and survive the session ending.
  `/resume` reopens the most recent one from the current directory (or from
  anywhere, if there is none); `/conversations` lists them all, to reopen or
  delete. Conversations unused for 90 days are deleted
  (`keep_conversations_days`); `save_conversations = false` turns saving off.
- `/compact` replaces the conversation so far with a summary the agent
  continues from. It happens by itself when the model's context is nearly
  full or the conversation reaches 200,000 tokens, mid-task included, and
  when a provider refuses a request as too long. `auto_compact` and
  `compact_limit` control it.
- Rules files: the agent follows `~/.config/astrum/AGENTS.md` and the
  `AGENTS.md` files of the project you are in, and says which ones apply.
- The agent panel shows how full the context is and the conversation's
  tokens: in, cached, out and total. `/usage` breaks it down. A model's
  context size is taken from the provider where it reports one (Anthropic,
  and Ollama's hosted models), and a `[models."id"]` table in the config
  file sets it for any model.

### Asking without switching

- Ask from the shell prompt: a line starting with `# ` goes to the agent
  instead of the shell, and the keyboard stays where it is. `ask_prefix`
  changes or disables the prefix; `natural_language = true` also sends lines
  that read as plain language.
- After a command fails, the status bar offers `explain` and `fix`
  (`Ctrl+B e` / `Ctrl+B f`, `/explain`, `/fix`): the agent gets the command,
  its exit code and its output.
- `watch` (`Ctrl+B o`, `/watch`) has the agent follow a command you started
  yourself and report when it ends.
- Type while the agent works: a message sent mid-turn reaches the agent at
  its next step instead of being refused, so a task can be steered without
  stopping it.

### Tools and safety

- Search tools: the agent can find files by name pattern and search the
  text of files itself, without asking and without running `find` or `grep`.
  Version-control, dependency and build directories are skipped.
- File changes are shown as a diff before they are made, and a change that
  touches several places can be approved one piece at a time.
- `allow` and `deny` lists for commands in the config file: allow-listed
  commands run without asking, deny-listed ones are never run.
- Secrets (keys, tokens, passwords) in command output, files and terminal
  text are replaced with a note before they are sent to the model.
  `redact_secrets = false` turns this off.
- A pager that one of the agent's commands opens is closed after its first
  screen instead of waiting for a key forever.
- Reports on a command that is still running say which program it is, when
  it last printed, and whether it has been using the processor.
- A question from the agent that appears while you are in the middle of
  typing is not answered by the keys already on their way.

### Providers

- `/key` stores a provider's API key in the system's password store (the
  macOS keychain, or the desktop keyring on Linux) instead of a config file
  or shell startup script.
- `astrum reload` and `/reload` bring new providers, model settings, stored
  keys and (from the shell) changed key variables into a running session
  without restarting it.

### Themes

- Six more themes, twenty in all: Outrun, Blade Runner 2049, Cyberpunk 2077,
  Chaos Theory, Matrix and Monokai.

### Fixed

- A client whose terminal went away (its window was closed, or the
  connection it came over dropped) could stay behind and use a whole
  processor core until it was killed by hand. It now exits within half a
  second. The session it was attached to was never affected and still
  carries on, as intended. This bug is in 0.1.0.

### Upgrading from 0.1.0

- A session that is already running keeps running the old server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. Until then the new client works with the old server, but none of
  the features above are there and `astrum reload` is refused.
- If an `astrum` process from 0.1.0 is using a whole processor core, it is a
  client left behind by a closed terminal window (see Fixed). It ignores an
  ordinary `kill`; use `kill -9` on it. Your sessions are not affected.
- The slash-command menu has new entries, so two shortcuts changed: `/r` and
  Enter now picks `/resume` (it was `/rename`), and `/co` and Enter picks
  `/conversations` (it was `/copy`).
- Asking from the shell prompt is on by default: a line that starts with `#`
  and a space is sent to the agent rather than treated as a comment. Set
  `ask_prefix = ""` under `[agent]` to keep such lines for the shell.

### Known limitations

- `/key` has been tested against a stand-in for the password store, not
  against a real macOS keychain or Linux keyring.
- The Anthropic and Amazon Bedrock providers were not run live in this
  release; what changed for them (token accounting, context-size lookup,
  summaries for `/compact`) is covered by tests of the shared code only.
- Secret redaction and the deny list are safeguards, not guarantees: a
  secret that looks like an ordinary word in ordinary code can get through,
  and the deny list only sees the commands it can pick out of a line.
- Asking from the shell prompt recognizes a line typed at a prompt. A line
  typed before the prompt has appeared, or edited at a prompt that redraws
  itself, goes to the shell as before.
- Plain-language detection (`natural_language`) is a guess and is off by
  default.

## 0.1.0 — 2026-11-10

The first release: a persistent terminal with tabs, mouse support and a
built-in agent, as one self-contained binary.

### Sessions and tabs

- A background server per session keeps your shells running when the terminal
  window, or the whole terminal app, is closed. Run `astrum` again to return.
- Named sessions (`astrum -s work`), `astrum ls`, `astrum kill`.
- Tabs: create, switch, close and rename, from the keyboard (`Ctrl+B` prefix)
  or with the mouse. New tabs open in the current directory.
- The same session can be open in several terminal windows; it takes the size
  of the one used most recently.

### Terminal

- Full terminal emulation with 10,000 lines of scrollback, so editors, pagers
  and other full-screen programs work as usual.
- Mouse: wheel scrollback; drag, double-click and triple-click selection that
  copies on release; mouse events pass through to programs that use them.
- Shell integration for zsh, bash and fish, loaded automatically, that tracks
  the working directory and where each command starts and ends.
- 24-bit color where the terminal supports it, with an automatic fallback to
  the 256-color palette where it does not.
- 14 color themes with live preview, including one that follows the terminal
  app's own colors.

### Agent

- `Ctrl+Space` switches between the shell and the agent. The agent's commands
  run in your live shell, where you can watch them and take over.
- Three autonomy levels: ask first (default), auto-run read-only commands, and
  autopilot. Destructive commands always ask.
- Approval prompts let you approve, approve for the session, edit the command
  first, or decline and say what to do instead.
- Tools for running commands, following long-running ones, typing into
  interactive programs, reading the terminal, and reading and editing files.
- Providers: any OpenAI-compatible endpoint (including Ollama), Anthropic, and
  Amazon Bedrock. `/model` lists what your providers offer.
- Slash commands: `/model`, `/theme`, `/autonomy`, `/clear`, `/copy`,
  `/providers`, `/rename`, `/config`, `/help`.

### Configuration

- `~/.config/astrum/config.toml` for the theme, keys, scrollback, agent
  defaults and providers. Choices made inside Astrum are remembered separately.

### Known limitations

- Anthropic and Amazon Bedrock have both been run against the live services,
  Bedrock with access keys from an AWS profile. Bedrock API keys and single
  sign-on profiles are untried, as is the OpenAI service itself (Ollama's
  OpenAI-compatible endpoint is what has been used).
- Tested on macOS (Apple silicon) and Linux (arm64). Linux on x86-64 and the
  clipboard on a Linux desktop are untried.
- All terminals attached to a session share one view.
- Config changes take effect when a session starts.
