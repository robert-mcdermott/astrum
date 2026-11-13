---
title: Several agents at once
description: Each tab has an agent of its own. See what every one is doing, answer one from wherever you are, and be told when one needs you.
---

## An agent in every tab

Each tab has an agent of its own, with its own conversation, and they work at
the same time: one can run the tests in one tab while you ask another about
something else. Ask, go to another tab, and carry on. You need not keep
checking on them: the tab bar and the status bar say how each is doing, one
list shows them all, and your terminal is told when one needs you.

Within its tab, an agent works in the pane the keyboard was in when you asked,
and stays there until it has finished (see [The agent](agent.html#asking)).

## The tab bar

A mark before a tab's name says how its agent is doing:

| Mark | What it means |
| --- | --- |
| a spinner | The tab's agent is at work. |
| `?` | The agent is asking you something. |
| `✓` | The agent finished while you were in another tab. |
| `✗` | The agent stopped with an error while you were in another tab. |

`✓` and `✗` stay until you look at the tab. Other news is marked with `●`: a
command that finished, or a bell, in a tab you have not looked at since; a `●`
in another color says that a command is still running there.

## The status bar

When agents in other tabs are asking or at work, the status bar says how many:
`? 1 agent asking`, or a spinner and `2 agents working`. While any agent is
asking, those are the ones it counts. Click it to see them all.

## Every agent at a glance

One list shows every tab's agent. To open it:

- click the count on the status bar (`? 1 agent asking`, `2 agents working`);
- choose `≡ menu` → Every agent at a glance;
- press <kbd>Ctrl+B</kbd> <kbd>A</kbd> (a capital A);
- type `/agents` in the agent panel.

{{shot agents | A list titled Agents over a shell, with a row for each of five tabs: one asking to run pytest, with a Yes button; one running a build; one finished; one stopped, with the provider's error; and one with no conversation. Each row has an Open button | Every tab's agent in one list}}

Each tab has a row: its number and name, what its agent is doing, and on a
second line what it is on, its model and the tokens its conversation has used.
A `●` marks news you have not seen.

| The row says | What it means |
| --- | --- |
| `asks: Run this command?` | It is waiting for your answer. The second line has the command, or the start of what it would change. |
| `Running command for 1m 20s` | It is at work, and has been for that long. The second line has the command or the tool it is on. |
| `following a command it started` | A command it started is still running after the agent stopped waiting for it. |
| `finished 3m 2s ago` | It is done. |
| `stopped 40s ago` | It ended with an error, which the second line gives. |
| `stopped by you 5s ago` | You stopped it. |
| `ready` | There is a conversation, and nothing is going on. |
| `no conversation` | Nothing has been asked in that tab yet. |

Click a row, or its `Open` button, to go to that tab's agent: the tab comes to
the front with the agent panel open and the keyboard in it. With the
keyboard, <kbd>Up</kbd> and <kbd>Down</kbd> choose a row, <kbd>Enter</kbd> goes to it,
and <kbd>Esc</kbd> closes the list. When the list opens, the row already
chosen is the first agent that is asking, else the first with news, else the
tab in front.

## Answering from the list

The row of an agent that asks to run a command has a **Yes** button when the
list can show you the whole command: one line, short enough to fit, with no
warning attached. Click it, or choose the row and press <kbd>y</kbd>, to answer
yes without leaving where you are. It is the same as **Yes** on the question
itself: the command runs.

Everything else is answered in its tab: a command of several lines or too
long to show, one that comes with a warning, a change to a file, an MCP tool,
and any answer but yes (editing the command first, or declining). For those
the row has only **Open**. A question that has only just appeared takes no
yes from the list for a moment, so a click meant for the one before it is not
taken as an answer.

## Being told when an agent needs you

When an agent in a tab you are not looking at asks something, finishes, or
stops with an error, your terminal is told: with a notification such as
"Astrum: The agent in tab 2 (tests) is asking you something" where the
terminal is known to show one, and with the bell otherwise:

| Terminal | How it is told |
| --- | --- |
| iTerm2, WezTerm, Ghostty | A notification, sent as OSC 9. |
| kitty | A notification, sent as OSC 99. |
| foot | A notification, sent as OSC 777. |
| any other, or `astrum` run inside a terminal multiplexer | The bell. |

Astrum recognizes the terminal from the environment `astrum` is started in
(`TERM_PROGRAM`, `TERM` and the like). Where that does not say, as can happen
over SSH, or to choose another way, set `ASTRUM_NOTIFY` before starting
`astrum`:

```bash
ASTRUM_NOTIFY=osc777 astrum
```

`osc9`, `osc99` and `osc777` choose that kind of notification, and `none` the
bell alone.

Each terminal window attached to the session is told on its own, in its own
way, and a window that has that tab in front is not told at all. Whether a
notification then appears on screen is up to the terminal and the system's
notification settings.

## What the agents share

- **The model.** One model is chosen for the session: choosing another in one
  tab changes it for every tab's agent from its next request. See [Providers
  and models](providers.html).
- **The autonomy level** is the session's too. A **Yes, don't ask again this
  session** given to one tab's agent applies to that tab alone. See [How much
  it does on its own](agent.html#how-much-it-does-on-its-own).
- **MCP servers** are the session's, and every tab's agent uses the same ones.
  See [MCP servers](mcp.html).
- **Rules and skills** come from the files of each tab's own directory, with
  your own beside them. See [Rules and skills](rules-and-skills.html).
- **Conversations** are each tab's own, and are saved one by one.
  `/conversations` lists them all, with the tab each one is open in; a
  conversation can be open in one tab at a time. See
  [Conversations](agent.html#conversations).
