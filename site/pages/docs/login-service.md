---
title: The login service
description: Have the system start a session when you log in and start it again if it stops, with launchd on macOS or a systemd user service on Linux.
---

## What it is for

Normally a session's server is started by the first `astrum` that asks for it, and runs until it is stopped. Running it as a login service hands it to the system's own service manager instead:

- it is started when you log in, before you open a terminal;
- it is started again if it stops, for whatever reason, so `astrum kill` becomes a restart and the session's tabs come back;
- on Linux, with lingering turned on, it keeps running while you are logged out: shells, running programs and all.

It is optional. Nothing else about using Astrum changes: you attach with `astrum` as before, and detach as before.

| | macOS | Linux |
| --- | --- | --- |
| Service manager | launchd, as a launch agent | systemd, as a user service |
| The file it writes | `~/Library/LaunchAgents/io.github.robert-mcdermott.astrum.<session>.plist` | `~/.config/systemd/user/astrum-<session>.service` |
| Started | when you log in | when your user's systemd starts: at login, or at boot with lingering on |
| Through a logout | no: macOS ends every process of yours; the session comes back restored at the next login | yes, with lingering on |
| The server's own log | `~/.local/share/astrum/service-<session>.log` | the user journal |

## Setting it up

```bash
astrum service install
```

sets up the service for the `default` session and starts it. Add `-s name` for another session; each session has a service of its own.

On Linux it says something like:

```text
Installed /home/me/.config/systemd/user/astrum-default.service
Session `default` now starts when you log in, and starts again if it stops.
To keep it running while you are logged out as well, turn on lingering:
    loginctl enable-linger
```

On macOS it says the session now starts when you log in, and that logging out of macOS still ends it.

If the session is already running, started the ordinary way, it is left as it is. The service takes over the next time the session is stopped (`astrum kill`), and its tabs come back.

Running `astrum service install` again when nothing has changed says the service is already installed and up to date, and touches nothing. When something has changed (you moved Astrum, say), the file is written again and the session restarted to take the change, and its tabs come back. Run it from a terminal outside that session: a restart would cut off a command running inside it, so Astrum refuses there and says why.

## Seeing what it would write

```bash
astrum service install --print
```

prints where the file would go and everything that would be in it, and does nothing else. Add `-s name` to see another session's.

## What it changes

### On macOS

- It writes a launch agent, `~/Library/LaunchAgents/io.github.robert-mcdermott.astrum.<session>.plist`, and loads it into launchd for your login with `launchctl bootstrap`.
- The agent runs Astrum's session server for that session, is started when it is loaded (at each login), and is kept alive: if the server stops, launchd starts it again a moment later.
- What the server prints goes to `~/.local/share/astrum/service-<session>.log`.
- Logging out of macOS ends every process of yours, this one included. The session comes back restored at the next login, as it would after a restart.

### On Linux

- It writes a systemd user service, `~/.config/systemd/user/astrum-<session>.service` (under `$XDG_CONFIG_HOME` if you have set that), then runs `systemctl --user daemon-reload` and `systemctl --user enable --now` for it.
- The service runs Astrum's session server for that session, with `Restart=always`: if the server stops, systemd starts it again a second later. It is wanted by your user's `default.target`, so it starts whenever your user's systemd does.
- What the server prints goes to the user journal: `journalctl --user -u astrum-default.service` shows it.

### Either way

- The service runs the `astrum` you ran `install` with, by its full path. If you move Astrum to another place, run `astrum service install` again with the one in its new place, and the service is changed to match. Upgrading in place needs nothing: the next start (after `astrum kill`, say) runs the new version.
- Nothing outside your home folder is touched, and no `sudo` is needed.

## Staying up through a logout on Linux

Without lingering, systemd stops a user's services at their last logout, and the session stops with them; its tabs come back at the next login. With lingering on, your user's systemd keeps running while you are logged out (and starts when the machine does), so the session stays up:

```bash
loginctl enable-linger
```

`astrum service install` and `astrum service status` both say when lingering is off. `loginctl disable-linger` turns it off again.

## The environment it has

A server started by the service manager does not come from one of your shells, so it has a bare environment. Astrum hands it only what a session needs to find its files and start your shell, as they are in the terminal you run `install` from: `SHELL`, `LANG`, `LC_ALL`, `LC_CTYPE`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `ASTRUM_CONFIG_DIR`, `ASTRUM_DATA_DIR`, `ASTRUM_RUNTIME_DIR` and `ASTRUM_KEYCHAIN_FILE`. If none of the language settings is set, it is given a UTF-8 one, so that shells do not garble anything beyond plain ASCII.

What that means:

- API keys set in your shell's startup files do not reach it. Run `astrum reload` from a shell that has them to pass them in, or store them with `/key` in the agent panel, which needs nothing in the environment at all. See [Providers](providers.html).
- A [tab on another machine](other-machines.html) runs `ssh` with the server's environment, so it does not see your SSH key agent (`SSH_AUTH_SOCK`). Whatever `ssh` then asks, such as a key's passphrase, is asked in the pane.
- After changing any of the variables above, run `astrum service install` again to write them into the service.

## Checking on it

```bash
astrum service status
```

says whether the service for the session is installed, and whether it is running:

```text
Service for session `default`: running
/home/me/.config/systemd/user/astrum-default.service
```

or `installed, not running`, or that no service is installed for the session. On Linux it also says when lingering is off. `astrum ls` lists the session as usual once it is running.

The system's own tools see it too: `launchctl print gui/$(id -u)/io.github.robert-mcdermott.astrum.default` on macOS, and `systemctl --user status astrum-default.service` on Linux.

## Removing it

```bash
astrum service uninstall
```

removes the service's file, tells the service manager it is gone, and stops the session. Its tabs are kept, and come back the next time you start it with `astrum`, the ordinary way. Add `-s name` for another session's service.

Run it from a terminal outside that session: stopping the session would cut the command off half-way, so Astrum refuses to run it inside and says why. The file goes before the session is stopped, so even a command that was cut off cannot leave the service to come back at the next login.

## Things to know

- With the service installed, `astrum kill` restarts the session, and `astrum kill --forget` restarts it empty. To stop it and keep it stopped, remove the service.
- Closing a session's last shell ends that session for good, as always; the service manager then starts a fresh, empty one, which waits for you to attach.
- A session under the service is the same session as before: [what comes back](sessions.html#what-comes-back) after a stop or a reboot is decided by `restore`, just the same.
