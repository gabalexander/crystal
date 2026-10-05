# Installing crystal

<sub>[← README](../README.md#documentation)</sub>

How to install crystal, from a release, a package manager or its source, how to keep it up to date, and how to
have your shell complete its commands.

- [The install script](#the-install-script)
- [Homebrew and Nix](#homebrew-and-nix)
- [From source](#from-source)
- [Updating](#updating)
- [Shell completions](#shell-completions)

## The install script

On macOS or Linux, on Apple silicon, Intel or ARM:

```sh
curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh
```

It downloads the latest release, checks it against its checksum, and puts `crystal` in `~/.local/bin`.
`CRYSTAL_VERSION=0.1.0` picks a release, and `CRYSTAL_INSTALL_DIR` another directory. With Claude Code on the
machine, it also installs [the skill](driving.md#a-skill-for-claude-code) that teaches Claude Code to drive crystal;
`CRYSTAL_NO_SKILL=1` leaves it out. The Linux builds are static, so they run on any distribution.

## Homebrew and Nix

```sh
brew install gabalexander/crystal/crystal     # from a tap of crystal's own
nix run github:gabalexander/crystal           # or build it with Nix
nix profile install github:gabalexander/crystal
```

Homebrew's own `crystal` is the Crystal programming language, so crystal is installed from its tap by the tap's
name, and `brew upgrade` keeps it up to date. The tap's formula installs the release built for your machine,
checked against its checksum, with completions for bash, zsh and fish; [`packaging/homebrew`](../packaging/homebrew)
holds it, and `formula.sh` there fills in a release's version and checksums for the tap. The flake builds crystal
from source with the nixpkgs it pins, completions too, and `nix develop` gives a shell with the toolchain. Neither
restarts a daemon that's running, as the install script does: after an upgrade, run `crystal restart-server`, and
`crystal skill --install` for Claude Code. `crystal update` leaves a crystal either installed alone, and says what
updates it.

## From source

Build it from source, with Rust 1.88 or newer:

```sh
git clone https://github.com/gabalexander/crystal
cd crystal
make install    # into ~/.local/bin; make install PREFIX=/usr/local for /usr/local/bin
```

or `cargo install --git https://github.com/gabalexander/crystal`.

There's one binary: crystal starts its daemon in the background, from the same binary, the first time it's
needed. A daemon that's already running goes on running the old crystal until it's restarted, so after
upgrading, run `crystal restart-server` (the install script and `make install` do it for you). It hands the
daemon over to the new crystal without stopping anything: programs go on running, background tasks go on
working, and every screen keeps what it showed, history and all. A TUI or `crystal attach` left open picks its
session up again by itself. `crystal restart-server --cold` stops the daemon and starts it again instead, as a
daemon from before handovers is restarted: running sessions come back, Claude Code in its conversation, other
programs from the start. A crystal that finds a daemon of another version says so, rather than misunderstanding
it; a TUI left open on an older crystal asks you to start it again.

## Updating

```sh
crystal update            # install the latest release, if it's newer
crystal update --check    # only say whether a newer one is out
crystal update 0.2.0      # install that release instead, even an older one
crystal update --notes    # print what this crystal changed, its release notes
```

`crystal update` does what the install script does, in place: it downloads the release for this machine,
checks it against its checksum, runs it once to see that it runs here, and only then puts it in place of the
crystal you ran. Then the new crystal restarts every daemon that's running, each server's, handed over as
`crystal restart-server` does, so your sessions carry on, and brings the skill up to date where Claude Code is
(`CRYSTAL_NO_SKILL=1` leaves it). A crystal installed by Homebrew, mise, Nix or cargo, or one built from
source, is left alone, and the command says what updates it instead. `CRYSTAL_RELEASES` names another place to
download releases from, as it does for the install script.

Once a day, as it opens, the TUI looks for a newer release and says so on its bottom line when there is one.
`check = false` under `[update]` in the [settings](configuration.md) turns that off.

The first time the TUI opens on a new crystal, it shows what's new in it: the release's notes, as GitHub has
them, scrolled with the arrows and put away with any other key, and never shown again. `crystal update` keeps
them as it installs the release, so the TUI has them at once; updated some other way, the TUI asks for them
itself, unless `check = false` says not to. `release-notes` in the [command list](keys.md#the-command-list) (`:`)
shows them again, and `crystal update --notes` prints them, or another release's, like
`crystal update --notes 0.2.0`. A mirror named by `CRYSTAL_RELEASES` keeps each release's notes as
`release-notes.md` among its files.

## Shell completions

`crystal completions <shell>` prints the script that completes crystal's commands and options in bash, zsh,
fish, elvish or PowerShell. In bash, zsh and fish, a command that takes a session's name, like `attach`, `send`
or `kill`, completes the names of the sessions running now; with no daemon running there are none, and none is
started.

```sh
# bash: in ~/.bashrc
eval "$(crystal completions bash)"
# zsh: in a directory on your $fpath, then start a new shell (compinit must run in ~/.zshrc)
crystal completions zsh > ~/.zfunc/_crystal
# fish
crystal completions fish > ~/.config/fish/completions/crystal.fish
# elvish: in ~/.config/elvish/rc.elv
eval (crystal completions elvish | slurp)
# PowerShell: in your $PROFILE
crystal completions powershell | Out-String | Invoke-Expression
```
