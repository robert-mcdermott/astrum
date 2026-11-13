---
title: Install
description: Install Astrum with one line, check what you download, build it from source, upgrade it, or remove it.
---

## The one-line install

Astrum is one file. To install the latest release as `~/.local/bin/astrum`, run this in a terminal:

{{install}}

Then start it:

```bash
astrum
```

No `sudo` is needed, and nothing else has to be installed first: Astrum does not depend on any other program when it runs. (A tab on another machine is reached with the `ssh` you already have.)

## What the installer does

In order, it:

1. Works out which file this machine needs, from what `uname` says about the system and the processor. A shell running under Rosetta on an Apple silicon Mac still gets the Apple silicon build.
2. Downloads that file and the release's `SHA256SUMS` from GitHub, over HTTPS only.
3. Checks the file's SHA-256 checksum against its line in `SHA256SUMS`, and stops if they differ.
4. Puts the file in a temporary place beside where it is going, runs it to see that it is Astrum (and, if you asked for a particular version, that version), and only then moves it into place. A download that went wrong never replaces a copy that works.
5. Installing to the default folder, it also puts the manual page in `~/.local/share/man/man1`, where `man astrum` finds it.

It does not use `sudo`, change your `PATH`, edit your shell's startup files or start anything. When it is done it prints the command to start Astrum, and a reminder that a session already running keeps the version it was started with (see [Upgrading](#upgrading)).

It needs `sh`, `curl`, `uname`, `awk`, `grep`, `mktemp`, and `sha256sum` or `shasum`, which nearly every macOS and Linux system has. If one is missing, it says which.

### If ~/.local/bin is not on your PATH

The installer says so, and prints the full path to run instead. To be able to type just `astrum`, add the folder to your `PATH` in your shell's startup file (`~/.zshrc`, `~/.bashrc`, or `~/.config/fish/config.fish`). For zsh and bash:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Open a new terminal afterwards for it to take effect.

## Supported systems

| System | The release file |
| --- | --- |
| macOS on Apple silicon | `astrum-darwin-arm64` |
| Linux on x86-64 | `astrum-linux-amd64` |
| Linux on ARM64 | `astrum-linux-arm64` |

The Linux builds are fully static: they need nothing from the system, and run on old and new distributions alike. Release builds exist from version 0.4.0 on.

There is no release build for an Intel Mac, or for other systems; the installer says so and stops. An Intel Mac can [build from source](#building-from-source).

### Windows

There is no Windows build: Astrum is made for Unix-like systems, from how it keeps sessions running to the shells it works with. On Windows 10 and 11 it can run inside WSL2, the Windows Subsystem for Linux, where it is on Linux like anywhere else:

1. In PowerShell, install a Linux distribution: `wsl --install` (Ubuntu, unless you name another).
2. Open that distribution in Windows Terminal, and use the [one-line install](#the-one-line-install) there.
3. Run `astrum`. Your shells are the distribution's, and so are the files the agent works with; your Windows drives are under `/mnt/c` and so on.

> Astrum under WSL2 has not been tried yet. What we expect to work differently there:
>
> - **Copying** is always sent to the terminal (as OSC 52), which Windows Terminal is meant to put on the Windows clipboard. If it does not, `ASTRUM_CLIPBOARD_TOOL=clip.exe` set before starting `astrum` hands the text to Windows' own `clip.exe` as well, which may garble letters outside plain ASCII.
> - **Links** are opened with `xdg-open`, which in WSL often has no browser to open. Set `ASTRUM_OPEN_TOOL=wslview` (from the `wslu` package) or `ASTRUM_OPEN_TOOL=explorer.exe` to open them in your Windows browser.
> - **Sessions** run as long as the distribution does. A restart of Windows, or `wsl --shutdown`, stops them; their tabs come back the next time, as after any restart (see [Sessions that come back](sessions.html)).
> - **The login service** needs systemd, which a distribution has only when its `/etc/wsl.conf` says `systemd=true` under `[boot]`.
>
> If you try it, an [issue](https://github.com/robert-mcdermott/astrum/issues) saying what worked and what did not is welcome.

## Reading the installer first

The installer is a short shell script. To read it before running it, download it, look through it, then run it:

```bash
curl --proto '=https' --tlsv1.2 -fsSLo install-astrum.sh https://raw.githubusercontent.com/robert-mcdermott/astrum/main/install.sh
```

```bash
less install-astrum.sh
```

```bash
sh install-astrum.sh
```

`sh install-astrum.sh --help` lists its options.

## A particular version, or another folder

`--version` installs a particular release, and `--install-dir` puts it in another folder (an absolute path):

```bash
sh install-astrum.sh --version v0.4.0
```

```bash
sh install-astrum.sh --install-dir "$HOME/bin"
```

The version can be written `v0.4.0` or `0.4.0`. With a version given, the installer also checks that the program it downloaded reports exactly that version.

For the one-line form, the environment variables `ASTRUM_VERSION` and `ASTRUM_INSTALL_DIR` do the same, set for the `sh` at the end of the line:

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/robert-mcdermott/astrum/main/install.sh | ASTRUM_VERSION=v0.4.0 sh
```

`ASTRUM_DOWNLOAD_BASE` takes the release files from somewhere other than GitHub: an `https://` address of a mirror, or `file:///` and a folder on this machine that holds the release file and its `SHA256SUMS` (files you downloaded by hand, say). The checksum is checked just the same. The manual page is only placed when installing to the default folder.

## Checking a download by hand

To install without the script, take the file for your machine and `SHA256SUMS` from the [releases page](https://github.com/robert-mcdermott/astrum/releases). With `curl`, for the latest release on a Mac:

```bash
curl -fsSLO https://github.com/robert-mcdermott/astrum/releases/latest/download/astrum-darwin-arm64
```

```bash
curl -fsSLO https://github.com/robert-mcdermott/astrum/releases/latest/download/SHA256SUMS
```

Work out the file's checksum and compare it with the line for the same file in `SHA256SUMS`:

```bash
shasum -a 256 astrum-darwin-arm64
```

```bash
grep astrum-darwin-arm64 SHA256SUMS
```

On Linux, `sha256sum` does the same, and can check the file against the list in one step:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

If the two match, make the file executable and put it on your `PATH` as `astrum`:

```bash
chmod +x astrum-darwin-arm64 && mv astrum-darwin-arm64 ~/.local/bin/astrum
```

## Completions and the manual page

`astrum completions bash` (or `zsh`, or `fish`) prints a completion script, which completes Astrum's commands, their options and the names of your sessions. The first lines of each script say where it goes:

| Shell | Where |
| --- | --- |
| bash | `astrum completions bash > ~/.local/share/bash-completion/completions/astrum` |
| zsh | `astrum completions zsh > ~/.zfunc/_astrum`, with `fpath=(~/.zfunc $fpath)` in `~/.zshrc` before `compinit` runs |
| fish | `astrum completions fish > ~/.config/fish/completions/astrum.fish` |

`astrum man` prints the manual page. The installer already puts it where `man astrum` finds it when it installs to the default folder; otherwise, write it to a `man1` folder that `man` searches, such as:

```bash
mkdir -p ~/.local/share/man/man1 && astrum man > ~/.local/share/man/man1/astrum.1
```

The manual page is also [on this site](manual.html).

## Building from source

Building needs a Rust toolchain, 1.88 or newer, and a C compiler. Get the source and install from it:

```bash
git clone https://github.com/robert-mcdermott/astrum
```

```bash
cd astrum && cargo install --path . --locked
```

That puts `astrum` in `~/.cargo/bin`. `--locked` builds with the exact dependency versions recorded in `Cargo.lock`, the ones Astrum is tested with. To build without installing, the binary is then `target/release/astrum`:

```bash
cargo build --release --locked
```

If `cargo` or `astrum` is "not found" afterwards, `~/.cargo/bin` is not on your `PATH`; add `. "$HOME/.cargo/env"` to your shell's startup file.

## Upgrading

Run the one-line install again: it installs the newest release over the old one, with the same checks.

A session that is already running keeps running the version it was started with. To move it to the new one, stop it and start it again; its tabs come back:

```bash
astrum kill
```

```bash
astrum
```

Do that for each session you have (`astrum ls` lists them, and `astrum kill --all` stops them all). In the meantime the new `astrum` can still attach to, list and stop a session the older one started. A session run as a [login service](login-service.html) needs only `astrum kill`: the service manager starts it again from the same file, which is now the new version.

[The changelog](changelog.html) says what changed in each release, with notes on upgrading from the one before.

If you once installed Astrum with `cargo install`, that copy is in `~/.cargo/bin`, and whichever of the two comes first on your `PATH` is the one that runs. Remove the one you no longer want (`cargo uninstall astrum` for the other), or go on upgrading it the way you installed it.

## Removing Astrum

1. If you stored API keys with `/key`, remove each from the system's password store with `/key name remove` in the agent panel while Astrum is still there.
2. Work from a terminal outside Astrum for the rest. If you set up the [login service](login-service.html), remove it, or it starts the session again: `astrum service uninstall` (and `astrum service uninstall -s name` for each other session that has one).
3. Stop your sessions: `astrum kill --all`. Add `--forget` to discard their saved tabs as well.
4. Delete the program: `~/.local/bin/astrum`, or wherever you installed it.

What is left are your files. Delete them too if you want nothing to remain:

| What | Where |
| --- | --- |
| Settings, your own rules and skills | `~/.config/astrum` |
| Saved sessions, conversations, builds kept for other machines | `~/.local/share/astrum` |
| The manual page | `~/.local/share/man/man1/astrum.1` |
| Completion scripts | wherever you put them |
| Session sockets and server logs | `/tmp/astrum-<uid>` (your user id) |

A machine you opened a [tab on](other-machines.html) has its own copy in `~/.local/bin/astrum` there, if Astrum put it there.
