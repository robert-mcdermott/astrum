---
title: Scripts and the command line
description: Reach a running session from a script or another terminal. Open a pane running a command, type into a pane, read what it shows, list the tabs and panes, and say how a script is doing.
---

## A session can be reached from outside

A running session answers on a socket that only you can open. Five commands use it. They work from any terminal, and from inside the session itself: a script running in a pane can open another pane, type into one, or say it needs you.

| Command | What it does |
| --- | --- |
| `astrum run -- CMD ARGS` | Opens a pane running the command, and prints the pane's id. |
| `astrum send TEXT` | Types text into a pane. |
| `astrum capture` | Prints what a pane shows. |
| `astrum list` | Lists the tabs and their panes. |
| `astrum status STATE [MESSAGE]` | Says how a script is doing: at work, done, waiting for you. |

Run outside a session they act on the session named with `-s` (`default` otherwise). Run inside one, they act on that session, and on the pane they are run in, unless told otherwise.

## Panes have ids

Every pane has a number, which `astrum list` shows and `astrum run` prints. Inside a pane it is in the environment, beside the session's name:

```bash
echo "$ASTRUM_SESSION $ASTRUM_PANE"
```

`--pane ID` names a pane to `send`, `capture`, `status` and `run --split`. Without it, the pane is the one the command is run in; from outside the session, the one the keyboard is in.

## Run a command in a pane

```bash
astrum run -- npm run dev
```

The command gets a pane of its own, in the folder `astrum run` was run from, and the pane closes when the command ends. It is run as it is given, not through your shell, so aliases and shell syntax do not apply; give it a shell if you want one (`astrum run -- sh -c 'make && make test'`). Its environment is the session's.

| Option | What it does |
| --- | --- |
| `--new-tab` | In a tab of its own. This is the default. |
| `--split right`, `--split down` | In a split beside, or below, the pane it is run in (or `--pane ID`). |
| `--wait` | Waits for the command to end, and exits with its exit code. |
| `--keep` | Leaves the pane up when the command ends, saying how it ended, until Enter. |
| `--focus` | Moves the keyboard to the new pane. Otherwise it stays where it was. |

With `--wait`, a script can use a pane the way it would use a command:

```bash
astrum run --split down --wait -- make test && astrum status done "Tests pass"
```

## Type into a pane, and read one

```bash
astrum send --pane 7 --enter "git pull"
```

`send` types the text into the pane's program, exactly as if it had been typed there; `--enter` presses Enter after it.

```bash
astrum capture --pane 7
```

`capture` prints what the pane shows now; `--history` prints everything it has kept, its scrollback included.

What is sent goes to a pane and nowhere else. It never reaches the agent's panel, so a question the agent is asking, or a command it wants approved, cannot be answered from a script: that stays with a person at a terminal. A command of the agent's own that uses `astrum send` or `astrum run` is asked about like any other command that changes something.

## List the tabs and panes

```bash
astrum list
```

```text
tab 1  shop  (in front)
  pane 1   ~/src/shop  at the prompt  *
  pane 4   ~/src/shop  running npm run dev
tab 2  deploy
  pane 6   ~/src/shop  running deploy eu-west  [needs-you: deploy needs you: Go ahead?]
```

`*` marks the pane the keyboard is in in its tab. `--json` gives the same as JSON, for a script: each pane's `id`, `cwd`, `state` (`prompt`, `running` or `unknown`), `command`, `machine` for a shell on another machine, and what it `says` it needs.

## Say how a script is doing

```bash
astrum status blocked "Roll out to the other 18 hosts?" --kind question
```

`status` says for the pane what a program can say with the [program-status sequence](several-agents.html#when-a-pane-needs-you): `working` (with `--progress 40`, how far), `done`, `blocked` (it needs you; `--kind` says whether for `permission`, a `question` or `auth`), `error`, and `idle` or `clear` to take it back. The tab is marked, the status bar counts it, and your terminal is told, as for anything else that needs you.

A state a script sets lasts while the script runs: when the shell is back at its prompt, what the script said it was doing is over.

## What these need

The session has to be running, and to run Astrum 0.9.0 or later (`astrum upgrade` moves an older one onto it without stopping anything, from 0.8.0 on).
