#!/bin/sh
# Prints crystal's Homebrew formula for a release: crystal.rb, beside this
# script, with the release's version, where its archives are and each one's
# checksum, read from the release itself, filled in.
#
#   packaging/homebrew/formula.sh 0.4.0 > Formula/crystal.rb
#
# CRYSTAL_RELEASES names another place to read releases from, as it does for
# install.sh.

set -eu

releases="${CRYSTAL_RELEASES:-https://github.com/gabalexander/crystal/releases}"
targets="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl"

fail() {
    printf 'formula.sh: %s\n' "$*" >&2
    exit 1
}

main() {
    [ $# -eq 1 ] || fail "say which release, like: formula.sh 0.4.0"
    version="${1#v}"
    case "$version" in
        *[!0-9.]* | "") fail "$1 isn't a release's version, like 0.4.0" ;;
    esac
    template="$(dirname "$0")/crystal.rb"
    script="s|@VERSION@|$version|g; s|@RELEASES@|$releases|g"
    for target in $targets; do
        url="$releases/download/v$version/crystal-$version-$target.tar.gz.sha256"
        line=$(curl -fsSL "$url") || fail "couldn't download $url"
        sum=${line%% *}
        case "$sum" in
            *[!0-9a-f]*) fail "$url doesn't hold a checksum" ;;
        esac
        [ ${#sum} -eq 64 ] || fail "$url doesn't hold a checksum"
        script="$script; s|@SHA256_$target@|$sum|g"
    done
    sed -e "$script" "$template"
}

main "$@"
