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
| 3 | v0.3.0 | Multiplexer depth | In progress |
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

## Wave 3 → v0.3.0: multiplexer depth — in progress

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
      It needs a slice to itself
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
