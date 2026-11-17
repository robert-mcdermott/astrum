# Changelog

Notable changes to Astrum, newest first. Each release corresponds to one wave
in [ROADMAP.md](ROADMAP.md).

## 0.11.1 — 2026-11-16

### Setting up a provider, explained

Nothing in Astrum itself changes; the starter config file and the
documentation do.

- The five built-in providers need nothing in the config file, only a key,
  given with `/key` or in the provider's environment variable. The file
  `astrum config --init` writes now says so, and shows all five written
  out, OpenRouter among them, for anyone who wants to change one.
- [Providers and models](https://robert-mcdermott.github.io/astrum/docs/providers.html)
  has the same, and the steps for OpenRouter from making a key to choosing
  a model.
- The settings page said a table named after a built-in provider replaces
  it whole. It keeps the address and the key variable it leaves out, and
  the page now says that.

## 0.11.0 — 2026-11-16

### How hard the model thinks

- A model can be told how much effort to put into its replies: `low`,
  `medium`, `high`, and for the Claude models that take them `xhigh` and
  `max`. Click `◔` beside the model on the status bar and pick a level, or
  type `/effort` (`/effort high` picks one directly). `default`, which is
  where every model starts, says nothing and leaves it to the provider, as
  before.
- The level is kept for each model, and comes back when the model is
  chosen again. `effort = "high"` under `[models."name"]` in the config
  file sets one to start from.
- It works with Anthropic, with Claude models on Amazon Bedrock, and with
  OpenAI, OpenRouter and OpenAI-compatible providers such as Ollama. A
  model that refuses a level is asked again without it, goes back to its
  own level, and the panel says so.

### Tables in replies

- A table in the agent's reply is drawn as a table, with ruled columns,
  where before it was shown as rows of `|` that wrapped mid-cell. A table
  wider than the panel wraps its longest cells, and in a very narrow
  panel each row becomes a short list.

### Calls that are tried again

- A request the provider turns away because it is busy or rate limited,
  one that meets an error on the provider's side, and one whose connection
  drops are made again: up to four times, waiting 2, 4, 8 and 16 seconds
  or as long as the provider asks. The panel counts down what it is
  waiting for, and Esc stops the wait. What waiting cannot mend is not
  waited for: a wrong key, an account out of credit, a server on this
  machine that is not running.
- A reply cut off part of the way through is asked for again once, and
  what had been shown of it is taken back. Before, a dropped connection
  could leave half a reply standing as if it were the whole.

### Amazon Bedrock

- Claude models on Bedrock now keep the prompt between requests (prompt
  caching), so the steps of a turn cost a fraction of what they did.
- Claude models that think for themselves show a summary of their
  thinking, as they do with Anthropic.

### OpenAI's newer models

- OpenAI's own models are now asked through its Responses API, which is
  the only way its newer ones use tools while thinking. Before, they
  refused every request the agent made ("Function tools with
  reasoning_effort are not supported"). Other OpenAI-compatible providers
  are asked as before; `api = "responses"` or `api = "chat"` in a
  provider's table chooses where the address does not say.

### OpenRouter, ready to use

- `openrouter` is a built-in provider. Give it a key with
  `/key openrouter` (or in `OPENROUTER_API_KEY`) and `/model` lists its
  models. Each model's context length is read from OpenRouter's own list,
  and an effort level is sent in the form OpenRouter takes.

## 0.10.0 — 2026-11-15

### Upgrading in one command

- `astrum upgrade` looks for a newer release before it moves anything. At
  a terminal it says what is out and asks whether to install it; say yes
  and the release is downloaded, checked and installed over the program
  you ran, and that program then moves the sessions onto itself.
  `--fetch` installs without asking, and `--no-fetch` does not look.
- The download is made by the same script as a first install, now built
  into the program: over https, against the release's checksums, and
  tried before it replaces the copy that works. A download that fails any
  of that replaces nothing, and no session is moved.
- Run from a script, where nothing can be asked, a newer release is only
  mentioned. Where the program is kept in a folder you cannot write, that
  is said instead of asking. A program reached through a link is replaced
  where it really is, and the link left alone.
- `ASTRUM_INSTALL_DIR` and `ASTRUM_DOWNLOAD_BASE` do for this what they do
  for the install script.

### Panes that move

- Right-click a pane → Move this pane… (or `Ctrl+B M`) and the pane is
  held. Click the middle of another pane and the two change places; click
  near one of its edges and the held pane is given that half of its
  space: left, right, above or below. A frame shows where it would go
  before you click. Esc, any other key, or the right button lets go.
- The keyboard goes with the pane, and whatever runs in the panes carries
  on. Where there is not room for two panes, that is said and nothing
  moves.

### Fixed

- A pane that was floated out of the splits and put back went to the
  right of its neighbour, whatever it had been. It goes back as it was
  split: above, below, left or right, as large, and beside the same panes.

## 0.9.0 — 2026-11-15

### Scripts and the command line

- A running session can be reached from a script, or from another
  terminal. `astrum run -- CMD` opens a pane running a command and prints
  the pane's id: in a tab of its own, or `--split right|down`, or
  `--float`; `--wait` waits for it and exits with its exit code, `--keep`
  leaves the pane up when it ends, `--focus` moves the keyboard to it.
- `astrum send` types text into a pane (`--enter` presses Enter),
  `astrum capture` prints what a pane shows (`--history` for all it has
  kept), and `astrum list` lists the tabs and panes with what each is
  doing (`--json` for a script).
- `astrum status working|done|blocked|error "message"` says how a script is
  doing, for the pane it runs in: the tab is marked and your terminal told,
  as for anything else that needs you.
- Every pane has an id, in `ASTRUM_PANE` in its shell. Run inside a
  session, these commands act on that session and that pane.
- What is sent goes to a pane's program and never to the agent: its
  questions, and the commands it wants approved, stay with a person at a
  terminal.

### Layouts

- A layout opens a project the same way every time: a file of your own,
  `~/.config/astrum/layouts/NAME.toml`, of tabs, each with a name and a
  folder, split into panes, each with a folder and a command. `≡ menu` →
  Open a layout…, `Ctrl+B l`, or `astrum -s shop --layout shop`;
  `astrum layouts` lists them.
- Every pane is an ordinary shell, and its command is typed at the shell's
  first prompt, so your aliases and `PATH` apply.
- Right-click a tab → Save this tab as a layout… writes one from what is
  open: its splits, its folders, and what is running.

### A floating pane

- A tab can have one pane drawn over the others, for a quick command
  without disturbing the splits: right-click a pane → Open a floating
  pane, or `Ctrl+B F`. Drag its title to move it and its `◢` corner to
  size it. Its `✕` puts it away, still running; `Ctrl+B F` brings it back.
  A click outside it goes to the pane clicked.
- A pane of the splits can be made to float (Float this pane), and the
  floating one put back into them. Its place is kept with the session,
  through a restart and through `astrum upgrade`.

### Changes

- `≡ menu` leaves out Next tab and Previous tab while there is one tab,
  and has Open a layout… once there is a layout.

### Upgrading from 0.8.0

- `astrum upgrade` moves a running 0.8.0 session onto 0.9.0 without
  stopping anything in it. Until a session is moved, the new commands say
  that it runs an older Astrum.
- Shells started before the move have no `ASTRUM_PANE`; `ASTRUM_TAB`, which
  they have, holds the same number, and the commands use it.
- `Ctrl+B l` and `Ctrl+B F` are new. If you had given one of those keys to
  something else in `[keys]`, it stays yours, and the menus still reach the
  new things.
- `astrum list`, `send`, `capture`, `run`, `status` and `layouts` are new
  commands; `astrum ls` still lists sessions.
- Nothing new is needed to build from source: the same Rust (1.88), and no
  packages added.

### Known limitations

- A layout that comes with a project, in the project's own files, is not
  read: only the layouts in your own folder are.
- `astrum run` runs its command as given, not through your shell, with the
  session's environment and not that of the terminal it is run from.
- A tab has one floating pane. A terminal looking at a tab that is laid
  out for another terminal of a different size does not draw it, and a
  picture it partly covers is not drawn.
- A `--wait` in progress when its session is moved onto a new Astrum loses
  its connection, and says so.
- The limitations listed under 0.8.0 still hold.

## 0.8.0 — 2026-11-15

### Upgrading without stopping anything

- `astrum upgrade` moves a running session onto the Astrum you have just
  installed, with everything in it still running: every shell and the
  programs in them, what each pane shows and its scrollback, your tabs and
  splits, and each tab's conversation with the agent. The session's server
  becomes the new program in place, so nothing is restarted. Terminals
  attached to the session come back by themselves. `--all` moves every
  running session; `-s name` another than the default.
- Inside a session, the status bar offers the same when a newer Astrum is
  installed than the session runs: `⬆ 0.8.1 installed · upgrade`. A click
  asks once more, and "Not now" puts the offer away.
- It is careful. Nothing a program writes during the move is lost. The new
  program is asked first whether it can take the session over, and then
  whether it can read exactly what was written down for it; if not, the
  session carries on as it was and says why. A move waits for an agent that
  is in the middle of something: it is refused, and asked for again.
- The installer's closing note now says `astrum upgrade`.

### A terminal that only watches

- `astrum -s work --watch` attaches a terminal that sees the session and
  sends nothing to it, for showing a session to someone, or keeping an eye
  on one from another screen. Nothing typed, pasted or clicked there
  reaches a shell, the agent or a menu; its status bar says
  `watching · read only`. It can look at another tab than the one you are
  on, and leave. No tab is sized for it, it is not sent what you copy, and
  it starts no session.

### Upgrading from 0.7.0

- This is the last upgrade that needs a restart. A session started by
  0.7.0 or earlier cannot be moved (`astrum upgrade` says so): restart it
  once, with `astrum kill` and then `astrum`; its tabs come back, the
  programs in them do not. From 0.8.0 on, `astrum upgrade` does it
  without stopping anything.
- Until then the two versions understand each other: a 0.8.0 `astrum`
  attaches to, lists, reloads and stops a session 0.7.0 started, and the
  other way round. `--watch` refuses a session older than 0.8.0, which
  would take the terminal for an ordinary one.
- Nothing new is needed to build from source: the same Rust (1.88), and no
  packages added.

### Known limitations

- A move onto a new Astrum does not carry: pictures in the scrollback; the
  rows of earlier commands (their output cannot be copied whole
  afterwards); what panes had said that needed you; each terminal's own
  view (all come back on the tab that was in front); autopilot given for
  the session. Connections to MCP servers and to other machines are made
  again (the shells on other machines are kept there, and found as after a
  dropped connection).
- A move between two different versions could only be tried against a
  build made for the purpose, there being no release after 0.8.0 yet; and
  not with a session run by launchd or systemd.
- A terminal that only watches cannot scroll a pane's history: a pane's
  place in its history is shared, and scrolling it would scroll it for
  whoever is working there. It is a guard against accidents, not against
  a person: anyone who can reach the session's socket is you.
- Programs Astrum started itself for a session (`ssh` to another machine,
  MCP servers) end at a move and are started again when needed; the ended
  ones stay in the process table until the session stops.
- The limitations listed under 0.7.0 still hold.

## 0.7.0 — 2026-11-15

### Everything that needs you

- Any pane, not only the agent, can now say it needs you, is at work, is
  done or has failed. Astrum hears it from a long command (one that runs
  at least `done_after_secs`, 10 unless set, in a pane you cannot see,
  marked at work while it runs and done or failed with its exit code when
  it ends); from the bell; from a desktop notification a program asks for
  (OSC 9, OSC 777, kitty's OSC 99), which Astrum used to ignore; and from
  the program-status sequence (`ESC ] 7501 ; state=…`) that coding agents
  run as programs, build tools and scripts can send, with a state, a kind
  of question, progress, a message and a task name. A program that asks
  what is understood is answered.
- It shows where an agent's news does: the tab's mark (`?`, `✓`, `✗`), the
  status bar's count (`? 2 need you`), and your terminal's notification
  ("Astrum: Tab 3 (api): `make test` failed (exit 2)").
- The overview is now **Agents and panes at a glance**: under each tab's
  agent, a row for whatever its panes are saying, with its message and
  progress; a click goes to that pane.
- Nothing is marked for the pane in front of you; a full-screen program and
  the line being typed are never at work; looking at a pane clears what it
  said. A program that says it is waiting for you stays marked until it
  says otherwise, or ends. Right-click a pane → **Don't tell me about this
  pane** to hear nothing more from it. Panes on other machines work the
  same way. What a program says is cleaned of control characters and
  characters that reorder text, and capped, before it is shown or sent on.

### From what a pane shows to the right place

- A file a pane names opens in your editor at the line: `src/app.py:42`,
  `src/app.py:42:7`, `src/App.cs(42,7)`, and `a/…` as `git diff` prints
  it, where the file is there. Point at it and the status bar shows it;
  `Ctrl+click` it, or right-click → **Open in editor** (or **Copy path**).
  It opens in a pane beside, with `$VISUAL` or `$EDITOR` (else `vi`), told
  the line its own way, and the pane closes with the editor.
- Right-click a command or its output → **Copy this command's output**
  copies everything it printed, however far back into the scrollback, or
  **Copy it with the command**. What was copied shows selected for a moment.
- **Previous prompt** and **Next prompt** in the pane's right-click menu,
  and `[` and `]` in copy mode, go from one command to the next.
- Right-click a pane → **Open the history in my editor**: its whole
  scrollback as plain text, in your editor, at the line you were looking
  at, from a file only you can read, which is removed when the editor ends.

### Sessions, and coming back to them

- The session's name at the right of the tab bar (now shown for `default`
  too) opens the list of sessions; so do `≡ menu` → Sessions… and
  `Ctrl+B S`. It lists running sessions and stopped ones that would come
  back, those in your folder or git repository first. A click moves this
  terminal there; the session it leaves keeps running, and so do other
  terminals on it. The list goes back to the last session, starts a new
  one (a name, and a folder), and stops or forgets one, asking first.
- After a restore, a pane says what was running in it before
  (`was running: npm run dev`), and the status bar offers **run again**
  (`Ctrl+B !`), which types it at the fresh prompt. Nothing runs by
  itself. A command line with a secret in it is never kept.

### The git chip does something

- Click the branch on the status bar (or `Ctrl+B g`): **Show the changes**
  opens the diff in a pane beside, through your git's pager; **Ask the agent
  to review my changes**; **Draft a commit message**, after which the agent
  proposes the commit, which is always asked about first, autopilot or not.
- A shell on another machine now shows the branch, and what has changed, of
  its folder there.

### Changes

- `≡ menu` leaves out Watch the running command, and Take the agent into
  this shell, when nothing is running that they could apply to.
- Shell integration's marks are now taken in step with what is drawn: a
  command's marks and its output arriving together are no longer drawn
  first and marked after.

### Upgrading from 0.6.1

- A session that is already running keeps running the 0.6.1 server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. Its tabs come back. The two versions understand each other in
  the meantime, but going between sessions from the list needs the new
  `astrum` in the terminal and the new server: an older `astrum` attached
  to a 0.7.0 session is told to run `astrum -s name` itself.
- `Ctrl+B S`, `Ctrl+B !` and `Ctrl+B g` are new. If you had given one of
  those keys to something else in `[keys]`, it stays yours, and the menu
  and the mouse still reach the new things. `s` stays the theme picker.
- The overview's status bar count now reads `? 1 needs you` (agents asking,
  and panes), where it read `? 1 agent asking`.
- A program that sends notifications or the program-status sequence in a
  pane you are not looking at now marks its tab and is told of. Right-click
  its pane → Don't tell me about this pane, or set `done_after_secs = 0`
  for long commands, if that is more than you want.
- A tab on another machine shows its branch once the Astrum there is
  0.7.0; an older one there keeps working without it.
- With `restore = "all"`, the saved session now also holds the command line
  running in each pane (never one with a secret in it).
- Nothing new is needed to build from source: the same Rust (1.88), and no
  packages added.

### Known limitations

- How each terminal shows a notification has still not been seen by hand.
- What needs you is not passed up to a terminal that itself understands the
  program-status sequence.
- File paths in a tab on another machine are not followed: the files are
  there. Its git menu has the agent's entries, not the diff.
- **Show the changes** shows no files git does not track, and with both
  staged and unstaged changes shows them together (`git diff HEAD`).
- The rows of commands from before a pane changed width are not found
  again for copying their output (lines rewrap at another width), nor
  those a `clear` took out of the scrollback.
- A session cannot be renamed from the list yet.
- In bash, a command typed with a space in front (left out of the history
  by `ignorespace`) is not known by its command line: it is not offered
  again after a restore, and its row says "A command".
- The git chip's counts and **Show the changes** run git in the folder a
  pane is in. A repository's own settings there can name programs git runs,
  as they would for `git status` typed in that folder.
- The limitations listed under 0.6.0 still hold.

## 0.6.1 — 2026-11-14

- In a git repository, the status bar shows the branch beside the folder
  (`git:main`), then what has changed: staged (`+`), changed and not staged (`*`),
  untracked (`?`), in conflict (`!`), and commits ahead of the upstream and
  behind it (`↑`, `↓`). A merge or rebase stopped half way is named. The
  branch is read from the repository's files, so it shows without git
  installed; the counts come from `git status`, run in the background and
  never taking the repository's lock. On a narrow terminal the counts give
  way first, then the branch. `git_status = false`, or Settings → Show the
  git branch beside the folder, turns it off.
- When an MCP server's program ends, the reason it gave is always shown:
  its last three lines, as for one that fails to start. Only the last line
  was shown, and when it wrote to both its output and its error output on
  the way out, that could be the wrong one.

## 0.6.0 — 2026-11-12

### Several agents at once

- Every agent at a glance: one list of every tab's agent, saying what each
  is doing (asking, at work and on what, finished, stopped), its model and
  its tokens. A click goes to it; one that asks to run a short command
  can be answered yes from the list, which shows that command whole. `Ctrl+B A`, `≡ menu`, `/agents`, or a click on the status
  bar, which now says how many agents elsewhere are asking or at work.
- The tab bar tells an agent that is asking (`?`) from one at work, and
  marks one that finished (`✓`) or stopped (`✗`) while you were elsewhere.
- When an agent in a tab you are not looking at asks something or ends,
  your terminal is told: a notification in iTerm2, WezTerm, Ghostty, kitty
  and foot, a bell elsewhere. `ASTRUM_NOTIFY` chooses the way.

### Skills

- Skills: folders with a `SKILL.md` (a name and a description up front,
  instructions after) and the files they bring, in the layout other agents
  use. Yours go in `~/.config/astrum/skills/`, a project's in its
  `.agents/skills/` or `.astrum/skills/`, and `skill_dirs` adds more.
- The agent is shown each one's name and description, and loads the rest
  when one fits what you ask; `/name` uses one on purpose, and the slash
  menu offers them.
- `/skills` lists them and turns one off or on; so does **Skills** in the
  settings.
- In a tab on another machine, your own skills are offered, and a script
  one brings is put on that machine when the agent needs it.

### MCP servers

- The agent can use the tools of MCP servers: programs on your machine,
  named under `[mcp.<name>]` in the config file with `command` and `args`,
  and services on the network, with `url`. They are started the first time
  the agent is asked something.
- Using one of their tools is asked about, with its arguments shown, as a
  command is; `allow` lists the tools of a server that need no asking, and
  `tools` limits which are offered at all. Autopilot uses them unasked.
- A program gets a small environment of its own, not your API keys, plus
  what `env` gives it. What it writes to its error output is kept for
  `/mcp log`.
- A server on the network that wants a token gets one stored with
  `/mcp key`; one that asks you to sign in is signed in to with
  `/mcp signin`, in your browser (OAuth), and the sign-in is kept in the
  password store and renewed when it runs out. Over SSH, `/mcp paste`
  takes the address the browser ended at.
- `/mcp` says how each server is doing, lists its tools and log, and turns
  one off or on for the session; so does **MCP servers** in the settings.

### A website, with the full documentation

- Astrum has a website: <https://robert-mcdermott.github.io/astrum/>. It
  says what Astrum is, shows it, and holds the user documentation, a page
  per subject, from getting started to every setting, with a search over
  it all. Its screenshots are made by Astrum itself, light and dark.
- The lists on it (every key, every setting, every theme) are made from
  Astrum, through two new options: `astrum themes --json` gives every
  theme's colors, and `astrum keys --json` every action with its keys.
- Every setting in the starter file (`astrum config --init`) now says what
  it does.

### Changes

- A table of your own under a built-in provider's name and of its kind,
  such as `[providers.bedrock]` with only a `region` and a `profile`, now
  keeps what it leaves out: the built-in address, and the variable its key
  is read from. Before, it replaced the built-in provider entirely, so a
  Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK` was not used, and an
  Anthropic key in `ANTHROPIC_API_KEY` was not found.

### For those working on Astrum

- `site/` holds the website: its pages, a generator (`node site/build.mjs`,
  Node and no packages) that checks every link, anchor and picture, and the
  scripted sessions its screenshots are taken from
  (`node site/capture/capture.mjs`). `site/README.md` says how to change a
  page, preview the site and publish it. The test suite builds the site
  against the binary under test where Node is present.

### Upgrading from 0.5.0

- A session that is already running keeps running the 0.5.0 server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. Its tabs come back. (The two versions understand each other in
  the meantime: a 0.6.0 `astrum` can attach to, list, reload and stop a
  session a 0.5.0 one started, and the other way round.)
- `Ctrl+B A` is new. If you had given it to something else in `[keys]`, it
  stays yours, and the list of agents is in the menu.
- A provider table of your own under a built-in's name now gets the
  built-in's address and key variable where it sets none (see Changes). If
  you meant it to have no key from the environment, give it `api_key` or
  another `api_key_env`.
- Nothing new is needed to build from source: the same Rust (1.88), and no
  packages added.

### Known limitations

- MCP: Astrum uses a server's tools only, not its resources or prompts,
  and answers no requests of its own from a server (no sampling, no
  roots). Signing in with OAuth was tried against a stand-in server, not
  yet against a hosted one.
- In a tab on another machine, your own skills are offered, but a
  project's skills there are not.
- A notification is sent as the bytes each terminal understands; how each
  terminal shows it was not seen.
- The limitations listed under 0.5.0 still hold.

## 0.5.0 — 2026-11-11

### Tabs on other machines

- A tab can be on another machine: `≡ menu` → New tab on another machine…,
  `Ctrl+B R`, or `/ssh host`. Astrum reaches it with your own `ssh` (your
  keys, jump hosts and config as they are; its questions are shown in the
  pane), and its own program there keeps the shells. They and what runs
  in them outlast a dropped connection, a closed laptop and a restart of
  Astrum here; the pane picks up where it left off, or is shown the
  screen as it now stands.
- Such a tab is an ordinary one: splits open on the same machine, and
  scrollback, search, links and pictures work. The tab and the status bar
  say which machine it is on.
- The agent works there with all its tools. Commands run in the shell
  there; reading, searching and changing files (with the diff to approve
  first) are carried out there. The model and your keys stay here.
- If Astrum is not on the machine, it asks, then copies the release build
  for it over the SSH connection into `~/.local/bin`, with the checksum
  verified on your machine first. No root, no package manager, and the
  machine needs no internet access. `download_base` names a mirror or a
  folder to take the build from.
- The two sides may be different versions as long as they speak the same
  link language (a number of its own, now 1). An older Astrum there is
  replaced only when you say so; a newer one never.
- A new key to change, `remote_tab`, and two settings in the environment,
  `ASTRUM_DOWNLOAD_BASE` and `ASTRUM_SSH`.

### The agent on another machine

- When the shell in a pane has taken you somewhere else (`ssh`, a
  container, `sudo -s`, a jump host), the agent can come along. The status
  bar offers "take the agent in" while such a program runs; `Ctrl+B i`,
  the menu and `/here` do the same. Astrum types the marks it uses into
  the shell you reached, clears what it typed, and from then on the agent's
  commands run there. The status bar shows whose shell it is and its
  directory.
- Nothing is installed on the other machine, and the model and your keys
  stay on yours. It needs bash, zsh or fish there. The agent has only the
  shell there, not its file tools.
- A command the agent runs while `ssh` has the pane and it has not been
  taken in still runs on your own machine, and the agent is now told that
  plainly, where it used to be told only that the terminal was busy.

### For those working on Astrum

- The same code is no longer tested twice. CI recognises code it has
  passed by its files, not by the commit or the branch: a pull request
  that is exactly its branch, a merge that brings in what the pull request
  tested, a push that changes only the roadmap or the changelog, and the
  release build for a tag on such code do not run the suite again.
  Releasing 0.4.0 tested one state of the code four times over; the same
  steps now test it once.
- `scripts/test.sh` does the same by hand: it runs the suite unless this
  exact code has passed already, here or in CI.

### Upgrading from 0.4.0

- A session that is already running keeps running the 0.4.0 server. After
  installing, restart each session to get the new one: `astrum kill`, then
  `astrum`. Its tabs come back. (The two versions understand each other in
  the meantime: a 0.5.0 `astrum` can attach to, list and stop a session a
  0.4.0 one started, and the other way round.)
- `Ctrl+B R` and `Ctrl+B i` are new. If you had given either key to
  something else in `[keys]`, it stays yours, and the new action is in the
  menu; `astrum keys` shows what each has.
- A machine that has Astrum 0.4.0 can be the far side of a tab once it has
  0.5.0. Astrum offers to put it in `~/.local/bin` there and does so only
  on a yes; sessions running there under 0.4.0 are not disturbed.
- Going back to 0.4.0, a saved tab that was on another machine comes back
  as a tab on this one.
- Nothing new is needed to build from source: the same Rust (1.88), and
  no packages added.

### Known limitations

- A tab on another machine needs Astrum there, so a machine where you may
  not run or keep a program of your own, or a system with no build (other
  than Linux on x86-64 or ARM64 and macOS on Apple silicon), cannot have
  one. `ssh` in an ordinary tab, with the agent taken in, works there.
- An account whose `ssh` forces a command (`RemoteCommand`,
  `ForceCommand`) cannot be used for one.
- Shells kept on another machine have no agent forwarding and no X
  forwarding, and `ssh` is run with the environment the session server
  was started with.
- After a long absence, or a restart of this machine, a pane is shown the
  screen and history as they now stand. Pictures are not carried over
  then, and links lose their targets.
- A tab closed while its connection is down leaves its shell running
  there until the session next reaches that machine.
- On a Linux system set to end a user's programs at logout, shells there
  end with your last logout unless lingering is on. Astrum says so when it
  connects.
- Taken into a shell you reached by hand, the agent has only that shell,
  not its file tools, and needs bash, zsh or fish there.
- The limitations listed under 0.4.0 still hold.

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
