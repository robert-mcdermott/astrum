# Changelog

Notable changes to Astrum, newest first. Each release corresponds to one wave
in [ROADMAP.md](ROADMAP.md).

## 0.2.0 — unreleased

Agent depth. In progress on the `wave-2` branch; see [ROADMAP.md](ROADMAP.md).

### Agent

- After a command fails, the status bar offers `explain` and `fix`
  (`Ctrl+B e` / `Ctrl+B f`, `/explain`, `/fix`): the agent gets the command,
  its exit code and its output.
- `watch` (`Ctrl+B o`, `/watch`) has the agent follow a command you started
  yourself and report when it ends.
- Ask from the shell prompt: a line starting with `# ` goes to the agent
  instead of the shell, and the keyboard stays where it is. `ask_prefix`
  changes or disables the prefix; `natural_language = true` also sends lines
  that read as plain language.
- Type while the agent works: a message sent mid-turn reaches the agent at
  its next step instead of being refused, so a task can be steered without
  stopping it.
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
- The agent panel shows how full the context is and the conversation's
  tokens: in, cached, out and total. `/usage` breaks it down. A
  model's context size is taken from the provider where it reports one
  (Anthropic, and Ollama's hosted models), and a `[models."id"]` table in
  the config file sets it for any model.
- Rules files: the agent follows `~/.config/astrum/AGENTS.md` and the
  `AGENTS.md` files of the project you are in, and says which ones apply.

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
