---
title: Troubleshooting
description: What to do when something does not work as it should, from colors, keys and copying to the agent and MCP servers, and where Astrum keeps its files.
---

## Starting Astrum

### `cargo` or `astrum` is not found

If you built Astrum from source, `~/.cargo/bin` is not on your `PATH`. Add
this line to your shell's startup file:

```bash
. "$HOME/.cargo/env"
```

If you used the install script, Astrum is in `~/.local/bin`. When that folder
is not on your `PATH`, the script says so and prints the full path to run;
add the folder to your `PATH` to run plain `astrum`.

### An old version runs after an upgrade

`astrum --version` says which version runs. A copy installed with
`cargo install` is in `~/.cargo/bin`, and one from the install script is in
`~/.local/bin`. Whichever of the two folders comes first on your `PATH` is
the one that runs. Remove the other copy (`cargo uninstall astrum` for the
first), or keep updating the one that runs the way you installed it.

### New features are missing after an upgrade

A session that was already running is still on the old server. Restart it:

```bash
astrum kill
```

```bash
astrum
```

Its tabs come back. Add `-s name` for a session other than the default. The
same restart is the answer when `astrum` says that it speaks one protocol and
the running session another, or that a session did not answer because it was
started by an older `astrum`.

### "You are already inside Astrum"

`astrum` was run in a shell that is itself in Astrum. For another tab, click
`+` in the tab bar (or press <kbd>Ctrl+B</kbd> <kbd>c</kbd>). To start Astrum
inside Astrum anyway, set `ASTRUM_ALLOW_NESTED=1` first.

### "The session server did not come up"

`astrum` starts a session's server in the background and waits a few seconds
for it to answer. When it does not, the server's log says why. The message
names the log: `/tmp/astrum-<uid>/<session>.log`, which is `default.log` for
the default session.

Two other messages at the start:

- **"astrum needs to run in a terminal"**: its input or output is not a
  terminal, as in a script or a pipe.
- **"... is not owned by the current user"**: Astrum keeps its sockets in
  `/tmp/astrum-<uid>`, and will not use a folder of that name that belongs
  to someone else. Set `ASTRUM_RUNTIME_DIR` to a folder of your own.

### An `astrum` process is using a whole processor core

On 0.1.0, closing a terminal window could leave its client behind, spinning.
It does no harm to your sessions. It ignores an ordinary `kill`; end it with
`kill -9`. Later versions exit on their own.

### Something looks stuck

`astrum ls` shows the sessions that are running, and `astrum kill` stops one.
That is a restart rather than a loss: the tabs come back the next time the
session starts, each with a fresh shell in its old directory.
`astrum kill --forget` discards the tabs as well. The server's log is in
`/tmp/astrum-<uid>/`.

If it is a program in one pane that is stuck, close that pane: right-click
it (holding <kbd>Shift</kbd> if the program uses the mouse) and choose
**Close pane**, or **Close tab** when it is the tab's only pane. The rest of
the session is not affected.

## Settings

### A change in the config file does nothing

In the order they are most likely:

- **It was chosen inside Astrum.** A setting chosen on the settings screen,
  with `/theme` or `/model`, or by stepping the autonomy level, is kept in
  `state.toml` and takes the place of the config file's value. `≡ menu` →
  Settings → **Forget the choices made here** hands everything back to the
  file. See [Choices made inside Astrum](settings.html#choices-made-inside-astrum).
- **Astrum does not know the name.** A misspelled setting, or one written
  under the wrong table, is passed over without a word. Settings for the top
  level have to come before the first line in square brackets: below
  `[agent]`, a line belongs to `[agent]`.
- **The file has a mistake.** Astrum then said **Config problem, nothing was
  changed** and the reason. `astrum keys` prints what is wrong with the file,
  after its list of keys, and `astrum themes` what is wrong with a theme of
  your own.
- **It waits for what comes next.** `scrollback` and `shell` apply to shells
  started afterwards, and `truecolor` to terminals that attach afterwards.
- **It is an environment variable.** A running session keeps the environment
  it started with. `astrum reload` passes along your providers' key
  variables; for anything else, restart the session.

### Every setting went back to its default

A session that starts with a config file it cannot read uses the defaults in
place of everything the file says (the choices made inside Astrum still
apply), and says **Config problem** with the reason. Correct the file and
save it: the session takes it at once.

## Colors

### Colors look reduced

Astrum did not detect 24-bit color support, which is common over SSH. Set
this in the config file, then detach and run `astrum` again, since it applies
to terminals that attach afterwards:

```toml
truecolor = true
```

### Colors are wrong or garbled

The terminal says it has 24-bit color and does not show it properly. Set
`truecolor = false` to send it the 256-color palette instead, then detach and
attach again.

### Programs look different after choosing a theme

A chosen theme colors what programs draw as well as Astrum's own bars. To
keep programs in your terminal's own colors, turn off **Programs use the
theme's colors too** on the settings screen, or set `theme_terminal = false`.
See [Themes](themes.html#colors-for-programs-too).

### The light theme is not used

`theme_light` is used when the terminal says its background is light. Astrum
asks it each time it attaches; a terminal that does not answer counts as
dark. After changing your terminal from dark to light, detach and attach
again so that it is asked again.

## Keys and the mouse

### <kbd>Ctrl+Space</kbd> does something else

macOS uses it to switch input sources when several keyboard layouts are
enabled, and some shell plugins bind it. Pick another key in the config
file, such as one of these:

```toml
agent_key = "ctrl+backslash"
```

```toml
agent_key = "f2"
```

The `SHELL` chip on the status bar always switches between the shell and the
agent with a click.

### A program never receives <kbd>Ctrl+B</kbd>

The prefix key, <kbd>Ctrl+B</kbd> unless you changed it, is Astrum's. Press
it twice to send it to the program in the pane, or choose another prefix
with `prefix` in the config file. The same goes for `agent_key`, and for any
key you gave an action in `[keys]` with `ctrl` or `alt`: those work without
the prefix, so programs no longer receive them. `astrum keys` lists what each
key does now.

### Keys with Alt do nothing on a Mac

The terminal app has to send the Option key as Meta for Astrum to see Alt.
That is a setting of the terminal: in macOS Terminal it is "Use Option as
Meta key", and in iTerm2 it is the Option key set to "Esc+".

### Typing goes to the agent, not the shell

The status bar says which one you are typing into. Click the `SHELL` or
`AGENT` chip, or press <kbd>Ctrl+Space</kbd>, to switch. Other places your
keys can go instead of the shell:

- **Copy mode** (<kbd>Ctrl+B</kbd> <kbd>[</kbd>): nothing typed in it reaches
  the shell. <kbd>q</kbd> or <kbd>Esc</kbd> leaves it.
- **The find bar**: <kbd>Esc</kbd> or its `✕` closes it.
- **After the prefix**: for ten seconds the status bar waits for the key that
  follows. <kbd>Esc</kbd> lets go at once.

### Keys are ignored when the agent asks something

That is on purpose. If a question appears while you are typing, the keys you
were about to press are not taken as its answer: they are ignored for as
long as you go on typing, and the question waits until you have stopped for
a second, so that a stray `y` cannot approve a command you have not read.
The exception is <kbd>Ctrl+C</kbd>, which stops the agent at once.

### Cannot select text in a full-screen program

Programs that use the mouse themselves (vim, htop) receive it. To select
text inside them, hold <kbd>Shift</kbd> while dragging; hold it while
right-clicking to get Astrum's menu rather than theirs.

### <kbd>Ctrl+click</kbd> does not open a link

Some terminals keep <kbd>Ctrl+click</kbd> for themselves, such as macOS
Terminal. Right-click the link and choose **Open link** instead; that works
everywhere.

### The shell is drawn too small, with a note

Another terminal window is showing the same tab, and the tab's shells are
sized for the window that used it last. Press a key or click in this window
to bring them over to its size.

## Copying and links

### Copied text does not reach the clipboard

When you copy, Astrum says **Copied** and how many characters, and sends the
text two ways at once:

- to your terminal, asking it to put the text on its clipboard (the OSC 52
  sequence). This reaches the terminal you are looking at even over SSH.
- to the clipboard program of the machine `astrum` runs on: `pbcopy` on
  macOS, `wl-copy` or `xclip` on Linux.

On your own machine the second way is enough. When you run `astrum` on
another machine over `ssh`, only the first can reach your clipboard, and not
every terminal accepts it: some have it turned off until you allow programs
to use the clipboard in their settings. (In iTerm2 the setting is
"Applications in terminal may access clipboard".) A
[tab on another machine](other-machines.html) avoids the question, because
there `astrum` runs on your own machine.

`ASTRUM_CLIPBOARD_TOOL` names another program to hand copied text to, or
`none` to leave it to the terminal. With `copy_on_select = false`, a mouse
selection is not copied when you let go; select with the keyboard
(<kbd>Ctrl+B</kbd> <kbd>[</kbd>) and press <kbd>y</kbd>.

### A link does not open, or opens on the wrong machine

Astrum opens web and mail links and nothing else. A link to a file is not
opened, because opening a file can mean running it, but it can still be
copied: right-click it and choose **Copy link**.

A link is opened with `open` or `xdg-open` on the machine `astrum` runs on.
When that is another machine you reached over `ssh`, copy the link instead.
`ASTRUM_OPEN_TOOL` names another program to open links with, or `none`.

## Pictures

### A picture shows as a faint block

The line on the block says why:

- **"this terminal cannot show it"**: your terminal has no way to draw that
  kind of picture (see the table below). If you know yours can, say so with
  `ASTRUM_IMAGES` before starting `astrum` (`kitty`, `iterm`, `sixel`, or
  several with commas).
- **"images are off"**: `images = false` is set in the config file.
- **Only what the picture is**: it is partly off the edge of its pane or under
  a menu. A terminal cannot be asked for part of a picture, so it is drawn
  whole or not at all; scroll it fully into view or close the menu.

A picture is drawn once it has been still for a moment, not at every line
while output is pouring past. Each terminal attached is sent pictures in a
way it has:

| Your terminal | What it is shown |
| --- | --- |
| iTerm2, WezTerm | Everything, except raw pixels a program sent compressed in kitty's way |
| Ghostty, kitty | Pictures sent in kitty's way, and PNG files sent in iTerm2's |
| A terminal that only draws sixel | Sixel pictures |
| macOS Terminal, and others with no pictures | A line saying what the picture is |

### A program prints no pictures in Astrum

A program finds out whether pictures work by asking, and is answered for the
terminal you are using. One that decides from the name of the terminal sees
`TERM_PROGRAM=astrum` and may need telling which way to send them, such as
`chafa -f kitty`, or `viu` with its protocol option.

### Pictures are the wrong size

Astrum learns how large your terminal's cells are when it attaches. After
changing the font size, detach and attach again before printing pictures.
Sixel pictures are shown only on a terminal that says how large its cells
are, and only while they are the size they were when the picture was
printed.

### A JPEG or a moving picture is not shown

Astrum passes pictures on and never decodes them. So a JPEG cannot be shown
on a terminal that only has kitty's way of taking pictures (Ghostty, kitty),
and nothing can be shown on a terminal that only draws sixel unless it was
sent as sixel. Pictures that change many times a second, such as a video
player drawing to the terminal, are not shown: no frame stays still long
enough.

## The agent

### "Pick a model first"

No model has been chosen yet, so the list of models opens. Choose one, then
press <kbd>Enter</kbd> again to send what you typed. The choice is
remembered. To make a model the default for every session, set `provider`
and `model` under `[agent]` in the config file.

### `/model` lists no models

When nothing is found, the list says so: start Ollama, set a provider's key,
or add a provider to the config file. When some providers answered and others
did not, the bottom line of the list names those it could not reach, and why.

- **Ollama** has to be running on `localhost:11434`.
- **A hosted OpenAI-compatible provider** without a key is not asked for its
  models at all. Give it a key, or list its models in `models`.
- **Amazon Bedrock** lists the models of one region: see
  [below](#bedrock-models-are-missing-or-requests-are-refused).

`/providers` shows every provider that is configured, its address, and
whether it has a key.

### The agent answers with an error

A failed request is shown in the agent panel with what the provider said.
The common ones:

| The panel says | What to do |
| --- | --- |
| `HTTP 401` or `HTTP 403` (check the provider's API key) | The key is wrong, or there is none. `/key` alone shows where each provider's key comes from. |
| `HTTP 404` (check the model name and base_url) | The model is not one this endpoint has, or `base_url` is wrong. |
| `HTTP 429` (rate limited; try again shortly) | The provider is refusing requests for a while. Ask again shortly. |
| `could not reach` and an address | Nothing answered there: the server is not running, the address is wrong, or there is no network. |

With Amazon Bedrock, expired temporary credentials show up as Bedrock's own
error. Refresh them and ask again; Astrum re-reads profile credentials every
ten minutes.

### A new API key is not picked up

The background server reads the environment when it starts. Run this in the
shell that has the new value:

```bash
astrum reload
```

Or store the key with `/key` instead, which needs no shell setup. A key is
taken from the first of these that has one: `api_key` in the config file,
then the key stored with `/key`, then the environment variable. So an old
`api_key` in the file wins over a new key anywhere else. `/key` alone says
where each provider's key is coming from.

### Bedrock models are missing, or requests are refused

Check that `region` under `[providers.bedrock]` is the region where your
models are enabled. Without it, Astrum uses `AWS_REGION` or
`AWS_DEFAULT_REGION`, then your profile's region, then `us-east-1`. Filling
the `/model` list needs `bedrock:ListInferenceProfiles` and
`bedrock:ListFoundationModels`; without them, name models yourself with
`models` under `[providers.bedrock]`. Running the agent needs
`bedrock:InvokeModelWithResponseStream`. Single sign-on profiles need the AWS
CLI, and `aws sso login` first. More in [Providers and models](providers.html).

### The agent waits on a command

A command that takes a long time does not hold the conversation up: after
`command_timeout` seconds (60 unless set) the agent is told it is still
running, and how it is doing. If the command asks for a password, press
the agent key (<kbd>Ctrl+Space</kbd>) and type it yourself in the pane.
Pressing <kbd>Esc</kbd> stops the agent and interrupts the command it was
running.

### `# question` ran in the shell instead of going to the agent

Astrum only takes a line it is sure about. It needs the shell integration
(zsh, bash, fish), a line typed after the prompt appeared, and, for a line
you edited with the cursor keys or recalled from history, a prompt that has
not changed since you started typing. <kbd>Ctrl+Space</kbd> always works.
`ask_prefix = ""` under `[agent]` turns asking from the prompt off.

### `explain`, `fix` and `watch` never appear

They need the shell integration, which marks where each command starts and
how it ended. zsh, bash and fish have it, loaded by Astrum without changing
your files. A shell of another kind, or one started inside another
(`sudo -s`), has none.

### "Nothing there answered" after taking the agent in

The shell you reached did not answer the line Astrum typed: it is not at a
prompt, or it is not a shell (a database console, a pager). The line may
still be sitting there; clear it with <kbd>Ctrl+U</kbd> or <kbd>Ctrl+C</kbd>.
It needs bash, zsh or fish on the other side.

### The agent compacts too early or too late

It goes by the model's context size, which it may be assuming. `/usage`
shows the figure in use. Set `context_window` for the model under
`[models."name"]` to correct it. A model Ollama runs on your own machine is
one to set: Ollama serves it with the context it was configured to give it,
which can be much less than the model's maximum.

### No notification when an agent finishes

You are told about agents in tabs you are not looking at: with a
notification in iTerm2, WezTerm, Ghostty, kitty and foot, and with a bell in
other terminals. The terminal is recognized from what it sets in the
environment, which a connection over `ssh` may not carry. Set
`ASTRUM_NOTIFY` before starting `astrum` to say which way your terminal
takes: `osc9`, `osc99` or `osc777`, or `none` for the bell alone.

## API keys and passwords

### "Keys cannot be stored here"

`/key` keeps keys in the system's password store: the macOS keychain, or on
Linux the desktop's keyring, through `secret-tool` (in the package
`libsecret-tools`). Without `secret-tool` there is nowhere to keep them, and
on a machine with no desktop session the keyring it talks to may not be
there either. Use an environment variable for the key instead (see
[Providers and models](providers.html)), or `api_key` in the config file.

### A session run as a login service cannot find your keys

A server started by the [login service](login-service.html) has only a bare
environment, so the keys set in your shell's startup files are not there.
Run `astrum reload` from a shell that has them, or store them with `/key`.
The same bare environment is what `ssh` runs with in that session, so a key
agent your shell knows of (`SSH_AUTH_SOCK`) may not be found: whatever `ssh`
asks instead is shown in the pane for you to answer.

## MCP servers

### A server does not start

`/mcp` lists the servers and how each is doing:

| `/mcp` says | What it means |
| --- | --- |
| not started yet | Servers start the first time the agent is asked something, not when the session starts. |
| starting | It is on its way. A question to the agent waits a while for it. |
| ready | Its tools are offered to the agent. |
| failed, and why | It could not be started, or it stopped. For a program, the reason includes the last of what it wrote to its error output. |
| asks to be signed in to | It wants a token or a sign-in: see below. |
| off | `enabled = false` in the config file, or turned off for this session with `/mcp off name` or the settings. |

While a program runs, `/mcp log name` shows the last lines it wrote to its
error output. `/mcp restart name` starts one again after you have put it
right.

A server that is not in the list at all has an entry that cannot be used as
written: it needs `command` or `url` and not both, its name may have only
letters, digits, `-` and `_`, and a `url` elsewhere than on this machine has
to be `https://`. A session that starts with such an entry leaves that
server out; a change to the file that brings one in while the session runs
is refused as a whole, with **Config problem, nothing was changed**. Either
way Astrum says which entry and why, and `astrum keys` prints it too.

### "`npx` was not found"

A server's program is looked for on the `PATH` of the session's server, as
it was when the session started. A session run as a login service has only a
bare environment, so a program in a folder your shell's startup files add to
`PATH` is not found there. Give the server its `PATH` in `env`:

```toml
[mcp.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"]
env = { PATH = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin" }
```

A server's program gets only a small environment of its own, not your API
keys. What it needs beyond that goes in `env`, where `${NAME}` is a variable
of the session's own environment, or in `token_env` with the token stored by
`/mcp key name`.

### A server asks to be signed in to

`/mcp signin name` opens your browser at the server's sign-in page, and the
browser comes back to Astrum when you are done; `/mcp key name` stores a
token instead, for a server that takes one. Used over SSH, the browser cannot
come back to this machine: it ends at an address that does not load. Copy
that address and give it to Astrum:

```text
/mcp paste https://...
```

The sign-in is kept in the password store and renewed when it runs out.
`/mcp signout name` forgets it.

### A server stopped

A server that had been running for more than half a minute and then ends is
started again by itself, up to three times. Otherwise it is marked as failed,
and Astrum says why, with the last thing it wrote to its error output.
`/mcp restart name` starts it again.

### A tool takes too long

A tool may take 120 seconds unless the server's entry sets `timeout` to some
other number of seconds.

### A tool works on the wrong machine

MCP tools act on your own machine, wherever the tab is. In a tab on another
machine, or with the agent taken into an `ssh`, the agent is told that the
servers are not where its shell is.

## Tabs on other machines

### A tab on another machine cannot be opened there

It needs Astrum's own program on that machine, so it cannot be used:

- on a machine where you may not run a program of your own, or keep one
  running in the background (some shared hosts, restricted shells, and
  containers that are torn down when you disconnect);
- on a system there is no build for: anything but Linux on x86-64 or ARM64
  and macOS on Apple silicon;
- with an account whose `ssh` setup forces a command of its own
  (`RemoteCommand` in your config, `ForceCommand` on the server).

There, run `ssh` in an ordinary tab and take the agent in with
`take the agent in` on the status bar. If the Astrum on the other machine is
newer than yours, nothing there is replaced: update this machine. More in
[Other machines](other-machines.html).

### Shells there end when you log out of that machine

A Linux system set to end a user's programs at logout
(`KillUserProcesses=yes`) needs lingering turned on, and Astrum says so when
it connects. Run this on that machine:

```bash
loginctl enable-linger
```

### `ssh` behaves differently in such a tab

`ssh` is run with the environment the session server was started with, so it
uses the key agent from then. Shells on the other machine belong to no one
connection, so they have no agent forwarding and no X forwarding.

## Where Astrum keeps things

| What | Where |
| --- | --- |
| Settings | `~/.config/astrum/config.toml` |
| Choices made inside Astrum | `~/.config/astrum/state.toml` |
| Your rules and skills | `~/.config/astrum/AGENTS.md` and `~/.config/astrum/skills/` |
| Saved sessions, readable only by you | `~/.local/share/astrum/sessions/` |
| Saved conversations, readable only by you | `~/.local/share/astrum/conversations/` |
| The shell integration, written again when a session starts | `~/.local/share/astrum/shell/` |
| Builds of Astrum kept to copy to other machines | `~/.local/share/astrum/builds/` |
| Session sockets and each session server's log | `/tmp/astrum-<uid>/`, as `<session>.sock` and `<session>.log` |
| The login service | `~/Library/LaunchAgents/io.github.robert-mcdermott.astrum.<session>.plist` on macOS, `~/.config/systemd/user/astrum-<session>.service` on Linux |
| The login service's log on macOS | `~/.local/share/astrum/service-<session>.log` |
| Keys stored with `/key` and `/mcp key`, and MCP sign-ins | The macOS keychain, or the desktop keyring on Linux, as entries named `Astrum: <name> API key` (for an MCP server, `<name>` is `mcp.` and its name) |
| What an MCP server's program wrote to its error output | Its last 40 lines, kept in memory while it runs: `/mcp log name` |
| On another machine | Astrum in `~/.local/bin/astrum`, its log in `/tmp/astrum-<uid>/hold.log` there, and your skills' scripts in `~/.cache/astrum/skills/` |

When `XDG_CONFIG_HOME` is set, the settings folder is
`$XDG_CONFIG_HOME/astrum` in place of `~/.config/astrum`, and when
`XDG_DATA_HOME` is set, the data folder is `$XDG_DATA_HOME/astrum`.
`ASTRUM_CONFIG_DIR`, `ASTRUM_DATA_DIR` and `ASTRUM_RUNTIME_DIR` name other
folders for those two and for `/tmp/astrum-<uid>`: see
[Environment variables](settings.html#environment-variables).

## Starting fresh

Take only as much away as you need:

- **A session's tabs**: `astrum kill --forget` stops the session and discards
  what it would bring back (add `-s name` for another session).
  `astrum kill --all --forget` does it for every session, stopped ones
  included. A session also ends for good when you close its last shell.
- **The choices made inside Astrum**: `≡ menu` → Settings → **Forget the
  choices made here**.
- **A conversation**: `/conversations`, then <kbd>Del</kbd> twice on it. To
  keep none on disk from now on, turn off **Keep conversations so they can be
  resumed** on the settings screen.
- **The walkthrough again**: `≡ menu` → Settings → **Show the walkthrough**.

To take everything away and start as on the first day:

1. Run `astrum service uninstall` for each session you run as a login
   service (with `-s name` for others), from a terminal outside that session.
2. In the agent panel, `/key name remove` for each stored key, and
   `/mcp key name remove` and `/mcp signout name` for each MCP server.
3. Stop every session and discard its tabs: `astrum kill --all --forget`.
4. Delete `~/.config/astrum`, which holds your settings, rules and skills,
   and `~/.local/share/astrum`.

The next `astrum` then starts as the first one did, with the walkthrough.
