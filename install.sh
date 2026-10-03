#!/bin/sh
# Installs crystal from its releases on GitHub:
#
#   curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh
#
# All of these are optional:
#
#   CRYSTAL_VERSION      the release to install, like 0.1.0, rather than the latest
#   CRYSTAL_INSTALL_DIR  where to put crystal, rather than ~/.local/bin
#   CRYSTAL_RELEASES     where to download releases from, for a mirror or a test
#   CRYSTAL_DRY_RUN=1    say what would be installed, and stop there
#   CRYSTAL_NO_SKILL=1   leave out the skill that teaches Claude Code to drive crystal
#
# Everything happens in main, called on the last line, so a download cut off
# halfway never runs half a script.

set -eu

repo="gabalexander/crystal"
releases="${CRYSTAL_RELEASES:-https://github.com/$repo/releases}"
install_dir="${CRYSTAL_INSTALL_DIR:-$HOME/.local/bin}"

say() {
    printf 'crystal: %s\n' "$*"
}

fail() {
    say "$*" >&2
    exit 1
}

# The release built for this machine, like aarch64-apple-darwin.
detect_target() {
    case "$(uname -s)" in
        Darwin) os="apple-darwin" ;;
        Linux) os="unknown-linux-musl" ;;
        *) fail "there's no release for $(uname -s); build crystal from source instead" ;;
    esac
    case "$(uname -m)" in
        arm64 | aarch64) arch="aarch64" ;;
        x86_64 | amd64) arch="x86_64" ;;
        *) fail "there's no release for $(uname -m); build crystal from source instead" ;;
    esac
    # A shell running under Rosetta on an Apple silicon Mac says x86_64, but
    # the build to have is the one for the machine itself.
    if [ "$os" = "apple-darwin" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = "1" ]; then
        arch="aarch64"
    fi
    echo "$arch-$os"
}

# The version to install: CRYSTAL_VERSION, or else the latest release, which
# is where GitHub's /releases/latest sends you.
resolve_version() {
    if [ -n "${CRYSTAL_VERSION:-}" ]; then
        echo "${CRYSTAL_VERSION#v}"
        return
    fi
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest") ||
        fail "couldn't reach $releases"
    tag="${url##*/}"
    case "$tag" in
        v*) echo "${tag#v}" ;;
        *) fail "there's no release at $releases yet" ;;
    esac
}

# Whether Claude Code is on this machine: its command, or its config.
has_claude_code() {
    command -v claude > /dev/null 2>&1 || [ -d "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" ]
}

# The SHA-256 checksum of a file, with whichever tool this machine has.
sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

main() {
    command -v curl > /dev/null 2>&1 || fail "curl is needed to download crystal"
    target=$(detect_target)
    version=$(resolve_version)
    archive="crystal-$version-$target"
    url="$releases/download/v$version/$archive.tar.gz"

    if [ "${CRYSTAL_DRY_RUN:-}" = "1" ]; then
        say "would install crystal $version for $target"
        say "from $url"
        say "into $install_dir"
        return
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    say "downloading crystal $version for $target"
    curl -fsSL -o "$tmp/$archive.tar.gz" "$url" || fail "couldn't download $url"
    curl -fsSL -o "$tmp/$archive.tar.gz.sha256" "$url.sha256" ||
        fail "couldn't download $url.sha256"

    expected=$(cut -d ' ' -f 1 < "$tmp/$archive.tar.gz.sha256")
    actual=$(sha256 "$tmp/$archive.tar.gz")
    [ "$expected" = "$actual" ] || fail "the download doesn't match its checksum; try again"

    tar -xzf "$tmp/$archive.tar.gz" -C "$tmp"
    mkdir -p "$install_dir"
    # Removed first rather than written over: macOS kills a program whose
    # signed file changes under it, and a running daemon is one.
    rm -f "$install_dir/crystal"
    cp "$tmp/$archive/crystal" "$install_dir/crystal"
    chmod 755 "$install_dir/crystal"
    say "installed $install_dir/crystal"

    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *) say "$install_dir isn't on your PATH yet: add it to run crystal" ;;
    esac

    # A daemon left running goes on running the crystal it was started from
    # until it's handed over to this one. Its running sessions carry on.
    "$install_dir/crystal" restart-server ||
        say "couldn't restart the daemon: run crystal kill-server, then crystal"

    # Claude Code learns to drive crystal from a skill: put where Claude Code
    # looks for it, or brought up to date. A skill you've changed is kept.
    if [ "${CRYSTAL_NO_SKILL:-}" != "1" ] && has_claude_code; then
        "$install_dir/crystal" skill --install ||
            say "kept the skill you changed: crystal skill --install --force replaces it"
    fi
}

main "$@"
