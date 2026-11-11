# Astrum roadmap

## What Astrum is

A terminal session manager with a built-in terminal agent, for people who are
not terminal-multiplexer experts. You start it inside whatever terminal app you
like; it looks and behaves like your normal shell, keeps your sessions alive in
the background, gives you tabs you can click, and has an agent one keystroke
away that works in the same shell you do.

## Principles

1. **Invisible until wanted.** By default it is your shell with a tab bar. No
   prompt rewriting, no changed colors, no config edits.
2. **Mouse and keyboard are equals.** Everything reachable by a shortcut is
   also clickable, and the screen says what the shortcuts are.
3. **One self-contained binary.** No external multiplexer, no helper daemons,
   no runtime dependencies, no vendor SDKs. Astrum's own code is all Rust; the
   only native code is the TLS library's cryptography, compiled as part of the
   build, so `cargo` and a C compiler are all it takes.
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
| A pure-Rust terminal emulation library | Fast and mature, and it adds nothing to the build. Emulators that require another language's toolchain conflict with principle 3. |
| Server draws the whole UI and ships cell diffs inside synchronized updates | Simplest correct model; no tearing. Drawing the tab bar and status bar in the client scales better with several clients (see wave 3). |
| Shell hooks emit OSC 133 / OSC 7 marks carrying a per-pane secret | Gives exact command boundaries, output and exit codes, and program output cannot forge them. |
| Agent commands are typed into the user's live shell | Shared state (cwd, env), visible work, and the user can take over. Falls back to a subshell when the terminal is busy. |
| Short wait, then hand control back to the model with `command_status` / `send_input` | Long-running and interactive commands never block the conversation. |
| Layered approval: destructive always asks → read-only allowlist → autonomy level | A local read-only classifier decides, rather than trusting the model's own judgment of what is safe. |
| Approval card with edit-first and decline-with-guidance | Saying no should redirect the agent, not just stop it. |
| Shell-first, agent one key away | Astrum must feel like a plain shell by default. |

## How the work is organized

Work happens in **waves**. A wave becomes a release when it is complete:
wave 1 ships as v0.1.0, wave 2 as v0.2.0, and so on. Inside a wave, each
logical group of work is a **slice**. The last slice of every wave is release
prep, followed by a pull request into `main`.

Each wave is developed on its own branch (`wave-1`, `wave-2`, …), with slices
landing there as they are finished.

| Wave | Release | Theme | State |
| --- | --- | --- | --- |
| 1 | v0.1.0 | A dependable first version | Released 2026-11-10 |
| 2 | v0.2.0 | Agent depth | In progress |
| 3 | v0.3.0 | Multiplexer depth | Planned |
| 4 | v0.4.0 | Polish and distribution | Planned |
| 5 | v0.5.0 | Beyond the terminal | Planned |

Slices in later waves are a first cut and will be reshaped when each wave starts.

## Wave 1 → v0.1.0: a dependable first version — released

### Slice 1: foundation — done

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

What slice 1 verified, and what it left for the rest of the wave:

| Area | Status |
| --- | --- |
| Tabs, mouse, scrollback, selection, vim, resize, detach/reattach, session end | Tested with the headless client and through a real pseudo-terminal |
| Agent end to end (approvals, edit/decline, autopilot checks, long-running and interactive commands, file tools, cancel) | Tested against Ollama with `gemma4:31b-cloud` and `deepseek-v4.1-flash:cloud` |
| zsh and bash (3.2) integration | Tested |
| Throughput | About 30 MB/s of output through full emulation; 0% CPU idle |
| Real terminal apps: key handling, `Ctrl+Space`, clipboard, look | Tried by hand in slice 2 (results below) |
| Server surviving the terminal *app* quitting on macOS | Confirmed by hand in slice 2 |
| Anthropic provider | Run live in slice 3 |
| Amazon Bedrock provider | Run live in slice 3 |
| fish integration | Run in slice 4 (fish 3.6 on Linux) |
| Linux | Built and tested in slice 4 (arm64; x86-64 not yet built) |

### Slice 2: real-terminal shakedown — done

- [x] Scripted end-to-end tests that run the real binary on a pseudo-terminal:
      raw keys, mouse reports, resize, hang-up, detach / reattach, and the
      agent flow against a stand-in model server
- [x] The client restores the terminal when it is killed, not only on a normal exit
- [x] Tried by hand on macOS in Ghostty, a libghostty-based terminal, WezTerm,
      iTerm2 and the built-in Terminal. The first three worked and looked
      right; the last two each showed a problem, fixed below
- [x] Fixed: garbled colors in terminals without 24-bit color (the built-in
      macOS Terminal on older systems). Each client now reports whether its
      terminal supports 24-bit color, and those that do not are sent the
      nearest colors from the 256-color palette. `truecolor = true/false` in
      the config overrides the detection
- [x] Fixed: the view not filling the window when the session was also open in
      another, smaller terminal. The session now takes the size of the
      terminal used most recently; other attached terminals show as much of
      the view as fits
- [x] Both fixes covered by scripted end-to-end tests
- [x] Decided: `Ctrl+Space` stays the agent key (changeable with `agent_key`
      in the config)
- [x] Decided: the tab bar and status bar stay visible by default, so
      everything is discoverable and clickable (the status bar can be turned
      off with `status_bar = false`)
- [x] Decided: "ask first" stays the default autonomy level
- [x] Confirmed: sessions survive quitting the terminal app, not just closing a window
- [x] Both fixes confirmed by hand in iTerm2 and the built-in Terminal

### Slice 3: provider completeness

- [x] Amazon Bedrock through the Converse API, so every Bedrock model with
      streaming and tools works: request signing, API-key auth, credential
      lookup (environment, `~/.aws` profiles, single sign-on through the AWS
      CLI), binary stream decoding and model listing, all in-tree with no AWS SDK
- [x] Bedrock verified offline: signing against the published reference
      vectors, stream decoding, and the full agent flow against a stand-in server
- [x] Bedrock run against the live service (us-west-2, a profile with
      temporary credentials): model listing, and full agent turns with tool
      calls on Claude Haiku 5.5, Claude Sonnet 5.5 and Amazon Nova 2 Lite.
      Nothing needed fixing; the model list is now sorted so the regional
      variants of a model sit together. API-key and single sign-on
      credentials have not been tried live
- [x] Anthropic provider run against the live API: model listing, and agent
      turns with tool calls on Claude Haiku 5.5, Sonnet 5.5, Opus 5.5 and
      Fable 5.2, including a declined command and a follow-up turn. Thinking
      summaries stream into the status line and thinking is replayed correctly
      after a tool call. The provider needed no changes; the reasoning preview
      no longer lingers on the status line once a reply has ended
- [x] Clear guidance when a provider has no credentials: the model picker
      marks its models "needs an API key" and names providers it could not
      reach, and using one says which environment variable to set

### Slice 4: release prep, then the PR

- [x] Linux: builds, and the unit and end-to-end tests pass (Debian 12, arm64, in a container)
- [x] fish: the integration tracks the working directory and command boundaries (fish 3.6)
- [ ] Linux on x86-64, and the clipboard on a Linux desktop, are still untried
- [x] Changelog for v0.1.0 (`CHANGELOG.md`; its 0.1.0 section doubles as the release notes)
- [x] README pass: install, quick start, troubleshooting
- [x] Release date set in the changelog
- [x] Pull request from `wave-1` into `main`
- [x] After the merge: tag `v0.1.0` on `main` and publish the release with
      the changelog's 0.1.0 section as its notes

## Wave 2 → v0.2.0: agent depth — in progress

### Slice 1: conversations and context — done

- [x] Conversations persist across server restarts; `/resume`, `/conversations`.
      Each conversation is a file in the data directory, written as it
      changes; a turn that was cut off is closed out when it is reopened.
      `/resume` prefers the conversation from the shell's current directory;
      `/conversations` lists them to reopen or delete. Conversations unused
      for 90 days are deleted (`keep_conversations_days`)
- [x] Context management: `/compact`, automatic compaction near the window
      limit. The model writes a handover summary that takes the
      conversation's place; it runs on request, at 80% of the model's context
      or 200,000 tokens if that is less (mid-task included), and as one retry
      when a provider refuses a request as too long
- [x] Project and user rules (`AGENTS.md`, `~/.config/astrum/AGENTS.md`),
      read at the start of every turn, the user's first and the nearest
      project file last
- [x] Token display per conversation: context fill and tokens in, cached, out
      and in total in the panel header, `/usage` for the breakdown. A model's
      context size comes from `[models."id"]` in the config file, then from
      the provider where it reports one, then a cautious default. Cost was built and then dropped: it needs a price
      list that goes stale, so Astrum counts tokens and leaves prices to the
      provider's own usage page

What slice 1 verified, and what it did not:

| Area | Status |
| --- | --- |
| Saving, `/resume`, `/conversations` (reopen, delete), an interrupted turn | Scripted end-to-end tests against the stand-in model server; restart and resume also run live against Ollama (`deepseek-v4.1-flash:cloud`) |
| `/compact` | End-to-end tests; run live against Ollama, including a follow-up answered from the summary after a restart |
| Automatic compaction, and the retry after a provider's refusal | End-to-end tests only, with a deliberately tiny context window and a stand-in refusal. Not yet seen at a real model's real limit, and the wording providers use for "too long" is matched from their documented errors, not observed |
| Rules files | Unit and end-to-end tests; followed live by the Ollama model |
| Token totals | End-to-end tests; live against Ollama, which reports cached tokens |
| `/resume` choosing by directory | End-to-end test, including a directory reached through a symbolic link |
| Deleting conversations unused for 90 days | Unit and end-to-end tests |
| Learning a model's context size from the provider | Ollama: end-to-end test against the stand-in, and live (262,144 tokens reported for `gemma4:31b-cloud`). Anthropic: only the reading of its documented reply is tested; not run live. Whether Ollama serves a locally run model with less context than it reports is not something that could be checked here (no local models installed), which is why that figure is not used |
| Anthropic and Amazon Bedrock with any of the above | Not run live in this slice (no credentials were used). Their cache-token accounting and their replies to a summary request are covered only by the shared code paths |
| The new screens in a real terminal app | Not tried by hand; seen through the headless client and the scripted tests |

### Slice 2: asking without switching

- [ ] Ask from the shell prompt (`# question`), optional natural-language detection
- [ ] "Explain this" / "fix this" on the last failed command, one key
- [ ] Type-ahead while the agent works: queue or steer instead of "still working"
- [ ] Attach the agent to a command the user started themselves

### Slice 3: tools and safety

- [ ] Search tools (grep, glob) and a unified diff view for edits with per-hunk approval
- [ ] Per-user allow and deny lists for commands; secret redaction before sending
- [ ] Pager defusal for agent commands; process-liveness in status snapshots
- [ ] API keys via the OS keychain; providers reloadable without a restart

### Slice 4: release prep, then the PR

## Wave 3 → v0.3.0: multiplexer depth

### Slice 1: panes and tabs

- [ ] Split panes (click-and-drag borders), zoom
- [ ] Drag to reorder tabs; right-click menus for tabs and panes
- [ ] Agent panel docked to the side on wide terminals

### Slice 2: scrollback and persistence

- [ ] Search in scrollback; keyboard copy mode
- [ ] Restore tabs, directories and scrollback after a reboot
- [ ] Optional launchd / systemd user service so sessions survive logout

### Slice 3: terminal fidelity

- [ ] Each attached terminal gets a fully independent view, with its own
      active tab (today all attached terminals share one view, sized for the
      one used most recently)
- [ ] Pass enhanced (unambiguous) key reporting through to inner programs
- [ ] Report the host terminal's real colors to inner programs (dark/light detection)
- [ ] Hyperlinks (OSC 8) with Ctrl+click; inline images
- [ ] Working-directory tracking without shell integration (process cwd)

### Slice 4: release prep, then the PR

## Wave 4 → v0.4.0: polish and distribution

### Slice 1: settings and customization

- [ ] First-run walkthrough; in-app settings screen
- [ ] Config hot reload; fully remappable keys; user-defined themes in TOML;
      follow the host's light/dark setting

### Slice 2: distribution

- [ ] Prebuilt binaries: macOS universal, static Linux (musl); Homebrew formula
- [ ] Shell completions and a man page

### Slice 3: quality and speed

- [ ] CI with emulation conformance tests and scripted UI tests
- [ ] Damage-tracked rendering and a latency benchmark in CI

### Slice 4: release prep, then the PR

## Wave 5 → v0.5.0: beyond the terminal

- [ ] MCP client so the agent can use external tools
- [ ] Skills / reusable workflows; scheduled and background agent tasks
- [ ] SSH awareness: carry the shell integration into remote hosts
- [ ] Multiple agents across tabs with a shared overview

Slices to be defined when the wave starts; the last one is release prep and the PR.
