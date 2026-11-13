---
title: The mouse and the keys
description: Every click and menu in Astrum, then the keys: how the prefix key works, how to change the keys, and the full list.
---

## Everything with the mouse

Nothing in Astrum needs a keyboard shortcut. The tab bar, the status bar and every pane can be clicked, right-clicking a tab or a pane gives a menu, and `≡ menu` at the bottom right lists everything else. The keys are there for people who want them, and every menu shows the key beside its entry.

| Do this | With the mouse | With the keyboard |
| --- | --- | --- |
| New tab | click `+` in the tab bar | <kbd>Ctrl+B</kbd> <kbd>c</kbd> |
| New tab on another machine | `≡ menu` → New tab on another machine… | <kbd>Ctrl+B</kbd> <kbd>R</kbd> |
| Go to a tab | click it, or turn the wheel over the tab bar | <kbd>Ctrl+B</kbd> <kbd>n</kbd> / <kbd>p</kbd> / <kbd>1</kbd> to <kbd>9</kbd> |
| Move a tab | drag it along the tab bar | <kbd>Ctrl+B</kbd> <kbd><</kbd> / <kbd>></kbd> |
| Rename a tab | right-click it → Rename | <kbd>Ctrl+B</kbd> <kbd>,</kbd> |
| Close a tab | click its `✕`, or middle-click it | <kbd>Ctrl+B</kbd> <kbd>x</kbd> in its last pane, or `exit` |
| Split a pane | right-click it → Split right / Split down | <kbd>Ctrl+B</kbd> <kbd>\|</kbd> / <kbd>-</kbd> |
| Go to another pane | click it | <kbd>Ctrl+B</kbd> and an arrow key |
| Resize panes | drag the divider between them | |
| Zoom a pane, and back | right-click it → Zoom / Unzoom | <kbd>Ctrl+B</kbd> <kbd>z</kbd> |
| Close a pane | right-click it → Close pane | <kbd>Ctrl+B</kbd> <kbd>x</kbd>, or `exit` in its shell |
| Scroll back | turn the mouse wheel over the pane | <kbd>Shift+PageUp</kbd> / <kbd>Shift+PageDown</kbd> |
| Find in the scrollback | right-click the pane → Find…, then `↑` `↓` on the search bar | <kbd>Ctrl+B</kbd> <kbd>/</kbd> |
| Copy text | drag across it (copied when you let go); double-click a word | <kbd>Ctrl+B</kbd> <kbd>[</kbd> (copy mode) |
| Open or copy a link | right-click it → Open link / Copy link, or <kbd>Ctrl</kbd>-click it | |
| Talk to the agent | click the `SHELL` chip | <kbd>Ctrl+Space</kbd> |
| Show or hide the agent panel | `≡ menu` → Talk to the agent / Hide the agent panel | <kbd>Ctrl+B</kbd> <kbd>a</kbd> |
| Resize the agent panel | drag the line between it and the shell | |
| Explain or fix a failed command | click `explain` or `fix` on the status bar | <kbd>Ctrl+B</kbd> <kbd>e</kbd> / <kbd>f</kbd> |
| Have the agent watch a running command | click `watch` on the status bar | <kbd>Ctrl+B</kbd> <kbd>o</kbd> |
| Take the agent into a shell you reached | click `take the agent in` on the status bar | <kbd>Ctrl+B</kbd> <kbd>i</kbd> |
| See every tab's agent | click `agents working` (or `asking`) on the status bar, or `≡ menu` → Every agent at a glance | <kbd>Ctrl+B</kbd> <kbd>A</kbd> |
| Choose a model | click the model on the status bar | <kbd>Ctrl+B</kbd> <kbd>m</kbd> |
| Change how much the agent may do | click `ask first` (or the level shown) on the status bar | <kbd>Shift+Tab</kbd> in the agent panel |
| Choose a theme | `≡ menu` → Choose a theme | <kbd>Ctrl+B</kbd> <kbd>s</kbd> |
| Settings | `≡ menu` → Settings | <kbd>Ctrl+B</kbd> <kbd>.</kbd> |
| See every key | click `? help` on the status bar | <kbd>Ctrl+B</kbd> <kbd>?</kbd> |
| Leave, keep everything running | close the window, or `≡ menu` → Detach | <kbd>Ctrl+B</kbd> <kbd>d</kbd> |
| Detach every window from the session | close each window | <kbd>Ctrl+B</kbd> <kbd>D</kbd> |

### The tab bar

- Click a tab to go to it. Press on it and drag to move it along the bar.
- Click a tab's `✕`, or middle-click the tab, to close it.
- Click `+` for a new tab.
- Turn the mouse wheel anywhere over the tab bar to step through the tabs.
- Right-click a tab for its menu.

### In a pane

- Click a pane to type in it.
- Drag across text to select it; it is copied when you let go. Double-click selects a word, and a third click the whole line.
- Turn the wheel to scroll whichever pane the pointer is over, without moving the keyboard there.
- Point at a link and the status bar shows where it goes. Right-click it, or <kbd>Ctrl</kbd>-click it, to open it.
- Drag the line between two panes to resize them.
- Right-click for the pane's menu.

### The status bar

Every piece of the status bar that does something can be clicked:

| Piece | A click |
| --- | --- |
| `SHELL` / `✦ AGENT` | switches your typing between the shell and the agent |
| `^Space agent` / `^Space shell` | the same: a reminder of the key that does it |
| `explain`, `fix` (after a command fails) | has the agent explain the failure, or fix it |
| `watch` (while a command of yours runs) | has the agent follow the command and say how it went |
| `take the agent in` (while `ssh`, `sudo -s` and the like run) | carries the agent into the shell you reached |
| `◆` and the model's name | opens the list of models |
| `ask first`, `auto read-only`, `▶▶ autopilot` | steps to the next level of how much the agent may do |
| `2 agents working`, `? 1 agent asking` | opens Every agent at a glance |
| `? help` | shows every key |
| `≡ menu` | opens the menu of everything |

On a narrow terminal, the pieces on the right give way one by one, starting with the reminder of the agent key; `≡ menu` is always the last to stay. `status_bar = false` in the config file hides the bar altogether; <kbd>Ctrl+B</kbd> <kbd>Space</kbd> still opens the menu.

## The ≡ menu

Click `≡ menu` at the right-hand end of the status bar for a menu of every action, each with the key that does the same. Click an entry to do it.

{{shot menu | The ≡ menu open above the status bar, over an editor and a test run side by side, listing every action from New tab to Help, with the key for each at its right | The ≡ menu}}

It lists, in order: New tab, New tab on another machine…, Split right, Split down, Zoom pane, Close pane, Next tab, Previous tab, Rename tab, Find in scrollback, Select with the keyboard, Talk to the agent, Every agent at a glance, Explain the last command, Fix the last command, Watch the running command, Take the agent into this shell (ssh, sudo…), Choose a model, Choose a theme, Settings, Detach (keep everything running), and Help.

Some entries follow the moment: Zoom pane is there only in a tab with more than one pane, and reads Unzoom pane while one is zoomed; Close pane reads Close tab when the tab has only one; Talk to the agent reads Hide the agent panel while the panel is open.

From the keyboard, <kbd>Ctrl+B</kbd> <kbd>Space</kbd> opens the menu. <kbd>↑</kbd> and <kbd>↓</kbd> move through it and <kbd>Enter</kbd> picks the entry; or press an entry's own key (<kbd>c</kbd> for New tab, say) to pick it at once. Any other key, or a click outside the menu, closes it.

## Right-click menus

Right-click a tab or a pane for a menu of what can be done with it. Click an entry, or use <kbd>↑</kbd> <kbd>↓</kbd> and <kbd>Enter</kbd>; a click outside, or another right-click, closes it.

### On a tab

| Entry | Does |
| --- | --- |
| Rename | names the tab |
| Move left, Move right | moves the tab one place (offered where there is a place to move to) |
| New tab | opens a new tab |
| Close tab | closes the tab and its shells |

### On a pane

| Entry | Does |
| --- | --- |
| Open link, Copy link | when you right-click a link: opens it (web and mail links only), or copies where it goes |
| Split right, Split down | splits the pane in two |
| Find… | opens a search of the pane's scrollback |
| Zoom, Unzoom | lets the pane fill the tab, and back (with more than one pane) |
| Close pane | closes the pane; reads Close tab when it is the tab's only pane |

Right-clicking a pane the keyboard is not in moves the keyboard there first.

### Programs that use the mouse

Programs such as `vim` and `htop` take the mouse for themselves, so clicks, drags, the wheel and right-clicks in their pane go to them. Hold <kbd>Shift</kbd> to have Astrum take it instead: <kbd>Shift</kbd> and drag selects text, <kbd>Shift</kbd> and right-click gives Astrum's menu.

## How the keys work

### The prefix key

Most of Astrum's keys come after a prefix key, <kbd>Ctrl+B</kbd> unless you change it: press <kbd>Ctrl+B</kbd>, let go, then press the key. <kbd>Ctrl+B</kbd> <kbd>c</kbd> is a new tab. Keys are case-sensitive: <kbd>Ctrl+B</kbd> <kbd>d</kbd> detaches this window, <kbd>Ctrl+B</kbd> <kbd>D</kbd> every window.

After the prefix, the status bar shows `PREFIX` and what can follow it, each with its key. They can be clicked as well as typed, so the prefix also works as a way into the menu. They wait ten seconds for you, and moving the pointer starts the wait again. When they do not all fit, `≡ more` opens the full menu. <kbd>Esc</kbd>, or a key that is none of these, lets go without doing anything.

A few things to know:

- If you are still holding <kbd>Ctrl</kbd> when you press the second key, it works all the same: <kbd>Ctrl+B</kbd> <kbd>Ctrl+C</kbd> is <kbd>Ctrl+B</kbd> <kbd>c</kbd>, unless you have given that key with Ctrl something of its own.
- Pressing the prefix twice sends it to the program in the pane, for a program that uses <kbd>Ctrl+B</kbd> itself.
- On screen, keys are written as terminals write them: `^B` is <kbd>Ctrl+B</kbd>, `^B c` is <kbd>Ctrl+B</kbd> then <kbd>c</kbd>.

### Keys without the prefix

- <kbd>Ctrl+Space</kbd> switches your typing between the shell and the agent.
- <kbd>Shift+PageUp</kbd> and <kbd>Shift+PageDown</kbd> page through the scrollback (unless a full-screen program has the pane).
- In the agent panel, <kbd>Esc</kbd> stops the agent and <kbd>Shift+Tab</kbd> steps through how much it may do on its own. [The agent](agent.html) has the panel's other keys.
- Any key you give an action with `ctrl` or `alt` in it, as below.

### The help screen

Click `? help` on the status bar, or press <kbd>Ctrl+B</kbd> <kbd>?</kbd>, for one screen of the keys, grouped as tabs, the agent, and scrolling and copying. It shows the keys as you have set them. Any key or a click closes it.

## Changing the keys

Every key Astrum uses can be changed, in the config file. (The settings screen has the settings most people change; keys are not among them.) `astrum config` says where the file is, and `astrum config --init` writes a commented starter file there if there is none. A change takes effect in the running session as soon as you save the file: Astrum says "Settings reloaded", or says what is wrong with the file and changes nothing. [Settings](settings.html) has the rest of the file.

### The prefix and the agent key

Two settings at the top of the file:

```toml
prefix = "ctrl+a"         # in place of ctrl+b
agent_key = "f2"          # in place of ctrl+space
```

On macOS, <kbd>Ctrl+Space</kbd> switches input sources when several keyboard layouts are turned on, and some shell plugins use it. If it does something else for you, pick another `agent_key`, such as `"ctrl+backslash"` or `"f2"`.

### The [keys] table

Every other key is in a `[keys]` table, by the name of what it does:

```toml
[keys]
new_tab = ["c", "ctrl+t"]   # Ctrl+B c as before, and Ctrl+T directly
next_tab = "alt+right"
prev_tab = "alt+left"
split_right = "v"
detach_all = "none"
```

How it is read:

- A plain key is pressed after the prefix. A key with `ctrl` or `alt` in it works by itself, with no prefix. To put one of those after the prefix instead, write `prefix` in front: `"prefix ctrl+t"`.
- Naming an action replaces all the keys it had. A list gives it several, and `"none"` (or an empty list) leaves it with none.
- A key does one thing. Giving it to an action takes it from whatever had it, so two actions can swap keys. If the table gives the same key to two actions, Astrum says so.
- Letters are told apart by case: `"D"` is <kbd>Shift</kbd> and <kbd>d</kbd>, and so is `"shift+d"`. Shift can be written only where it makes another character (a capital letter, `shift+tab`). `shift+left`, `shift+f5` and `ctrl+shift+t` are refused, because most terminals do not say whether Shift was held with those keys.
- The action names are the ones in the list below and in `astrum keys`. Case does not matter, and `-` works in place of `_`.
- A name or a key that Astrum cannot read is reported, and the rest of the table still applies. `astrum keys` reports it too.

The names of keys:

| Write | For |
| --- | --- |
| a single character: `c`, `D`, `,`, `/`, `[` | that key (a capital is Shift and the letter) |
| `space`, `enter`, `tab`, `shift+tab`, `esc`, `backspace`, `delete`, `insert` | those keys |
| `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown` | those keys |
| `f1` to `f24` | the function keys |
| `backslash`, `plus`, `minus` | the `\`, `+` and `-` keys |
| `ctrl+` or `alt+` in front (`option+` and `meta+` mean Alt) | the key with Ctrl or Alt held |

In macOS terminal apps, the Option key reaches programs as Alt only when the terminal is set to send it that way; check your terminal's settings if an `alt+` key does nothing.

The hint line after the prefix, the `≡ menu` and the help screen all show the keys as you have set them. When a new release brings a new key that you had already given to something else, it stays yours, and the new action can still be reached from the menu.

### Seeing the keys you have

```bash
astrum keys
```

lists every action with the keys it has now and what it does, the same list as below but with your changes in it:

```text
ACTION           KEYS                   WHAT IT DOES
new_tab          c, t                   open a new tab
next_tab         n, tab                 go to the next tab
prev_tab         p, shift+tab           go to the previous tab
```

## Every key

Every action, what it does, and the keys it has before you change any. A plain key is pressed after the prefix, <kbd>Ctrl+B</kbd>.

{{keys}}
