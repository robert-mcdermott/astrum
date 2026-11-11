# Changelog

Notable changes to Astrum, newest first. Each release corresponds to one wave
in [ROADMAP.md](ROADMAP.md).

## 0.1.0 — unreleased

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

- The Anthropic provider is written to the published API format but has not
  yet been run against the live service. Amazon Bedrock has been, with
  access keys from an AWS profile; Bedrock API keys and single sign-on
  profiles are untried.
- Tested on macOS (Apple silicon) and Linux (arm64). Linux on x86-64 and the
  clipboard on a Linux desktop are untried.
- All terminals attached to a session share one view.
- Config changes take effect when a session starts.
