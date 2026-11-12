# Changelog

Notable changes to Astrum, newest first. Each release corresponds to one wave
in [ROADMAP.md](ROADMAP.md).

## 0.4.0 — 2026-11-11

### Pictures

- A picture a program prints is shown in its pane, where before it was
  dropped: `imgcat`, `kitten icat`, `chafa`, `img2sixel` and the like. It
  scrolls and is cleared with the text around it, and is there when you
  look back through the scrollback.
- All three ways of sending a picture are understood (iTerm2's, kitty's,
  sixel), and each attached terminal gets it in a way that terminal has.
  iTerm2 and WezTerm show nearly everything; Ghostty and kitty show what
  was sent in kitty's way and PNG files sent in iTerm2's; a terminal with
  no pictures shows a line saying what is there. The README has the table.
- A picture is drawn whole or not at all: partly off the edge of its pane,
  or under a menu, a faint block takes its place.
- Programs that ask are told what the terminal can do: whether kitty's
  graphics or sixel are there, and the size of a cell in pixels (a question
  Astrum did not answer before, and one `wezterm imgcat` crashed without).
- `images = false` turns pictures off; `ASTRUM_IMAGES` corrects Astrum
  about a terminal it has misjudged.
- Not done: converting between kinds of picture that would need decoding
  one (a JPEG for kitty's way, anything into sixel), and pictures that
  change many times a second.

### Speed

- A key shows sooner. At an idle prompt, every key had a frame drawn for
  it before the shell had answered, in which nothing had changed; it was
  sent anyway, and the frame with the echo then waited its turn. Nothing
  is sent now when nothing has changed.
- Typing stays quick while another pane pours out text: keys are taken
  ahead of output, and a pane's output is never far behind another's.
- A terminal that cannot keep up (a slow connection, a stalled window) is
  no longer sent frames to queue in memory behind the ones it has not
  taken. It gets one, of how things are by then, when it catches up.
- Drawing costs less: of each pane, only the rows a program has written to
  since the last frame are worked out again.

### Installing

- Astrum can be installed without building it. Each release now carries
  ready-made binaries for macOS on Apple silicon and for Linux on x86-64
  and ARM64, with their SHA-256 checksums. The Linux ones are fully static
  and run on any distribution.
- `install.sh` installs the right one with a single `curl … | sh`. It
  verifies the checksum, checks that the program runs and is the version
  expected, and only then puts it in `~/.local/bin`, without `sudo`.
- `astrum completions bash|zsh|fish` prints a completion script, and
  `astrum man` the manual page.
- Building from source needs Rust 1.88 or newer. The README used to say
  1.85, which does not build it.

### Settings and customization

- A settings screen: `≡ menu` → Settings, `Ctrl+B .` or `/settings`. The
  common settings are rows to click (theme, colors for programs, where the
  agent panel sits, model, how much the agent may do, what a stopped
  session brings back, and more). A change applies at once and is
  remembered; one row hands everything back to the config file.
- The first time Astrum is started it shows a four-card walkthrough of what
  is where. It can be skipped, and shown again from the settings.
- Every key Astrum uses can be changed: a `[keys]` table in the config file
  gives any action other keys, after the prefix or (with `ctrl` or `alt`)
  without it. `astrum keys` lists the actions and their keys. The hint
  line, the menu and the help screen show the keys as they are set.
- The config file is read again whenever it is saved, and the running
  session takes the change: theme, keys, prefix, panel placement, agent
  settings, providers. A file with a mistake in it changes nothing and
  Astrum says what the mistake is.
- Themes of your own: a `[themes.NAME]` table in the config file, with any
  of a theme's colors, built on a theme you name. It shows up in `/theme`
  and `astrum themes` with the built-in ones.
- `theme_light` names a theme to use when the terminal's own background is
  light, for a terminal that changes with the time of day.
- Only what you choose inside Astrum is remembered in `state.toml`. Choosing
  a model used to save the current theme along with it, which then
  overrode the config file from that day on.

### Themes

- Out of the box Astrum now uses your terminal's own colors (the
  `terminal` theme), so starting it changes nothing about how your terminal
  looks. The Astrum Night theme used to be the default; it is still there
  as `astrum`.
- A chosen theme now also colors what programs draw (`theme_terminal` is on
  unless you turn it off), so a theme is one look, not Astrum's bars in one
  set of colors around a shell in another.
- One more theme, twenty-two in all: McDermott (`mcdermott`), the Matrix
  theme's one-color look built around `#ff0068`, on near-black. It is made
  for long hours in a dark room: the text is that color itself, and nothing
  on screen is brighter than it has to be.

### Fixed

- With two terminals on one tab, the terminal that took the tab over could
  have its first command run at the other's size.
- A question from the agent that appeared while you were typing ignored
  your keys for 1.2 seconds only: typing that ran on past that could still
  answer it, and a `y` among the letters approved the command. Keys are now
  ignored for as long as the typing continues, and an answer is taken only
  after a pause of a second. Ctrl+C, which had been held back with the
  rest, now stops the agent at once.
- Double-width characters (CJK, emoji) could leave marks behind or lose
  their neighbors: the second half of one stayed on screen as a blank in
  its colors when another was drawn just to its left; one pushed to the
  last column of a pane spilled onto the divider or the next row; and one
  split by an insert or a delete could make the character after it vanish.
- Found by a review before this release, in what the release itself
  added: a theme of your own edited in the config file now takes effect
  whichever color changed; autopilot set by the config file can be taken
  away by it; sessions no longer overwrite each other's choices in
  `state.toml`; the prefix followed by a key with Ctrl still held works as
  it did; `shift` in a key is refused where a terminal would not report it,
  instead of quietly binding the key without it; and a picture can be
  drawn only inside the pane that printed it.
- A change of the terminal's size that arrived in the same instant as
  input could be missed: splitting the terminal's window, which shrinks it
  and takes the focus away at once, left the session drawn for the old
  size until the window was resized again.

### Upgrading from 0.3.0

- A session that is already running keeps running the 0.3.0 server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. Its tabs come back. (The two versions understand each other in
  the meantime: a 0.4.0 `astrum` can attach to, list and stop a session a
  0.3.0 one started, and the other way round.)
- From this release on there is nothing to build: the one-line install in
  the README puts `astrum` in `~/.local/bin`. If you installed 0.3.0 with
  `cargo install`, that copy is in `~/.cargo/bin`; whichever of the two
  comes first on your `PATH` is the one that runs, so remove the other
  (`cargo uninstall astrum`) or go on updating the way you did before.
- If you had not chosen a theme, Astrum's bars change from Astrum Night to
  your terminal's own colors. `/theme` and Astrum Night bring the old look
  back. (A theme counts as chosen once anything set inside Astrum, such as
  the model, has been saved: it is then written to `state.toml`.)
- If you had chosen a theme, programs are now drawn in its colors too. Set
  `theme_terminal = false` in the config file to keep them in your
  terminal's own.

### Known limitations

- Pictures are passed on, never decoded. So a JPEG cannot be shown on a
  terminal that only has kitty's way of taking pictures (Ghostty, kitty),
  nothing can be shown on one that only draws sixel unless it was sent as
  sixel, and a picture partly off its pane or under a menu is not drawn in
  part. Pictures that change many times a second, and kitty's animations
  and placements inside text, are not shown.
- A program that decides how to send a picture from the name of the
  terminal sees `TERM_PROGRAM=astrum` and may need telling which way to
  use.
- Sixel is drawn pixel for pixel, so it is shown only on a terminal that
  says how large its cells are, and only while those are the size they
  were when the picture was printed.
- Astrum learns how large your terminal's cells are when it attaches. After
  changing the font size, detach and attach again before printing
  pictures, or they are sized for the old one.
- There are release binaries for macOS on Apple silicon and Linux on
  x86-64 and ARM64. An Intel Mac builds from source.
- The limitations listed under 0.3.0 still hold, except the first.

## 0.3.0 — 2026-11-11

Multiplexer depth: split panes, sessions that come back after a restart, a
search through the scrollback, links, and a view of its own for each
terminal window. Everything new has a way in for the mouse.

### Panes and tabs

- Split panes: a tab can be divided into several shells, side by side or
  one above the other, as often as there is room. Move between them with
  `Ctrl+B` and the arrow keys or a click, drag a divider to resize, zoom one
  to fill the tab with `Ctrl+B z`, close one with `Ctrl+B x`.
- Tabs can be reordered: drag one along the tab bar, or `Ctrl+B <` and
  `Ctrl+B >`.
- Right-click a tab or a pane for a menu: rename, move, close, split, zoom.
- `≡ menu` on the status bar opens a menu of everything `Ctrl+B` offers,
  each entry with its key, so none of it needs the keyboard.
- The hints shown after `Ctrl+B` can be clicked as well as typed, and stay
  for ten seconds instead of three (longer while the pointer is moving).
  On a narrow terminal the line shows what fits and `≡ more` opens the rest.
- On a terminal at least 140 columns wide the agent panel opens beside the
  shell instead of below it, with a draggable edge. `agent_panel` in the
  config file fixes it to `bottom` or `side`.
- The agent works in the pane it was asked from and stays there until it
  has finished, while you carry on in another.

### Sessions that come back

- A session that is stopped, or goes down with the machine, comes back the
  next time it is started: its tabs and their names, the panes of each and
  how they are split, each shell's directory, what each pane showed and
  remembered, and each tab's conversation with the agent. Each pane gets a
  fresh shell; a line marks where the restored part ends.
- The session is written out as it goes (about fifteen seconds after
  anything changes) and once more when it stops, so a crash loses little.
- Closing a session's last shell ends it for good and nothing is kept.
  `astrum kill --forget` stops a session and discards its tabs; `astrum ls`
  lists stopped sessions that would come back.
- `restore` in the config file chooses how much is kept: `all` (the
  default), `layout` (nothing of what was on screen is written to disk) or
  `off`.

### Login service

- `astrum service install` runs a session under the system's service
  manager: started at login and restarted if it stops. On Linux (a systemd
  user service) the session also keeps running through a logout once
  lingering is on; on macOS (a launchd agent) a logout still ends it and it
  comes back restored. `astrum service status`, `uninstall`, and `install
  --print` to see what would be written.

### Scrollback

- Search: `Ctrl+B /`, `≡ menu` → Find, or a right-click on the pane opens a
  search bar on the pane. It finds as you type through everything the pane
  remembers, lights up the matches, and says which one you are on; step
  through them with `Enter`, the arrow keys or the bar's `↑` `↓` buttons.
- Copy mode: `Ctrl+B [` gives the keyboard a cursor of its own to move
  through the screen and the scrollback, select with `v`, and copy with `y`.
- `ASTRUM_CLIPBOARD_TOOL` names the program copied text is handed to, or
  `none` to leave copying to the terminal.

### Several terminals, links, and what programs are told

- Each terminal attached to a session has its own view: its own tab in
  front, its own menus and prompts, drawn to its own size. Two terminals on
  the same tab share it, sized for the one that used it last. All attached
  terminals used to mirror one view.
- Links can be followed. Point at a web address in a pane, or at a link a
  program marked as one, and the status bar shows where it goes; `Ctrl+click`
  opens it, and a right-click offers Open link and Copy link. Only web
  and mail links are opened; any link can be copied. `ASTRUM_OPEN_TOOL` names the program a
  link is handed to, or `none`.
- A shell without the integration (a plain `sh`, or a shell started inside
  another, such as `sudo -s`) is asked where it is through its process, so
  the status bar, the tab's name and new tabs follow its directory too.
- Programs that ask the terminal for its colors (to tell a dark screen
  from a light one) are told the real colors of the terminal Astrum is
  running in, or the theme's when `theme_terminal` is on. They used to be
  told a fixed dark pair.
- Enhanced key reports are passed through: a program that asks for them
  (recent Neovim, Helix, fish and others do) is sent keys unambiguously,
  so it can tell `Ctrl+I` from `Tab` and `Shift+Enter` from `Enter`, and
  hear of keys being held and let go. Programs that do not ask get exactly
  what they got before.

### Agent

- Compacting by itself now takes one line in the panel instead of printing
  the whole summary in the middle of the work. `/summary` shows the summary
  a compacted conversation continues from; `/compact` still shows it at once.

### Themes

- One more theme, twenty-one in all: Synthwave.

### Upgrading from 0.2.0

- A session that is already running keeps running the old server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. The first restart starts afresh, because a 0.2.0 server saved
  nothing to come back to.
- From then on `astrum kill` is no longer a clean slate: the session's tabs
  come back the next time it is started. `astrum kill --forget` stops it
  and discards them, and closing the last shell ends a session for good.
- What each pane shows and remembers is now written to
  `~/.local/share/astrum/sessions/`, readable only by you. Set `restore =
  "layout"` to keep tabs and directories but nothing that was on screen, or
  `restore = "off"` to keep nothing.
- `Ctrl+B [` now starts copy mode; it used to scroll back one page, which
  `Shift+PageUp` and the mouse wheel still do.
- The hints after `Ctrl+B` wait ten seconds for the key that follows,
  where they waited three. `Esc` lets go at once.
- With several terminal windows on one session, each now has its own tab in
  front. They used to move together.
- When it starts, `astrum` asks your terminal for its colors. A terminal
  that answers no questions at all would hold the start up for two seconds;
  no terminal in common use is known to.

### Known limitations

- Inline images are not shown: image data a program sends is dropped, as
  before.
- A restored pane gets a new shell in its old directory with its old
  output above it. The programs that were running are not started again,
  and a pane that was inside a full-screen program when the session was
  saved comes back with the scrollback from before that program started.
- On macOS the login service does not carry a session through a logout,
  because macOS ends every process of the user's then; the session is
  restored at the next login. `astrum service install` has been run for
  real on Linux only.
- `Ctrl+click` on a link reaches Astrum only in terminals that pass it on.
  macOS Terminal and some others keep it for their own menu; the right-click
  menu's Open link works everywhere.
- Two terminals looking at the same tab share one size for its shells.
- A link is opened on the machine the terminal runs on with the system's
  own `open` or `xdg-open`.
- Enhanced key reports cover what most programs ask for (telling keys
  apart, and repeats and releases of keys that are sent as codes). A
  program that asks for every key as a code is not told when plain letter
  keys are let go, nor about the modifier keys by themselves, and alternate
  key values are not sent.
- Reading the terminal's colors was tested against a stand-in terminal, not
  yet by hand with a real one. Enhanced keys were tried with fish 4, and
  otherwise with scripted programs.

## 0.2.0 — 2026-11-11

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
- In bash, pressing Enter on an empty prompt was counted as running the
  previous command again. This bug is in 0.1.0.

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
  typed before the prompt has appeared, or one recalled from history at a
  prompt that redraws itself, goes to the shell as before.
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
