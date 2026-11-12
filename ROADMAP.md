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
| 2 | v0.2.0 | Agent depth | Released 2026-11-11 |
| 3 | v0.3.0 | Multiplexer depth | Released 2026-11-11 |
| 4 | v0.4.0 | Polish and distribution | In progress |
| 5 | v0.5.0 | Remote machines, and beyond the terminal | Planned |

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

## Wave 2 → v0.2.0: agent depth — released

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

### Slice 2: asking without switching — done

- [x] Ask from the shell prompt (`# question`), optional natural-language
      detection. Enter on a line that starts with `# ` sends it to the agent
      and leaves the keyboard in the shell; `natural_language = true` does the
      same for lines that read as plain language. What was typed is known
      from the keys, or read from the screen for a line edited along the way,
      so no shell needs its prompt or key bindings changed
- [x] "Explain this" / "fix this" on the last failed command: offered in the
      status bar when a command fails (click, `Ctrl+B e`, `Ctrl+B f`), and as
      `/explain` and `/fix`
- [x] Type-ahead while the agent works: a message sent mid-turn is held and
      handed to the model at its next step, with the results of what it was
      doing, or continues the turn if it was about to finish
- [x] Attach the agent to a command the user started themselves: `watch` in
      the status bar while a command runs (`Ctrl+B o`, `/watch`); the agent
      follows it and reports, and never interrupts it

What slice 2 verified, and what it did not:

| Area | Status |
| --- | --- |
| Type-ahead: mid-step, at the end of a turn, and returned to the prompt on stop | Scripted end-to-end test; run live against Ollama (`gemma4:31b-cloud`) while the agent waited on a command |
| `# question` at the prompt | End-to-end tests in bash; live in zsh with plain typing and with a line edited using the cursor keys |
| Plain-language detection | Unit tests of the rules and an end-to-end test in bash. It is a guess by design and off by default; not tried on real day-to-day input |
| Explain, fix | End-to-end test (click and keys); explain run live in zsh. Fix not run live |
| Watch | End-to-end test against the stand-in; live in zsh, with the agent waiting on an 8-second command |
| fish | Not run in this slice; run in slice 4 (fish 3.6, in a container) |
| Prompts that redraw themselves, right-hand prompts, vi mode | Not tried. An edited line is only recognized when the prompt text is unchanged since typing began; otherwise it goes to the shell as usual |
| Keys typed before a prompt has appeared | Known limit: such a line is not recognized as a question and goes to the shell (seen live when typing during zsh's startup) |
| How the line is taken back from the shell | Changed in slice 4: with the line editor's erase keys, after Ctrl+C was found to hang bash 3.2 now and then |

### Slice 3: tools and safety — done

- [x] Search tools (grep, glob) and a unified diff view for edits with
      per-hunk approval. `find_files` and `search_files` walk the tree
      in-process and run without asking. A file change is worked out first
      and shown as a diff; with several hunks they can be applied or skipped
      one at a time
- [x] Per-user allow and deny lists for commands; secret redaction before
      sending. `allow` and `deny` in the config file match commands by their
      opening words, deny first and seen through `sudo`, `xargs`, `sh -c`.
      Keys, tokens and passwords in what the agent collects are replaced
      with a note before the model sees them
- [x] Pager defusal for agent commands; process-liveness in status
      snapshots. A pager the agent's command stops in is closed after its
      first screen. A still-running report names the foreground program and
      says when it last printed and how much processor time it has used
- [x] API keys via the OS keychain; providers reloadable without a restart.
      `/key` stores a key through the system's own tool (`security`,
      `secret-tool`); `astrum reload` and `/reload` re-read providers, model
      settings and keys in a running session
- [x] Added along the way: keys typed while an approval question pops up
      are not taken as its answer

What slice 3 verified, and what it did not:

| Area | Status |
| --- | --- |
| Search tools | Unit and end-to-end tests; used unprompted by `deepseek-v4.1-flash:cloud` (through Ollama) in a live run |
| Diff view and per-hunk approval | Unit tests of the diff and its partial application; end-to-end test (scroll, review each, approve whole, decline, no-op, autopilot); the card seen live with a real model's edit |
| Allow and deny lists | Unit and end-to-end tests. It matches the commands it can see in a line; it is a guard rail for a cooperating agent, not a sandbox |
| Secret redaction | Unit tests per pattern, including that ordinary code is left alone; end-to-end test that no request carries the secret. A safety net, not a guarantee: a secret that looks like an ordinary word in ordinary code gets through |
| Pager closing | End-to-end test with `less`. `man` and `more` prompts are recognized by their wording but were not run |
| Process liveness | macOS: unit and end-to-end tests. The Linux code (`/proc`) first ran in slice 4, where the same tests pass |
| Stored keys (`/key`) | End-to-end tests against a file-backed stand-in for the password store. **The real macOS keychain and the Linux keyring have not been used**: the `security` and `secret-tool` commands are written from their documentation, and only `security`'s reading of commands from standard input was checked (with its help command) |
| Reload | End-to-end test: new provider, key variable from the calling shell, `/reload`, unreadable config, session not running. An `astrum reload` sent to a 0.1.0 server is expected to be refused with a message; not tried |
| Linux, fish | Not in this slice; both run in slice 4 |

### Slice 4: release prep, then the PR

- [x] Linux: builds, and the unit and end-to-end tests pass (Debian 12,
      arm64, in a container), which also runs the Linux-only process
      inspection for the first time
- [x] fish 3.6: the shell integration, the explain / fix offer, and
      `# question` at the prompt, including a line corrected mid-way
- [x] Builds with Rust 1.88 as well as the current toolchain; the release
      build links nothing beyond the system's own libraries
- [x] Upgrading tried both ways: a 0.2.0 client with a 0.1.0 server and the
      reverse. `astrum reload` against a 0.1.0 server is refused with a
      message
- [x] An independent review of everything the wave changed. It found twelve
      defects, all fixed, each with a test where one could be written:
      - a key being typed for `/key` could be left in the prompt as plain
        text, and `/key` repeated its arguments back
      - on autopilot the agent could type a command at the shell's prompt,
        past the deny list and the destructive-command check
      - the deny list missed subshells, options before a subcommand
        (`git -C repo push`), wrapper options with values (`sudo -u x`),
        and `bash -lc`
      - redaction missed the context lines of a search, paths with spaces,
        diff lines, and a private key whose end was out of view
      - the typing guard did not cover keys typed in the shell
      - a new empty file was reported as unchanged instead of created
      - the pager rescue could close a pager the user had open themselves,
        and stopping the agent after `/watch` did not stop its own command
      - the client's exit watchdog misread a terminal opened as `/dev/tty`
      - a `# question` corrected mid-line was cut off at the cursor
      - compaction could repeat at every step when it could not help
- [x] Found while testing those fixes: sending Ctrl+C to drop the line of a
      `# question` could leave bash 3.2 stuck for good (about one time in
      twelve when the line arrived in a burst). The line is now erased with
      the shell's own editing keys. And in bash, Enter on an empty prompt
      was counted as a command (a 0.1.0 bug in the shell integration)
- [x] Version 0.2.0; changelog grouped by what changed for the user, with
      upgrade notes and known limitations
- [x] README pass: configuration, troubleshooting, source layout
- [x] Release date set in the changelog
- [x] Pull request from `wave-2` into `main`
- [x] After the merge: tag `v0.2.0` on `main` and publish the release with
      the changelog's 0.2.0 section as its notes
- [x] Decided after the release:
      - `Ctrl+B o` stays the key for watch (`w` already closes a tab)
      - plain-language detection stays as it is: off by default, a guess
      - a key stored with `/key` ranks above the environment variable
      - a command on the allow list still asks when it looks destructive
      - the search tools keep their fixed list of skipped directories
      - compacting by itself shows one line, with `/summary` for the rest;
        `/compact` shows the summary at once
- [ ] Still untried: the real macOS keychain and Linux keyring; the
      Anthropic and Amazon Bedrock providers with this wave's changes; Linux
      on x86-64; the new screens in a real terminal app by hand

## Wave 3 → v0.3.0: multiplexer depth — released

### Slice 1: panes and tabs — done

- [x] Split panes (click-and-drag borders), zoom. A tab holds a tree of
      splits; `Ctrl+B |` and `Ctrl+B -` split, `Ctrl+B` with an arrow or a
      click moves between panes, `Ctrl+B z` zooms, `Ctrl+B x` closes. The
      agent is bound to one pane for the length of a turn and follows the
      keyboard otherwise
- [x] Drag to reorder tabs; right-click menus for tabs and panes. Also
      `Ctrl+B <` and `Ctrl+B >` for the keyboard
- [x] Agent panel docked to the side on wide terminals (140 columns and up
      by default; `agent_panel` in the config file to fix it either way)

What slice 1 verified, and what it did not:

| Area | Status |
| --- | --- |
| Splitting both ways, nested; focus by key and by click; zoom; dragging a divider; closing a pane by key and by its shell exiting | Unit tests of the layout tree; scripted end-to-end test in bash; looked at in a rendered screenshot |
| The agent with several panes | End-to-end test: its command runs in the pane it started in while the user works in another, and it follows the keyboard afterwards. Not run against a live model |
| Reordering tabs, right-click menus | End-to-end test (drag, keys, mouse and keyboard in the menus) |
| Panel beside the shell | End-to-end test (wide, dragged, narrowed, fixed by config); looked at in a rendered screenshot |
| Everything that was there before | The whole earlier suite passes unchanged on top of the pane refactor |
| Tried by hand | By the owner on the day it was built, in a real terminal and mostly with the mouse: splitting, resizing by dragging, zoom, and watch. All worked |
| Not tried | On Linux; in fish; mouse-using programs (vim, htop) inside a split; very small windows beyond what the layout tests cover; tab dragging, the right-click menus and the side panel have only been through the scripted tests |
| Left for slice 2 | Pane layouts are not saved: a restarted session starts with one pane per tab |

### Slice 2: scrollback and persistence — done

- [x] Search in scrollback; keyboard copy mode. `Ctrl+B /`, the status-bar
      menu or a right-click opens a search bar on the pane with buttons for
      the mouse; `Ctrl+B [` gives the keyboard a cursor to select and copy with
- [x] Restore tabs, directories and scrollback after a reboot. Also the
      pane layout of each tab, tab names, and the conversation each tab had
      open. Written out as the session goes, so a crash is covered too.
      Closing the last shell ends a session for good; `astrum kill --forget`
      discards one; `restore = all | layout | off` chooses how much is kept
- [x] Optional launchd / systemd user service so sessions survive logout:
      `astrum service install | uninstall | status`. On Linux, with
      lingering on, the session runs through a logout. On macOS a logout
      ends every process of the user's, so the service starts the session
      at login and it comes back restored
- [x] Added along the way: a `≡ menu` button on the status bar listing
      every `Ctrl+B` action, and `Ctrl+B` hints that can be clicked and stay
      for ten seconds

What slice 2 verified, and what it did not:

| Area | Status |
| --- | --- |
| Search: finding as you type, stepping by key and by the bar's buttons, wrapping round, case, the three ways in, closing | End-to-end test; looked at in a rendered screenshot |
| Copy mode: moving, selecting, copying, a search from inside it, its keys staying out of the shell | End-to-end test |
| Restore: tabs, names, pane layout, directories, scrollback with colors, the front tab and the focused pane | Unit test of saving and replaying a screen; end-to-end test in bash; tried by hand in zsh |
| Restore after a crash (server killed outright) and after the shells are killed from outside | End-to-end test. A real reboot or logout has not been tried |
| A session whose last shell is closed is forgotten; `--forget`; `restore = layout` and `off` | End-to-end test |
| A restored tab's conversation | End-to-end test with the stand-in model |
| The service definitions | Unit tests; the launchd one also passes the system's own checker (`plutil`) |
| A server started as a service: waits for a session already running, takes over when it stops | End-to-end test |
| Linux | The whole suite (86 unit, 54 end-to-end) passes on Debian 12 arm64 in a container |
| The systemd service | Tried for real in a Debian 12 container running systemd: installed, ran with no login session (lingering on), restarted by systemd after `astrum kill` with its tab restored, uninstalled |
| Tried by hand | By the owner, in a real terminal: a session with split panes was stopped and started again, and the splits came back |
| Not tried | Linux x86-64; `astrum service install` on macOS itself (it adds a login item to the real machine, so it waits for the owner's say); a macOS logout or reboot with the service in place; fish; a pane that is in a full-screen program at the moment of saving (by design it keeps the scrollback saved before the program started) |

### Slice 3: terminal fidelity — done, except inline images

- [x] Each attached terminal gets a fully independent view, with its own
      active tab: its own menus, prefix, pointer and notices, drawn at its
      own size. A tab has one size, so two terminals on the same tab share
      it, sized for the one that used it last; the other sees it at that
      size, with a note, until it types or clicks in it
- [x] Pass enhanced (unambiguous) key reporting through to inner programs:
      the kitty keyboard protocol, with repeats and releases, every key as
      an escape code, and associated text, each where a program asks for it
- [x] Report the host terminal's real colors to inner programs (dark/light
      detection): asked of the terminal when a client attaches; the theme's
      colors when `theme_terminal` is on
- [x] Hyperlinks (OSC 8) with Ctrl+click. Also web addresses in plain text;
      a right-click menu with Open link and Copy link, for the mouse and for
      terminals that keep Ctrl+click to themselves; the target shown in the
      status bar on hover
- [ ] Inline images. Not built. Astrum draws every frame itself as a grid
      of characters, and the terminal library it uses for emulation drops
      image data. Showing images means keeping them beside the grid,
      following them as output scrolls, and re-sending them to each attached
      terminal in whichever of three incompatible image protocols it
      understands, around menus and split panes. That is a subsystem of its
      own, and whether it looks right can only be judged on a real terminal.
      It needs a slice to itself, and is now slice 3 of wave 4
- [x] Working-directory tracking without shell integration (process cwd):
      the foreground shell's directory, asked of the system; also covers a
      shell started inside the pane's own

What slice 3 verified, and what it did not:

| Area | Status |
| --- | --- |
| Independent views | End-to-end test with two terminals of different sizes: own frames, own tabs, own menus and prefix, a shared tab taken over by typing and by clicking and not by looking, a tab closing under both, detaching and returning. Every earlier test passes unchanged on top of it |
| Enhanced keys | Unit tests of the encoding against the protocol's forms; end-to-end test of a program asking, checking, receiving, and letting go |
| Terminal colors | Unit test of reading a terminal's answers; end-to-end test with a stand-in terminal that answers, and with `theme_terminal` |
| Links | End-to-end test: hover, Ctrl+click, the menu's Open and Copy, a marked link whose text differs from its target, a link that is refused |
| Directory without integration | Unit test on this process; end-to-end test with a plain `sh` and with a shell inside bash |
| Linux | The whole suite (89 unit, 58 end-to-end) passes on Debian 12 arm64 in a container, including the directory lookup through `/proc` |
| zsh | A short run by hand in an isolated session: a split, a second terminal of another size with its own tab |
| Tried by hand | By the owner, in a real terminal: two windows on one session, with the "in use in another window" note showing and working as described |
| Not tried | By hand: links, the terminal's colors, enhanced keys, and the directory lookup. In particular: what a real terminal answers when asked its colors (only a stand-in has been asked); a real program that uses enhanced keys (Neovim, Helix, fish 4) rather than a script; Ctrl+click in the owner's terminal, which may keep it for itself; three or more terminals at once; Linux x86-64 |

### Slice 4: release prep, then the PR

- [x] An independent review of everything the wave changed, by two
      reviewers. Eighteen findings; seventeen fixed, each with a test where
      one could be written, and one recorded as a limitation:
      - narrowing a pane while a search was open could bring the server
        down, shells and all
      - a link to a file was handed to the system to open, which can mean
        running it; only web and mail links are opened now
      - a tab's right-click menu acted on a position, so it could close the
        wrong tab when another terminal changed the tabs meanwhile
      - a session could be forgotten at shutdown if its shells ended by
        themselves just before the server was told to stop, and could be
        kept when everything was closed soon after a shell had been killed
      - a split too small for both halves could hide the pane the keyboard
        was in
      - a second terminal of the same size on a shared tab had its first
        click land in the wrong pane, and pointer movement over a tab laid
        out for another terminal was passed to programs at the wrong cells
      - page up and page down in copy mode moved up to two pages
      - removing or replacing the login service from inside the session it
        runs cut the command off half-way; the launchd log was under
        `/tmp`, which is emptied at start-up; a dollar sign in a systemd
        environment value was doubled
      - closing another pane moved an agent that was still following a
        command of its own
      - enhanced keys: Enter, Tab and Backspace reported releases they
        should not, and asking for releases alone changed how keys were
        spelled
      - the count of matches could disagree with the highlighted match;
        blanks with a background color were dropped from saved scrollback;
        a malformed color answer could overflow; saved files were not
        flushed before replacing the old ones; looking for a link under the
        pointer scanned a very long line whole
      - recorded, not fixed: a program that asks for every key as an escape
        code is not told when plain letter keys are let go, or about
        modifier keys by themselves, and alternate key values are not sent
- [x] Found while testing those fixes: one of the new tests ran `astrum
      service uninstall` expecting a refusal that did not come, and so
      reached the real service manager of the machine the tests ran on. It
      did nothing there (it asked to unload a service that did not exist),
      but an install would have. `ASTRUM_SERVICE_MANAGER=none` now keeps
      the service commands from running anything, and the tests cannot
      turn it off
- [x] Upgrading tried both ways: a 0.3.0 client with a 0.2.0 server and the
      reverse (attach, type, new tab, list, reload, stop)
- [x] A live agent turn in a split pane (Ollama, `deepseek-v4.1-flash:cloud`):
      the command ran in the pane it was asked from
- [x] Type-checks with Rust 1.88 as well as the current toolchain; no new
      packages in the build; the release build links only the system's
      own libraries
- [x] Linux: the whole suite (91 unit, 62 end-to-end) passes on Debian 12,
      arm64, in a container, at the last code commit
- [x] The systemd service, for real, in a Debian 12 container running
      systemd: installed; a second install changes nothing; removing it
      from inside its own session is refused; it runs with no login
      session; Stopped session `default`. has systemd start it again with its tab
      restored; uninstalled
- [x] fish 4.0.2 (Debian 13), a real program that asks for enhanced key
      reports: editing keys, interrupting a command, the directory, search,
      a split, and stopping and coming back all work, and fish's own key
      reader names Shift+Enter as a key of its own, which it can only do
      with the enhanced reports
- [x] Version 0.3.0; changelog grouped by what changed for the user, with
      upgrade notes and known limitations; README pass
- [x] Inline images moved to wave 4 as a slice of their own
- [x] Release date set in the changelog
- [x] Pull request from `wave-3` into `main`
- [x] After the merge: tag `v0.3.0` on `main` and publish the release with
      the changelog's 0.3.0 section as its notes
- [x] Decided after the release:
      - a new theme, McDermott, built around `#ff0068` for long hours in a
        dark room
      - out of the box Astrum uses the terminal's own colors, and a chosen
        theme colors what programs draw as well (`theme_terminal` on)
- [ ] Still untried: `astrum service install` on macOS itself, and a macOS
      logout or reboot with it in place; a real reboot; links, the
      terminal's colors and enhanced keys by hand in a real terminal; three
      or more terminals at once; Linux on x86-64

## Wave 4 → v0.4.0: polish and distribution — released

### Slice 1: settings and customization — done

- [x] First-run walkthrough: four short cards on what is where, shown the
      first time Astrum is used on a machine (no config or state file
      yet), with buttons, and again on request from the settings
- [x] In-app settings screen: the common settings as rows to click (the
      menu, `Ctrl+B .`, `/settings`). A change applies at once and is kept
      in `state.toml`, which now holds only what was chosen inside Astrum
- [x] Config hot reload: the file is looked at once a second and applied
      to the running session when it changes; a file with a mistake in it
      changes nothing and the mistake is shown
- [x] Fully remappable keys: every action has a name, and a `[keys]` table
      gives it other keys, after the prefix or without it. `astrum keys`
      lists them; the hint line, menu and help show the keys as set
- [x] User-defined themes in TOML: `[themes.NAME]`, any colors, on top of
      a theme named in `based_on`
- [x] Follow the host's light/dark setting: the default theme is the
      terminal's own colors, which follows by itself; for the others,
      `theme_light` names the theme for a terminal whose background is
      light, read when a terminal attaches
- [x] Before the slice, by the owner's request: the McDermott theme, and
      the defaults changed to the terminal's own colors with a chosen
      theme coloring programs too

What slice 1 verified, and what it did not:

| Area | Status |
| --- | --- |
| Keys | Unit tests of reading keys, the defaults and overrides; end-to-end test with a changed keymap (a direct key, a prefix key, a freed key, the hint line, the menu, `astrum keys`) |
| Themes of the user's own; light and dark | Unit test of building on a theme and of what is reported; end-to-end test with a dark and a light stand-in terminal, the theme list, and what is saved |
| Hot reload | End-to-end test: a theme, a key and the prefix changed under a running session; a broken file; a file with an oddity |
| Settings screen | End-to-end test: opened three ways, rows changed by mouse and keys, saved, kept over a restart and over a config change, forgotten; looked at in a rendered screenshot |
| Walkthrough | End-to-end test: shown on a first run, clicked and keyed through, not shown again, shown from the settings, not shown to someone with a config file |
| Linux | The whole suite (96 unit, 68 end-to-end) passes on Debian 12 arm64 in a container |
| Not tried | Any of it by hand in a real terminal; a real terminal switching between light and dark while attached (the theme follows at the next attach, not at once); an editor that saves by renaming, on Linux |

### Slice 2: distribution — done

Installing Astrum should not take a Rust toolchain. A release is built by
CI and installed with one command.

- [x] Release workflow: pushing a version tag has CI check that the tag,
      `Cargo.toml` and the changelog agree, run the tests, build the
      binaries, and attach them with a `SHA256SUMS` file to a draft release
      whose notes are that version's changelog section. The draft is looked
      over and published by hand. A push to a wave branch that changes the
      workflow or the install script rehearses all of it without making a
      release
- [x] Binaries for macOS on Apple silicon, and Linux on x86-64 and ARM64.
      The Linux ones fully static (musl), so that one file runs on any
      distribution
- [x] `install.sh` in the repository, run as `curl … | sh`: picks the file
      for this machine, downloads it and `SHA256SUMS` from the release,
      verifies the SHA-256 checksum and that the program runs and reports
      the expected version, and only then moves it into `~/.local/bin`. No
      `sudo`, no change to `PATH`, and a failed download never replaces a
      working copy. A version and a directory can be given
- [x] CI on every push and pull request: build and the test suite on macOS
      and both Linux architectures, the static build, the install script
      through `shellcheck`, and a build with the oldest Rust supported
- [x] README: installing from a release first, building from source second;
      how to check a download by hand
- [x] Shell completions (`astrum completions bash|zsh|fish`) and a manual
      page (`astrum man`), kept in step with the commands by a test
- [ ] Later, if wanted: a Homebrew formula; a macOS build for Intel

What slice 2 verified, and what it did not:

| Area | Status |
| --- | --- |
| The install script | Tests that run it for real against a release laid out in a directory: a good install, a wrong checksum, a missing or doubled entry, a file that does not run or is something else, a wrong version, and requests it should refuse. On macOS (bash as `sh`) and on Debian 12 (dash), where it is also clean under `shellcheck` |
| A static Linux binary | Built for ARM64 in a Debian 12 container: one file, statically linked, runs a session, and was installed by the script for an ordinary user, manual page included |
| Completions and the manual | A test that each names every command and option; bash completing real command lines; the zsh and fish scripts parse in those shells; the manual renders with `man` |
| The oldest Rust | 1.88 builds it; 1.85, 1.86 and 1.87 do not (the README had said 1.85) |
| Found on the way | A terminal taking over a tab laid out for another ran its first command at the old size; fixed |
| The workflows on GitHub | Both pass on the wave branch. CI: the tests on macOS (Apple silicon) and on Linux x86-64 and ARM64, the static build, the script check, the oldest Rust. The release rehearsal: all three binaries built and tested, the Linux ones static, the macOS one running on macOS 11 and later, `SHA256SUMS` written, and the x86-64 binary installed from those files by the install script |
| What the first runs turned up | The macOS 14 runner image had been retired days before, so the macOS jobs never started: the workflows now use the current image, and the release build states the oldest macOS it runs on and checks the binary agrees. `file` calls the x86-64 static binary "static-pie linked", not "statically linked". Two tests expected bash's own "command not found", where Ubuntu says "Command 'how' not found". One test typed for a fixed time around a question and failed on a slow runner; it now types until the question is on screen. What it had shown was real: typing that ran on for more than 1.2 seconds past a question could answer it. Keys are now ignored for as long as the typing continues, an answer needs a pause of a second, and Ctrl+C always stops. A later run failed on a resize the session never saw: the event reader drops a change of size that arrives together with input, which a split window does to a real terminal. The client now asks the terminal its size each time round, and a test resizes and sends a focus report together 150 times. The run after that failed in the test that had failed twice before for no known reason: its marker, `e2e-42`, is also in the name of the scratch directory whenever the process number starts with 42. Running four copies of the suite at once then found three more tests that counted on the machine's speed (a position measured before the screen had cleared, `exit` typed a fixed number of times on a timer, a command sent before the agent's turn was over); all now wait for what they need. One is left as it is: the test of shells ending at shutdown depends on the 400 ms the server allows for being told to stop, and can miss it on a machine many times overloaded |
| Not tried | Drafting a release, which happens only for a version tag: the first will be 0.4.0. Installing from a real release with the one-line command, which needs that release |

### Slice 3: inline images — done

- [x] Pictures that programs send are taken out of their output before
      the emulator, which drops them: iTerm2's file sequence (in one piece
      or several), kitty's graphics commands (sent whole or in chunks,
      placed, and deleted) and sixel
- [x] A picture is kept by its pane and given room in the grid. The cells
      it covers are marked, so the emulator itself carries it along: it
      scrolls, is cleared, wraps and goes into the history with the text
      around it, and nothing follows it separately
- [x] Each attached terminal is sent a picture in a way it has, where
      there is one without decoding the picture: a file as the file it is,
      a PNG also in kitty's way, raw pixels also as a PNG that is written
      without compressing, sixel only as sixel. Elsewhere a faint block of
      the same size says what the picture is
- [x] Drawn whole or not at all (a terminal cannot be asked for part of a
      picture): not while partly off its pane or under a menu, and only
      once it has been still for a moment, so that scrolling output does
      not send it again for every line
- [x] The client asks its terminal what it can show when it attaches;
      `ASTRUM_IMAGES` overrides the answer, and `images = false` turns
      pictures off
- [x] Programs are told what they ask: kitty's query, sixel in the answer
      to "what are you", and the size of a cell in pixels (which was never
      answered before)
- [x] What a program sent is checked to be data and nothing else before
      it goes into a sequence for the user's terminal. A file or shared
      memory a program names is not read. Limits on how much is kept
- [x] Judged by eye on real terminals: the owner tried it in Ghostty,
      WezTerm and iTerm2, and it works. The built-in Terminal, which shows
      no pictures, was not reported on
- [ ] Later, if wanted: decoding, so that a JPEG reaches a terminal with
      only kitty's way and a picture can be shown in part; pictures that
      change many times a second; kitty's placements inside text

What slice 3 verified, and what it did not:

| Area | Status |
| --- | --- |
| Reading the three kinds | Unit tests: each kind ended either way, cut at every possible point between reads, cut short by another sequence, too large; everything that only looks like a picture is handed on unchanged and in order |
| Sizes | Unit tests: PNG, GIF and JPEG sizes read from the file; sixel measured from its bands; the cells a picture takes for each way of asking, and when it has to shrink |
| Sending on | Unit tests of the bytes for each kind of terminal, of a large picture in pieces, and of the PNG written around raw pixels (its structure and checksums, not by opening it in a viewer) |
| In a session | End-to-end tests with a stand-in terminal that says what it can show: a picture drawn in the right cells, drawn again where scrolling took it, gone when partly off screen, back when looking through the history; taken away under a menu; two terminals each getting what it can show; kitty's query, placement, move without sending again, and delete; sixel and its cursor; the saved session and the agent's view free of the marks |
| A real program | `wezterm imgcat` run inside a session: it asked for the cell size, which showed that question was unanswered (it crashed), and with the answer its picture took the right room |
| On real terminals | Tried by the owner in Ghostty, WezTerm and iTerm2: it works. Which programs and which kinds of picture were used was not recorded |
| Not tried | The built-in Terminal, where a line should stand in for each picture. `kitten icat`, `img2sixel` and a file manager with previews (not installed here). Pictures in two terminals of different cell sizes at once |

### Slice 4: quality and speed — done

- [x] Emulation conformance tests, run in CI with the rest. The same bytes
      go to a pane and straight to an emulator of the same kind, and what
      the terminal Astrum draws on ends up showing is compared with it
      cell by cell: character, colors, style, cursor. A table of cases,
      each also held to what the screen should say; and long runs of
      anything at all from fixed seeds, checked after every burst
- [x] Scripted UI tests in CI: the end-to-end suite, since slice 2
- [x] A way to measure: the session counts what it spends on reading
      output and on drawing (`astrum debug-work`), and a test reports that
      and how long a key takes to reach the screen
- [x] Keys before output. What terminals send has a line of its own, taken
      first; the line for program output is short; output is taken in
      bulk only until a frame is due
- [x] Nothing is sent to a terminal when nothing it shows has changed, and
      only a frame that was sent counts toward the pace of frames
- [x] A terminal that is not taking what it is sent gets no more until it
      has: one frame, of how things are by then
- [x] Damage-tracked rendering. A pane keeps the cells it was last drawn
      as; only the rows the emulator says were written to are worked out
      again. Rows of the frame that are the same as last time are passed
      over when finding what to send
- [x] The measurement runs in CI on every push, with its numbers in the
      run's summary and loose limits that fail the run if one is far out

What slice 4 measured (a release build on the development Mac, a terminal
of 220 by 60, the previous build and the new one run in turn):

| | Before | After |
| --- | --- | --- |
| A key at an idle prompt, to the screen | 8.0 ms (95th: 9.7) | 2.0 ms (95th: 3.8) |
| The same with the window in four panes | 7.9 ms | 2.0 ms |
| A key while another pane pours out text | 57 ms; 31 ms on a quieter machine | 6.8 ms (95th: 8.0) |
| Frames queued for a terminal that stopped reading for 2.5 s | 172 frames, 5.6 MB | at most 3 frames |
| A frame while typing: the pane's rows kept, and not | 630 to 695 µs to draw | 270 to 305 µs |
| Two megabytes of output pouring past | about 410 ms | about 370 ms |
| A full screen repainted 200 times | about 1.3 s | about 1.3 s |

What slice 4 verified, and what it did not:

| Area | Status |
| --- | --- |
| Found by the conformance tests | Three faults with double-width characters, all older than this slice: half of one at the last column of a pane spilled onto the divider or the next row; one parted from its other half by an insert or delete could lose the character after it; and the second half of one stayed on the terminal as a colored blank when another was drawn a cell to its left. A frame now says what each such character does on a terminal. And one fault in the emulator library, recorded as a test: erasing from the start of the screen with the cursor on the second row leaves the first row |
| Found by measuring | The 8 ms: each key had an empty frame sent for it at once, and the frame with the echo waited out the pace. The first way of timing a key (any frame after it) hid this, and showed 1.3 ms |
| The kept rows | 80 random runs of the conformance test since they went in, checked after every burst, with none out of step. They are not used while there is a selection, a search or the history on screen |
| On Linux | The whole suite and the measurement in a Debian 12 container on ARM64: a key at an idle prompt 3.3 ms, beside a flood 9.3 ms |
| In CI | The whole suite on macOS and both Linux runners, and the speed job on GitHub's x86-64 Linux runner: a key at an idle prompt 3.6 ms, beside a flood 7.6 ms, a frame while typing 0.36 ms, well inside its limits (40 ms, 80 ms, 6 ms) |
| Not tried | A terminal on a slow connection for real: the stand-in is a client that is stopped. Latency as a person sees it, which adds the terminal's own drawing and the screen |
| Left as it is | Converting every cell of a full repaint, which nothing short of the program writing less can avoid; and the first, empty drawing after a key, which costs time only when the shell answers faster than a frame is drawn |

### Slice 5: release prep, then the PR

- [x] An independent review of everything the wave changed, by two
      reviewers. Twenty-six findings. Twenty-three fixed, each with a test
      where one could be written; one was wrong; two are recorded as
      limitations:
      - a sixel picture was given the room it declared, not the room it
        draws in: one declaring a pixel and drawing a screenful painted
        over everything
      - the marks a picture leaves in the grid are a program's to write,
        and one that wrote them by hand could have a picture drawn over
        the tab bar or another pane; a picture is now looked for only
        inside the pane that holds it
      - a change of theme left rows nothing had written to in the old
        colors, and a theme of your own edited in the config file took
        effect only if its background or text color changed
      - autopilot set by the config file could not be taken away by it,
        and "Forget the choices made here" left autopilot on
      - after passing through "its tabs only", panes that stayed quiet
        were not written out again and came back empty
      - sessions overwrote each other's choices in `state.toml`
      - keys: two actions swapping keys were reported as a conflict;
        `shift` was accepted and dropped where a terminal does not report
        it; `ctrl+backslash` and its like did not fire where a terminal
        reports them as `ctrl+4`; the prefix followed by a key with Ctrl
        still held did nothing; the status bar's help key and one hint
        ignored the bindings
      - the sample config had `shell` below its example tables, where
        uncommenting it made it part of one
      - a walkthrough put away by a click outside it came back; turning
        "ask from the shell prompt" off and on replaced a prefix of your
        own; in a short window the last settings could not be reached
        with the mouse
      - pictures: every one past the eighth looked through the whole
        history on the server's one loop; one sent again under a number in
        use left the old one in place; one reaching the last column was
        spoiled by the next character written
      - a file full of bell characters was a frame apiece; keys in a
        pane's search box, and keys other than letters in the agent's
        prompt, did not count as typing for a question that appeared next
      - the installer refused an Apple silicon Mac whose shell runs under
        Rosetta; running the release workflow again for a published
        release replaced its files; `astrum ls` removed the socket of a
        session that was slow to answer
      - wrong: that the release step would fail on Linux for the way one
        `sed` command is written. Tried with GNU sed in a container, it
        works
      - recorded, not fixed: the size of the terminal's cells is learned
        when attaching, so a change of font size is not seen until the
        next attach; a picture's placement number in kitty's way is not
        followed
- [x] Upgrading tried both ways: a 0.4.0 client with a 0.3.0 server and
      the reverse (attach, type, new tab, list, reload, stop)
- [x] A live agent turn (Ollama, `deepseek-v4.1-flash:cloud`): the agent
      ran a command that printed a picture and reported a 40 by 40 PNG,
      which it can only know from what Astrum tells it in the picture's
      place
- [x] Type-checks with Rust 1.88 as well as the current toolchain; no new
      packages in the build since 0.3.0; the release build links only the
      system's own libraries
- [x] The release workflow's own checks run by hand against this version:
      the tag, `Cargo.toml` and the changelog agree, and the notes it cuts
      from the changelog are the 0.4.0 section
- [x] Version 0.4.0; changelog with upgrade notes and known limitations;
      README pass; release date set
- [x] Linux: the whole suite (112 unit, 87 end-to-end, 5 of the
      installer), the static build, and the installer putting 0.4.0 in
      place for an ordinary user, on Debian 12 (ARM64, in a container) at
      the last code commit
- [x] Pull request from `wave-4` into `main`. One check failed on its
      first run: a test that asked its next question before the agent's
      turn was over, on a hosted runner slow enough to show it. The test
      now waits; every check passed on the commit that was merged
- [x] After the merge: `v0.4.0` tagged on `main`. The tag had CI test and
      build the three binaries and draft the release with the changelog's
      0.4.0 section as its notes: the first release made that way, and it
      worked the first time. Published
- [x] The one-line install from the README, run against the published
      release on an Apple silicon Mac: it downloaded the binary, verified
      the checksum, and installed 0.4.0; the file is the one in the
      release's `SHA256SUMS`
- [ ] Still untried: the one-line install on Linux from the real release
      (the script and the same binaries were tried there from a
      directory); the built-in Terminal with pictures; a terminal on a
      slow connection for real; a picture drawn by a program that forges
      the marks (the fix is reasoned, not tested)

## Wave 5 → v0.5.0: remote machines — released

Decided during wave 4: working on remote machines comes first in this wave,
in two slices, in this order. The two live side by side afterwards: the
first for an SSH session started by hand, on any server; the second for
remote tabs opened on purpose that should outlast the connection.

### Slice 1: the agent over an SSH session you opened — done

- [x] When a pane is in `ssh`, the status bar offers "take the agent in"
      (also `Ctrl+B i`, the menu, `/here`). Astrum types the marks it
      follows a shell by into the shell that was reached: a line that asks
      what shell it is, a line that has it read one more without showing
      it, and the marks themselves. What was typed is cleared from the
      pane when it has done its work. Nothing is installed on the remote,
      and it lasts as long as that shell
- [x] The agent then runs commands there the way it does locally, with the
      model and the keys still on this machine. It is told which machine
      it is on, and has only the shell there: the tools that read and write
      this machine's files are not offered, so it reads and changes files
      through commands
- [x] The same for a shell reached another way: through a jump host, after
      `sudo -s`, a plain shell started inside another, and a shell inside
      that. Leaving one puts the agent back in the one outside it
- [x] The status bar shows whose shell it is and its directory there; the
      pane's own directory stays what it was, for new tabs and for
      restoring
- [x] The shell there has a secret of its own for its marks, typed in with
      them. The pane's own secret is never sent
- [x] A command the agent runs while `ssh` has the pane and it has not
      been taken in still runs on this machine in a background shell, and
      it is now told so plainly
- [x] Known, and said in the README: bash, zsh and fish only; the session
      ends when the connection does; no diff to approve before a file
      changes there; a remote machine that is not trusted could feed the
      agent false reports of what a command did
- [x] Tried by the owner over SSH to a real server: it worked

What slice 1 verified, and what it did not:

| Area | Status |
| --- | --- |
| The lines typed | Unit tests: each is one line, plain characters, under the shortest keyboard line a system takes (1024 bytes); the three shells are told apart; what a shell says of itself is cleaned before it is shown |
| Whose mark is whose | Unit tests of marks from the pane's own shell and from shells inside it, a secret nobody vouched for, and going back out |
| In a session, bash | End-to-end tests with a shell started inside the pane's own: offered and taken in by a click; the typing cleared; a command run by the agent there and its result; the directory; no file tools, and a refusal if one is asked for all the same; back out; two shells deep and out again; a shell that is none of the three; a program that never answers; a command while not taken in |
| zsh | The same end-to-end test on macOS (zsh 5.9), and over SSH on Debian 12 |
| fish | Over SSH on Debian 12 (fish 3.6); the end-to-end test runs where fish is installed, which is not this Mac |
| Real SSH | In a Debian 12 container running `sshd`: accounts whose login shells are bash, zsh and fish, `sudo -s`, and `ssh -J` through a jump host. Each was taken into, used (a command, a directory change, an exit status), and left |
| A live model | Ollama (`deepseek-v4.1-flash:cloud`), taken into a shell inside the pane's own: asked to change a port in a file there, it read the file, changed the one line with `sed`, showed the result, and said it was not on the user's own machine |
| On a real server | Tried by the owner over SSH: it worked. Which shell and system the server has was not recorded |
| Not tried | A slow link; a remote macOS or BSD; a container (`docker exec`, `kubectl exec`); `mosh`; a remote bash with its own DEBUG trap or prompt framework (starship, oh-my-zsh); clicking "take the agent in" at a password or host-key prompt, which would type the first line as the answer |

### Slice 2: sessions that live on the remote — done

- [x] A remote tab, opened from Astrum over the user's own `ssh`: `≡ menu`
      → New tab on another machine…, `Ctrl+B R`, or `/ssh host`. Whatever
      is typed for the machine is given to `ssh` as it is (a name, options,
      a jump host), so keys, config and jump hosts apply unchanged. `ssh`
      gets a terminal of its own for its questions, which the pane shows
      and answers; what passes between the two Astrums goes through pipes
      beside it
- [x] Astrum's own program on the remote (`astrum __hold`, reached through
      `astrum __link`) keeps the shells, so they and what runs in them
      outlast a dropped connection. This machine draws them as ordinary
      tabs and panes: splits open on the same machine over the same
      connection, and the tab and the status bar say which machine it is
- [x] Reconnecting: by itself after 1, 3, 8 and 20 seconds, then every
      minute, or at once on Enter or a click. The pane continues from the
      byte it had reached while the last 4 MB the shell wrote covers the
      gap; past that, or for a session that was restarted and has nothing,
      the program there sends the screen as it now stands, with its
      history, colors, a full-screen program's screen and the shell's
      beneath it. A connection that stalls without closing is given up
      after 35 seconds of silence, and never holds the shell up
- [x] Saved with the session: after a crash or a restart of this machine
      the tab finds its shells again. `astrum kill` ends them; the tab
      still comes back, in a new shell where the old one was
- [x] If the program is not on the remote, Astrum asks first, then copies
      the release build for that machine over the SSH connection that is
      already open into `~/.local/bin`, its SHA-256 checksum verified on
      this machine before a byte is sent. It is run once there before it
      replaces anything. One file in the home folder: no root, no package
      manager, and the remote needs no internet access. `download_base`
      names a mirror or a folder in place of GitHub
- [x] The agent stays here, with the model and the keys. Its commands are
      typed into the shell there; its file tools, the diff before a
      change, a command run beside a busy shell and the project's
      `AGENTS.md` are carried out there by that program. It is told which
      machine it is on, and told plainly when the connection is down
- [x] The rule for two versions: each side says the number of the link
      language it speaks (now 1), which changes only when one side could
      not make sense of the other; messages may gain fields without it
      changing. The same number works together whatever the versions. An
      older one there is replaced only on a yes, a newer one never, and
      each number has a socket of its own so that two can run side by side
- [x] Kept running through a logout on Linux: where lingering is on (the
      switch the login service of wave 3 relies on), the program is
      started under the user's own systemd; otherwise as a program of its
      own session, which outlasts a logout unless the system is set to
      end a user's programs then, and Astrum says so when it is
- [x] Where it cannot be used is said in the README: a machine that does
      not let you run or keep a program of your own, a system there is no
      build for, and an account whose `ssh` forces a command. The first
      slice covers those
- [x] Builds of this branch call themselves 0.5.0-dev, so that they are
      never taken for a release there is a build to download of

What slice 2 verified, and what it did not:

| Area | Status |
| --- | --- |
| The link | Unit tests: frames there and back; a message or a frame of a kind this version does not know is passed over, not an error; a frame cut short; the one line run on the remote has nothing in it a login shell other than `sh` reads differently; what a login prints before it is passed over, and nothing but the exact word is taken for it |
| The screen as it stands | Unit tests that draw on one emulator, take its screen, draw that on another and compare every cell: colors and styles, history, lines that ran on (and rejoin when the pane is made wider), wide characters, a colored background to the end of a line, a full-screen program with the shell's screen beneath it, what the keys and the mouse were asked to send, and forty long random runs. The first random run found two faults, both fixed |
| Copying | Unit tests of the checksum list and of a build that does not match it. End to end: asked first and nothing there until the answer; copied, identical, nothing left beside it, one connection for all of it; a no; a wrong checksum (nothing sent); an older Astrum replaced on a yes; an older and a newer link number |
| In a session | End-to-end tests with a stand-in for `ssh` that runs the far side on the same machine in folders of its own: opened with the mouse and with the keyboard; a password asked for and answered in the pane, wrong and then right; a machine that cannot be reached, and Try again; a connection killed and found again with the shell's state and what it wrote meanwhile; a connection that stalls while 2.7 MB is written; more written than is kept; a pager found as it was by a restarted session; a split; closing; a killed session; a stopped one |
| The agent | End to end with the stand-in model: reads that machine's file and not this one's of the same name; runs a command there after the usual question; a change shown as a diff and made there; search; the project's rules from there beside the user's own; told when the connection is down, and works again when it is back |
| Both systems | The whole suite (136 unit, 106 end-to-end, 5 install) on macOS (Apple silicon) and on Debian 12 (arm64, in a container). Type-checks with Rust 1.88 |
| Real SSH, Linux to Linux | A Debian 12 container running systemd and `sshd` (OpenSSH 9.2): a machine without Astrum asked about, copied to, and the copy's checksum the same on both sides; the `sshd` serving the connection killed while a job ran, reconnected by itself to the same shell with the job's output; this side's server killed outright and the restored tab finding the same shell; `astrum kill` ending the shells and the program there gone twenty seconds later; accounts whose login shells are zsh and fish (the one line runs under both); a password asked in the pane, wrong and then right; `-J` through a jump host, and a split over that one connection |
| Logging out, on systemd | The same container with `KillUserProcesses=yes`: without lingering the warning is shown, the program is in the login's own scope and ends with it; with `loginctl enable-linger` it is started as a unit of the user's own systemd (`user@.service/app.slice/astrum-hold-…`), is still there after the logout, and the shell with it |
| Real SSH, Mac to Linux | From this Mac to that container: the Linux ARM64 build picked from what the machine said of itself, copied and identical; a tab used; the connection cut and found again with what a job wrote meanwhile; a split; stopped. With a key made for the test and `-F /dev/null`, so nothing of the owner's `ssh` setup was used |
| A live model | Ollama (`deepseek-v4.1-flash:cloud`) in that tab from the Mac: asked to change a port in a file there, it read the file with `read_file`, changed it with `edit_file`, showed it with `cat` in the shell there, followed the `AGENTS.md` of the project on that machine, and said it was working on the Linux machine and not the user's own |
| Not tried | A real network between the two machines (a slow or lossy link, a laptop going to sleep, a change of network): only killed and stalled connections. The static musl builds a release carries (the far side ran a build made in the container; CI builds and tests the static ones). Downloading from a release on GitHub, which cannot exist before 0.5.0 is released: the folder route was used throughout. A Mac as the far machine over real `ssh`. A second factor or a new host's fingerprint typed in the pane (a password was). An account with `ForceCommand`. Two sessions of this machine on one far machine at once. Many shells, or megabytes a second, over one connection for hours |

### Slice 3: nothing tested twice — done

Asked for by the owner during the 0.4.0 release: code that has passed its
tests is not tested again while it is the same code. That release tested
one state of the code four times: the push to the branch, the pull request
of the same commit, the merge into `main`, and the release build, on three
machines each time.

What counts as the same code is its files: every one as it stands, the
tests and the workflows among them, whatever commit, branch or tag they
are reached by. The merge of 0.4.0 and the last commit of its branch are
two commits and the same files. The roadmap and the changelog are left out,
since nothing built or tested reads them: a note that a slice is done is
not a change to the code.

- [x] A name for a state of the code, made from its files
      (`.github/scripts/code-key.sh`). A run of CI that passes leaves a
      note under it (an artifact, `tests-passed-<key>`, kept thirty days).
      The first job of every run asks whether there is one, and the others
      run only if not
- [x] One CI run for a commit, not two: a pull request from a branch whose
      pushes are tested, and which merged is exactly that branch, is left
      to the run the push started. One whose base has moved is different
      code and is tested as the merge it is; so is one from a fork
- [x] No second run on `main` for a merge that brings in exactly what was
      tested: that code has its note already
- [x] None at all for a push that changes only the roadmap or the changelog
- [x] The release build does not run the suite again for a tag on code
      that has passed: it builds, checks the binaries and drafts. A tag on
      code with no note is still tested first. A rehearsal on a branch
      never is, since the push that started it has a run of CI of its own
- [x] The same by hand: `scripts/test.sh` runs the suite unless this exact
      code, uncommitted changes included, has passed here or in CI
- [x] Whenever it cannot tell (GitHub does not answer, or answers
      something that is not the number of a run) the code is tested, and
      the run says what GitHub said. Only a note left by this repository's
      own runs counts: a fork's workflow could leave one of any name.
      Asking by hand tests everything regardless: Actions → CI → Run
      workflow, or running a finished run again

What slice 3 verified, and what it did not:

| Area | Status |
| --- | --- |
| The decision | Eleven tests (`tests/ci.rs`) that run the scripts for real in repositories made for them, with stand-ins for `gh`, `cargo` and `rustc`: code that has not passed and code that has; a merge and a tag as the same code; a note in the roadmap or the changelog as no change, and the README, a file that only looks like the roadmap, and a file removed as changes; a pull request that is its branch, one whose base moved, one from a fork, one from a branch whose pushes are not tested; no answer and a wrong answer from GitHub; a run asked for by hand |
| By hand | In those tests: once per state; an edit, a new file, a commit of what was tested, an ignored file, another compiler; a failing run not remembered; part of the suite never taken for the whole; the index left alone. And on this repository: the suite ran once (257 tests), and the second time was not run |
| The scripts | `shellcheck` (0.9, in a container) finds nothing in any of them; the name comes out the same under `dash` with GNU `grep` on Debian as under `sh` with BSD `grep` on macOS; the deciding one runs with no `gh` at all, and says to test |
| On GitHub, the first push | CI ran in full on code it had not seen: the first job said so, all seven jobs after it passed, and the last left the note, which the list of artifacts then showed with thirty days to run. The release rehearsal the same push started built and checked all three binaries with its test step skipped |
| On GitHub, a second run of the same code | A release rehearsal started by hand a few minutes later asked the same question and was answered "Not run again: this code passed in" the CI run above. (It was cancelled there: what it would have built had just been built.) |
| On GitHub, the same run made again | Tested everything again: it did not find the note it had itself left a minute earlier, though that note was in the list. So running a finished run again is a way of asking for a full run, and the workflow and the README now say so |
| On GitHub, the second push | The commit that left the roadmap and the changelog out of the name: CI ran in full once more (the name was new), every job passed, and the note was left under the name this Mac works out for the same commit, so the runner and a machine here agree on it. The rehearsal again built all three binaries without the suite |
| On GitHub, a push of the roadmap alone | The commit recording the row above: its run took seven seconds. The first job said "Not run again: this code passed in" the run before, every other job was skipped, and the run as a whole reads as passed. No release rehearsal was started. (A skipped job with a matrix is listed under its unfilled name, `Tests (${{ matrix.runner }})`: how GitHub shows it, and harmless) |
| Not verified | The three cases that only a release goes through: a pull request left to its push, the merge into `main` not tested again, and a tag built without the suite. Their decisions are tested in `tests/ci.rs`, and the lookup they rest on was seen to work on GitHub, for a push among others, but none of the three has happened yet. A run whose jobs were skipped reads as passed; that a pull request's page does too, where it is merged "when green", has not been seen |

### Slice 4: release prep, then the PR

- [x] An independent review of everything the wave changed, by two
      reviewers. Nineteen findings (one made by both). Fifteen fixed, with
      a test for the most serious; four recorded as limitations:
      - a yes given after the connection had gone, while the question was
        still showing, ran the agent's command on this machine in place
        of the one the tab is on, or typed it into nothing; a file tool
        approved the same way would have read or written this machine's
        files. Where the work is done is now asked again at the moment it
        is done, and a yes to doing it there is not a yes to doing it here
      - a change to a file approved after the connection had gone left
        the agent's turn waiting for ever for an answer nobody would send
      - "take the agent in", clicked on an `ssh` that was then refused,
        had this machine's own shell read what was typed for the far one,
        answer it, and have its own marks replaced: the agent then
        believed it was on another machine until that shell ended
      - two machines sharing a home folder were one owner to a far
        machine, and each ended the other's shells there
      - the screen of a full-screen program started from the middle of a
        line (an editor opened by `git commit`) was drawn from that column
      - two keepers started in the same moment took each other's socket
        away, leaving shells that could not be reached; one keeper at a
        time is now settled by a lock
      - the keeper never noticed a connection that had stopped without
        closing, so nothing answered what programs asked of the terminal;
        one that falls behind is now answered there too
      - a shell asked for and started, whose answer was lost with the
        connection, was asked for again, refused, and ended
      - a screen larger than a frame could never be sent (it now goes in
        pieces); a copy cut short by the connection was run before its
        length was looked at; Enter, pressed a moment after a password,
        counted as yes to copying Astrum over; keys typed while a copy
        was under way piled up in `ssh`'s terminal; the keeper stopped
        listening for good after one refused connection; the buttons of a
        waiting pane did nothing while the keyboard was in the agent's
        panel; a turn begun while the machine was down looked for the
        project's rules on this one
      - CI: a pull request from a branch named like `wave-5/fix` was left
        to a push run that such a branch never gets
      - recorded, not fixed: what names the code in CI does not include
        the compiler, so a note left under one Rust stands for up to
        thirty days under the next (by hand it is included); the
        checksum of a copied build shows it arrived whole, not who made
        it, as for the install script; some of a terminal's state is not
        carried when a screen is sent as it stands (now listed in
        `screen.rs`); after `astrum kill`, shells on a far machine are
        given under half a second to be told
- [x] Upgrading tried both ways with the released 0.4.0: a 0.5.0 `astrum`
      with a session 0.4.0 started and the reverse (attach, type, new
      tab, list, reload, stop). The line run on a far machine reports the
      released 0.4.0 there as one from before tabs on other machines
- [x] The static Linux build as the far side, which slice 2 had not
      tried: CI's own build of this branch, copied from this Mac over real
      `ssh` to Alpine 3.24 (musl, busybox, `ash`) and to Debian 12. On
      each: identical checksum, a tab used, the connection cut and found
      again with what a job wrote meanwhile, shells ended by `astrum kill`
- [x] A test for the agent's command run beside a busy shell on the other
      machine, which nothing covered
- [x] The release workflow's own checks run by hand against this version:
      the tag, `Cargo.toml` and the changelog agree, and the notes it
      would cut from the changelog are the 0.5.0 section
- [x] Version 0.5.0; changelog with upgrade notes and known limitations;
      README pass; release date set. No packages added since 0.4.0
- [x] Pull request from `wave-5` into `main`. Its own run was left to the
      run the push had started, every job skipped, and the pull request
      still read as green and mergeable (15 checks passing, 6 skipped).
      Every check passed on the commit that was merged
- [x] After the merge: the run on `main` was not tested again (ten
      seconds: "this code passed in" the branch's run), and the merged
      code is the branch's, file for file. `v0.5.0` tagged on `main`: the
      release build built and checked all three binaries with its test
      step skipped, and drafted the release with the changelog's 0.5.0
      section as its notes. Published. One state of the code was tested
      once, where 0.4.0's was tested four times
- [x] Astrum copied to a far machine from the release on GitHub: from an
      Apple silicon Mac over real `ssh` to Debian 12 (ARM64, in a
      container), with nothing set to say where the build is. It was
      downloaded, its checksum matched the release's `SHA256SUMS`, it was
      kept for next time, and the tab was used, cut off and found again
- [ ] Still untried: a real network between two machines (a slow or
      lossy link, a laptop going to sleep); a Mac as the far machine over
      real `ssh`; a second factor or a new host's fingerprint typed in the
      pane; an account with `ForceCommand`

### After those

Taken up in wave 6: the MCP client, skills, and agents across tabs.

## Wave 6 → v0.6.0: the agent's reach, and a website — in progress

Decided at the start of the wave: what wave 5 left for later (an MCP
client, skills, agents across tabs), and a website with the full user
documentation, made and published as part of each release's prep. Agent
tasks that run by themselves were looked at and put in the parking lot at
the end of this file.

### Slice 1: MCP servers — done

- [x] MCP servers named in the config file, `[mcp.<name>]`: a program on
      this machine (`command`, `args`), talked to over its input and
      output, or an address on the network (`url`), reached the way the
      protocol calls Streamable HTTP. A server elsewhere is reached over
      https only. A token a server needs is typed hidden with `/mcp key`
      and kept in the system's password store, as API keys are
- [x] Their tools are offered to the agent beside its own, by server
      (`files__read_text_file`), with what each server says of itself in
      the system prompt. A call to one is asked about as a command is, its
      arguments shown in full, unless that tool is in the server's `allow`
      or autopilot is on; `tools` limits which are offered at all. What a
      server says of its own tool (that it only reads) is not taken on
      trust. A call missing a required argument is not sent
- [x] What a tool returns goes through the same hiding of secrets and the
      same clipping as everything else the model is shown; the servers'
      tokens are among the secrets hidden. Pictures and files a tool
      returns are named, not passed on
- [x] `/mcp`: how each server is doing, its tools (offered or not, asked
      about or not), what a program last wrote to its error output, and
      starting one again or turning it off and on for the session. The
      settings screen has a row for the servers, where choosing one turns
      it off or on (or signs in to it), with the mouse
- [x] A hosted server that signs in with OAuth: `/mcp signin` finds where
      to sign in from what the server said when it turned Astrum away,
      registers Astrum where the authorization server lets clients do that
      (or uses the `client_id` given), opens the browser with PKCE, and
      takes the browser's return on this machine. Used over SSH, the
      address the browser ended at is given to `/mcp paste`. The sign-in
      is kept in the password store, renewed when it runs out or is
      refused, and forgotten with `/mcp signout`
- [x] Servers run on this machine, where the keys are, whatever machine a
      tab is on; the agent is told so when its shell is elsewhere
- [x] A program gets an environment of its own: what finding and running
      things needs, and what `env` gives it (`${NAME}` from Astrum's own);
      not the providers' keys. It runs in a process group of its own, which
      is ended with it
- [x] Servers start the first time the agent is asked something, not
      with the session; a turn waits up to twenty seconds for those still
      starting. One that ends after running a while is started again, up
      to three times
- [x] No new packages in the build

What slice 1 verified, and what it did not:

| Area | Status |
| --- | --- |
| The protocol | Unit tests against a stand-in server that is a shell script (`tests/mcp/stand-in.sh`): reached, its tools listed and called; a tool that fails; one that takes too long, given up on and the server told; its tools changing; a program that ends, one that prints something else and ends, one that is not there. What a server sends is answered or passed on (pings, requests Astrum does not offer, answers to nothing asked) |
| Results, names | Unit tests: text, pictures, files and links a tool returns; structured content; names made for providers' rules (at most 64 characters, letters, digits, `_`, `-`, no two alike); schemas providers refuse made acceptable |
| The network | Unit tests of the event stream taken apart and of which addresses are this machine. End to end against a stand-in server on the network with its own authorization server: turned away, signed in through a stand-in browser that follows where it is sent (discovery, registration, PKCE checked by the server, the browser's return), the session and protocol version sent back, answers as JSON and as an event stream with something else first, a sign-in run out and renewed; a server that wants a plain token, given one hidden |
| In a session | End to end with the stand-in program: asked first with the arguments shown, nothing called before the answer, declined, missing arguments refused unasked, a tool that fails, `/mcp`, off for the session and no longer offered; `allow` and `tools`; a server that will not start, with what it said; the settings row and list with the mouse; servers not started until something needs them; the program not given the providers' keys |
| A real server | The reference filesystem server (`@modelcontextprotocol/server-filesystem`, by `npx` inside a container that sees one folder), with a live model (Ollama, `deepseek-v4.1-flash:cloud`): it listed the folder and read a file unasked (both in `allow`), changed the file after the question, and said which tools it used. The container ended with the session |
| Both systems, Rust 1.88 | The whole suite (153 unit, 11 of the CI scripts, 113 end-to-end, 5 install) on macOS at the last commit. On Debian 12 (ARM64, in a container): the end-to-end tests at the last commit, the rest at the one before (the last changed only what the end-to-end tests cover). The Linux run found a wave 5 bug, fixed: what `ssh` wrote as it gave up ("Permission denied") could be dropped when its end was noticed first. Type-checks with Rust 1.88 |
| Not done | A server's resources and prompts (only tools are used); what a server on the network sends outside an answer (so a change to its tools is heard only when it is reached again); the older HTTP-with-SSE way of reaching a server; a client registered by a metadata document (the 2025-11 way) |
| Not tried | Signing in to a real hosted server; a real authorization server; many servers, or a server with hundreds of tools, at once |

### Slice 2: skills — done

- [x] A skill is a folder with a `SKILL.md` in it: front matter with a
      name and a line saying when it is of use, then the instructions, and
      any scripts or files they need. The layout other agents already use,
      so skills written for them work here unchanged: other keys of the
      front matter (`license`, `metadata`, `allowed-tools`) are passed
      over, and none of them grants anything
- [x] Found in the config directory (`skills/`), for every project; in
      folders the config file names (`skill_dirs`); and in a project's
      `.agents/skills/` or `.astrum/skills/`, from the working directory up
      to the top of the repository, as `AGENTS.md` is. The closest of a
      name wins
- [x] The agent is shown each skill's name and line, and reads the rest
      (`use_skill`) only when one fits the task, and the skill's other files
      the same way, never one outside its folder: many skills cost little
      of the context
- [x] `/name` in the panel uses one on purpose (the panel shows what was
      typed; the model is given the skill with it), and the slash menu
      offers them; `/skills` lists them and where each came from, and
      turns one off or on; so does a row of the settings screen, with the
      mouse. Turning one off is remembered
- [x] A skill's scripts are run as any command is: in the shell, where
      you watch, after the usual question
- [x] A tab on another machine: the user's own skills are offered and read
      here, and a script one brings is put there by Astrum's program
      (`~/.cache/astrum/skills/` on that machine) when the agent asks for
      it. With the agent taken into an `ssh`, it is told to write the
      script there itself

What slice 2 verified, and what it did not:

| Area | Status |
| --- | --- |
| Reading skills | Unit tests: front matter as skills write it (quoted, folded, run on, Windows line ends, a `metadata` table with a `name` in it), none at all; where they are found and which of a name wins; a folder that is not a usable name; a skill's files listed, and a file outside its folder refused however it is written |
| In a session | End to end with the stand-in model: shown by name and description and not by its instructions; loaded, with its files; a file read; one outside refused; a skill that is not there; its script run after the question. `/name` with a request, the slash menu offering it, the panel showing what was typed; `/skills`; off and remembered, and no longer shown; on again from the settings with the mouse |
| Another machine | End to end in a tab on another machine: only the user's own skills offered (not the local project's), told how its scripts reach that machine, and a script put there |
| A live model | Ollama (`deepseek-v4.1-flash:cloud`) with a release-notes skill, asked for release notes in a repository: it loaded the skill unprompted, ran its script (with `bash` when the file turned out not to be executable), grouped the commits as the skill said, and ended with the line it asked for |
| Both systems, Rust 1.88 | The whole suite (157 unit, 11 of the CI scripts, 116 end-to-end, 5 install) on macOS and on Debian 12 (ARM64, in a container) at the same commit; type-checks with Rust 1.88 |
| Not done | A project's skills when the project is on another machine (only your own are offered there) |
| Not tried | A published collection of skills; many skills at once |

### Slice 3: every agent at a glance — done

Each tab has an agent of its own, and they can already work at the same
time; but only the one in the tab in front could be seen, and the others
showed the same spinner whether they were busy or waiting for an answer.

- [x] One screen with every tab's agent: what it is doing (asking, at work
      and on which command or tool, for how long, following a command it
      started, finished, stopped, stopped by you), with which model, and
      the tokens it has spent. A click goes to that tab's agent; a
      question can be answered yes from there. `Ctrl+B A`, `≡ menu`,
      `/agents`, or a click on the status bar
- [x] A tab whose agent is asking is marked `?`, one whose agent is at work
      keeps its spinner, and one whose agent finished (`✓`) or stopped
      (`✗`) while the tab was not in front is marked until it is looked at.
      The status bar says how many agents in other tabs are asking or at
      work
- [x] A terminal that does not have the tab in front is told when its
      agent asks something, finishes or stops: with a notification where
      the terminal is known to show one (OSC 9 in iTerm2, WezTerm and
      Ghostty; kitty's own; foot's), a bell elsewhere. `ASTRUM_NOTIFY`
      chooses. Nothing is said to a terminal that has the tab in front

What slice 3 verified, and what it did not:

| Area | Status |
| --- | --- |
| In a session | End to end with the stand-in model: an agent asking in a tab not in front marks the tab, is counted on the status bar, and is announced as a notification; the list, opened from the status bar with the mouse, shows the question and what would be run, and the other tab; **Yes** there answers it and the command runs; the list then says it finished, with the model; a click goes to it and the mark goes. A turn ending in a tab not in front marks it ✓ and rings where no notification was asked for; `Ctrl+B A` and Enter go to it. Nothing announced for the tab in front; `/agents` |
| Both systems, Rust 1.88 | The whole suite (157 unit, 11 of the CI scripts, 119 end-to-end, 5 install) on macOS and on Debian 12 (ARM64, in a container); type-checks with Rust 1.88 |
| Not tried | A notification as each terminal shows it (only the bytes sent were checked); several terminals on one session, each with its own tab in front, being told apart |

### Slice 4: the website — done

- [x] A site on GitHub Pages, served from a `gh-pages` branch that holds
      only the built site, with no history from `main`. It is checked out
      beside this repository as a worktree, `../astrum-site`
- [x] A generator in the repository (`site/build.mjs`), with the pages it
      is made from: each page holds only its own content, and the
      generator adds the header, the documentation menu, the links to the
      pages before and after, the list of what is on the page, and the
      footer. It needs Node and no packages; nothing it needs goes into
      Astrum's build
- [x] It builds into any folder for checking and previewing (`--serve`
      shows it as GitHub Pages would), and into the `gh-pages` worktree
      only with `--publish`, after checking that is what the folder is.
      Every local link, anchor and picture is checked, and a broken one
      fails the build
- [x] A front page that says what Astrum is in a few lines and pictures,
      with the one-line install; an install page; a page for a link that
      leads nowhere
- [x] The full user documentation, seventeen pages: getting started,
      install, tabs and panes, the mouse and the keys, sessions that come
      back, the login service, tabs on other machines, the agent,
      providers and models, rules and skills, MCP servers, several agents
      at once, settings, themes, troubleshooting, the manual page and the
      changelog. Every key, setting and theme, the manual page and the
      changelog are made from Astrum itself (`astrum keys --json`,
      `astrum themes --json`, the starter file, `astrum man`), and a test
      checks that every setting Astrum reads is in the starter file with
      a word on what it does
- [x] Screenshots made by Astrum, in a light and a dark version, the page
      showing the one that matches it: ten scripted sessions in a sandbox
      with a stand-in model and stand-in MCP servers, drawn headlessly and
      photographed by headless Chrome (`site/capture/`). The README shows
      the logo and three of them; a click on one on the site shows it full
      size
- [x] Nothing loaded from elsewhere when a page is viewed; readable on a
      phone, light and dark, with a switch between them; a search over the
      documentation; the build checks headings, picture descriptions,
      button names and the contrast of the site's colors (4.5 to 1)
- [x] A README for the site (`site/README.md`): how to change a page, add
      a screenshot, build, preview and publish
- [x] The site is built in the test suite (`tests/site.rs`) against the
      binary under test, where Node is present, so a setting without a
      description or a broken link fails CI

What slice 4 verified, and what it did not:

| Area | Status |
| --- | --- |
| The site | Built with every check passing: 20 pages, 3,600 lines of documentation. Looked at in a browser at desktop and phone widths, light and dark: no sideways scrolling, the search finds across pages, the screenshots follow the theme |
| Screenshots | All ten scenes and the picture for shared links taken from scratch in one run; the text of every scene checked for paths or names from this machine (none). Runs give the same pictures except for the timers in the list of agents ("for 7s", "4s ago"); that two runs match was not compared byte for byte |
| Not tried | The site served by GitHub Pages itself (it is published with the release); a screen reader |

### Slice 5: release prep, then the PR — done

- [x] An independent review of everything the wave changed, by two
      reviewers (the MCP client; everything else). Twenty-three findings,
      all fixed, most with a test, and two more found while fixing them:
      - the list of agents offered a yes for a command it showed only the
        start of, and for one with a warning; and a yes went to whatever
        question was waiting when it arrived, not the one drawn
      - a stored MCP sign-in or token was sent to whatever address the
        entry named, even after it changed
      - an MCP server could make Astrum read without end (an error body,
        its error output), messages to it had no time limit, and a server
        whose error output was not text was cut off from it
      - the approval of an MCP tool cut its arguments short, so what was
        agreed to could be out of sight
      - a sign-in was not renewed while in use; one the password store
        could not keep was useless; one in progress was lost if its server
        restarted; `/mcp paste` reached no sign-in at all
      - discovery while signing in followed whatever addresses the server
        gave, redirects were followed, an address like
        `http://evil.example\@localhost` counted as this machine, and any
        visit to the loopback address ended the sign-in
      - MCP tokens were not hidden from the model on the turn that started
        the servers; `/mcp log` was empty after a server failed
      - a `SKILL.md` that was a link to `/dev/zero` froze the session, and
        one to `~/.netrc` put that file before the model; a project's
        `AGENTS.md` could do the same
      - a skill or rules file was looked for in folders above the home
        folder, such as `/tmp`, that anyone can write to
      - a file a skill copies to another machine could be written outside
        the skill's folder there, or cut short and still run
      - a project's skills could crowd out the user's own, and the slash
        menu did not say where one came from
      - notifications were sent as escape sequences inside a terminal
        multiplexer, which drops them
      - the site's search could break what it marked, and its preview
        server could read the folder beside it
- [x] Found while writing the website: a provider table of your own under
      a built-in's name replaced it entirely, so a Bedrock API key in
      `AWS_BEARER_TOKEN_BEDROCK` was never read. It now keeps the built-in
      address and key variable where it sets none
- [x] Upgrading tried both ways with the released 0.5.0: each attaches to,
      lists, reloads and stops a session the other started
- [x] Version 0.6.0; changelog with the website, the changes, upgrade
      notes and known limitations; README: where a link opens, what "Yes,
      don't ask again this session" does, when the list of agents offers a
      yes
- [x] The screenshots taken again with the release build, and the site
      built for 0.6.0; it is built into the `gh-pages` worktree and pushed
      once the release is out

What slice 5 verified, and what it did not:

| Area | Status |
| --- | --- |
| Both systems | The whole suite: 176 unit, 11 of the CI scripts, 124 end-to-end (2 measurements left out, as always), 5 install and the site's build, on macOS; the same on Debian 12 (ARM64, in a container), where the site is not built (no Node there) |
| Upgrading | 0.5.0 (the released build) and 0.6.0, each with a session the other started |
| Not tried | A hosted MCP server with real OAuth; how each terminal shows a notification; the site as GitHub Pages serves it before it is published |

## Parking lot

Ideas looked at and put aside, so that they are not forgotten. Each says
what it would give, and why it waited.

- **Agent tasks that run by themselves** (looked at for wave 6). Work
  handed to the agent that it does in a shell of its own while nobody is
  watching: started and left, or on a schedule (every morning, every
  quarter of an hour during a deploy), which the session server could do
  since it keeps running with no terminal attached. What it would add to
  what Astrum has: only work done while you are away. Working in another
  tab while one agent is busy is already possible, `watch` follows a long
  command, and the overview of wave 6 shows every agent at once. What it
  would cost: an agent acting unattended needs permissions set beforehand
  and a limit on what it may spend, and is a larger risk than one you
  watch. The owner prefers to see the agent at work in the terminal.
- **Agents that share out a job** (looked at for wave 6). One agent
  splitting a task between agents in several tabs and gathering what they
  find. Much larger than the overview, and riskier; nothing asked for it
  yet.
