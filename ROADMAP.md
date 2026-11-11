# Astrum roadmap

## What Astrum is

A terminal session manager with a built-in terminal agent, for people who are
not terminal-multiplexer experts. You start it inside whatever terminal app you like; it looks
and behaves like your normal shell, keeps your sessions alive in the
background, gives you tabs you can click, and has an agent one keystroke away
that works in the same shell you do.

## Principles

1. **Invisible until wanted.** By default it is your shell with a tab bar. No
   prompt rewriting, no changed colors, no config edits.
2. **Mouse and keyboard are equals.** Everything reachable by a shortcut is
   also clickable, and the screen says what the shortcuts are.
3. **One self-contained binary.** No external multiplexer, no helper daemons,
   no runtime dependencies, no vendor SDKs. Pure Rust all the way down, so it builds with
   `cargo` alone.
4. **The agent shares the user's session** and never acts beyond the autonomy
   the user chose. Destructive commands always ask.
5. **Any model.** Providers are plain HTTP; nothing is tied to one vendor.
6. **Fast.** Input-to-screen latency and scrolling throughput are features.

## Design decisions

These came out of studying how existing terminal multiplexers and terminal
agents are built and where they ran into trouble. Ideas were borrowed; no code was.

| Decision | Why |
| --- | --- |
| Background server + thin client over a Unix socket | Sessions outlive the terminal window. |
| Client sends semantic key/mouse events; server re-encodes them per pane | Passing raw bytes through breaks as soon as inner programs change keyboard or mouse modes. |
| A pure-Rust terminal emulation library | Fast and mature, and the build needs nothing but `cargo`. Emulators that require a second toolchain conflict with principle 3. |
| Server draws the whole UI and ships cell diffs inside synchronized updates | Simplest correct model; no tearing. Drawing the chrome in the client scales better with several clients (see phase 3). |
| Shell hooks emit OSC 133 / OSC 7 marks carrying a per-pane secret | Gives exact command boundaries, output and exit codes, and program output cannot forge them. |
| Agent commands are typed into the user's live shell | Shared state (cwd, env), visible work, and the user can take over. Falls back to a subshell when the terminal is busy. |
| Short wait, then hand control back to the model with `command_status` / `send_input` | Long-running and interactive commands never block the conversation. |
| Layered approval: destructive always asks → read-only allowlist → autonomy level | A local read-only classifier decides, rather than trusting the model's own judgment of what is safe. |
| Approval card with edit-first and decline-with-guidance | Saying no should redirect the agent, not just stop it. |
| Shell-first, agent one key away | Astrum must feel like a plain shell by default. |

## Phase 1: foundation — done

- [x] Session server and thin client; detach / reattach; named sessions;
      `astrum ls`, `astrum kill`
- [x] Tabs: create, switch, close, rename; new tabs open in the current directory
- [x] Mouse: clickable tabs, `+`, `✕`, status chips; wheel scrollback; drag,
      double- and triple-click selection with copy; mouse passthrough to
      full-screen programs; draggable agent panel divider
- [x] Terminal emulation with 10k-line scrollback, alternate screen, bracketed
      paste, focus events, OSC 52 clipboard from inner programs
- [x] Shell integration for zsh, bash and fish, loaded automatically
- [x] Agent panel: streaming replies with Markdown, tool rows, approval cards,
      slash commands with a menu, prompt history, stop/cancel
- [x] Agent tools: `run_command` (in the live shell), `command_status`,
      `send_input`, `read_terminal`, `read_file`, `write_file`, `edit_file`
- [x] Autonomy levels: ask first / auto read-only / autopilot, with a
      destructive-command check that always asks
- [x] Providers: OpenAI-compatible (Ollama, OpenAI, others) and Anthropic,
      streaming with tool calls; `/model` picker fed by the providers
- [x] 14 themes with live preview; a theme that follows the host terminal's colors
- [x] Config file plus remembered in-app choices
- [x] Headless test client (`debug-snapshot`)

### Verified so far, and not

| Area | Status |
| --- | --- |
| Tabs, mouse, scrollback, selection, vim, resize, detach/reattach, session end | Tested with the headless client and through a real pseudo-terminal |
| Agent end to end (approvals, edit/decline, autopilot checks, long-running and interactive commands, file tools, cancel) | Tested against Ollama with `gemma4:31b-cloud` and `deepseek-v4.1-flash:cloud` |
| zsh and bash (3.2) integration | Tested |
| Throughput | About 30 MB/s of output through full emulation; 0% CPU idle |
| Real terminal apps: key handling, `Ctrl+Space`, clipboard, look | **Not yet tried by a person** |
| Anthropic provider | **Written to the current API but not run** (no key on the dev machine) |
| fish integration | **Not run** (fish not installed on the dev machine) |
| Linux | **Not built or run** |
| Server surviving the terminal *app* quitting on macOS | **Not checked** (may need the launchd service planned in phase 3) |

## Phase 2: agent depth and providers

- [ ] Amazon Bedrock: Converse API with SigV4 and bearer-token auth, in-tree
      (no AWS SDK), streaming
- [ ] Run the Anthropic provider against the live API; show thinking summaries
- [ ] Conversations persist across server restarts; `/resume`, `/conversations`
- [ ] Context management: `/compact`, automatic compaction near the window limit
- [ ] Project and user rules (`AGENTS.md`, `~/.config/astrum/AGENTS.md`)
- [ ] Type-ahead while the agent works: queue or steer instead of "still working"
- [ ] Ask from the shell prompt without switching (`# question`), optional
      natural-language detection
- [ ] "Explain this" / "fix this" on the last failed command, one key
- [ ] Attach the agent to a command the user started themselves
- [ ] Search tools (grep, glob) and a unified diff view for edits with per-hunk approval
- [ ] Per-user allow and deny lists for commands; secret redaction before sending
- [ ] Pager defusal for agent commands; process-liveness in status snapshots
- [ ] Token and cost display per conversation
- [ ] API keys via the OS keychain; providers reloadable without a restart

## Phase 3: multiplexer depth

- [ ] Split panes (click-and-drag borders), zoom
- [ ] Drag to reorder tabs; right-click menus for tabs and panes
- [ ] Search in scrollback; keyboard copy mode
- [ ] Restore tabs, directories and scrollback after a reboot
- [ ] Each attached client gets its own view and size (client-side chrome rendering)
- [ ] Pass enhanced (unambiguous) key reporting through to inner programs
- [ ] Report the host terminal's real colors to inner programs (dark/light detection)
- [ ] Hyperlinks (OSC 8) with Ctrl+click; inline images
- [ ] Working-directory tracking without shell integration (process cwd)
- [ ] Optional launchd / systemd user service so sessions survive logout
- [ ] Agent panel docked to the side on wide terminals

## Phase 4: polish and distribution

- [ ] First-run walkthrough; in-app settings screen
- [ ] Config hot reload; fully remappable keys; user-defined themes in TOML;
      follow the host's light/dark setting
- [ ] Prebuilt binaries: macOS universal, static Linux (musl); Homebrew formula
- [ ] Shell completions and a man page
- [ ] CI with emulation conformance tests and scripted UI tests
- [ ] Damage-tracked rendering and a latency benchmark in CI

## Phase 5: beyond

- [ ] MCP client so the agent can use external tools
- [ ] Skills / reusable workflows; scheduled and background agent tasks
- [ ] SSH awareness: carry the shell integration into remote hosts
- [ ] Multiple agents across tabs with a shared overview

## Open questions for the owner

1. **Agent key.** `Ctrl+Space` is the default. It collides with macOS
   input-source switching for people with several keyboard layouts, and with
   some zsh autosuggestion setups. Alternatives: `Ctrl+\`, a function key.
2. **Chrome by default.** A tab bar and a status bar are always shown (two
   rows). The status bar can be turned off; should either hide automatically
   when there is one tab and the agent is closed?
3. **Default autonomy.** "Ask first" is the safest default; "auto read-only"
   is far less interruptive for investigation. Which should new users get?
4. **Bedrock scope.** Anthropic models only (Messages-compatible endpoint), or
   every Bedrock model through Converse?
