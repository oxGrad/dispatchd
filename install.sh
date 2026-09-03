#!/bin/sh
# dispatchd installer - https://github.com/oxGrad/dispatchd
#
#   curl -fsSL https://dispatchd.graditya.com | sh
#
# Downloads the right prebuilt binary for this machine from the latest
# (or $DISPATCHD_VERSION-pinned) GitHub Release, verifies its checksum,
# and installs it to $INSTALL_DIR (default /usr/local/bin, so `sudo
# dispatchd ...` and the systemd unit can both find it - run the whole
# one-liner under sudo, or set INSTALL_DIR to a path you own for a local
# install). Never compiles anything, never touches config, never runs
# `dispatchd init` for you - see the printed next steps at the end.
set -eu

REPO="oxGrad/dispatchd"
BIN_NAME="dispatchd"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

say() {
    printf '%s\n' "$1"
}

err() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Linux)
            case "$arch" in
                x86_64) echo "x86_64-unknown-linux-musl" ;;
                aarch64 | arm64) echo "aarch64-unknown-linux-musl" ;;
                armv7l) echo "armv7-unknown-linux-musleabihf" ;;
                *) err "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                arm64) echo "aarch64-apple-darwin" ;;
                x86_64)
                    err "Intel Mac isn't a supported target - see https://github.com/$REPO for the supported platform list"
                    ;;
                *) err "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        *)
            err "unsupported OS: $os (supported: Linux x86_64/aarch64/armv7, macOS Apple Silicon)"
            ;;
    esac
}

resolve_version() {
    if [ -n "${DISPATCHD_VERSION:-}" ]; then
        echo "$DISPATCHD_VERSION"
        return
    fi
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | grep '"tag_name"' \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
    [ -n "$version" ] || err "couldn't resolve the latest release version from the GitHub API"
    echo "$version"
}

checksum_verify() {
    # $1 = file to verify, $2 = checksums file (sha256sum -style: "<hash>  <name>")
    file="$1"
    sums="$2"
    name=$(basename "$file")
    line=$(grep "  ${name}\$" "$sums" || true)
    [ -n "$line" ] || err "no checksum entry found for $name in SHA256SUMS"

    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$(dirname "$file")" && echo "$line" | sha256sum -c -) \
            || err "checksum verification failed for $name"
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$(dirname "$file")" && echo "$line" | shasum -a 256 -c -) \
            || err "checksum verification failed for $name"
    else
        err "neither sha256sum nor shasum is available - refusing to install unverified"
    fi
}

main() {
    command -v curl >/dev/null 2>&1 || err "curl is required but not found"
    command -v tar >/dev/null 2>&1 || err "tar is required but not found"

    target=$(detect_target)
    version=$(resolve_version)
    say "Installing dispatchd $version for $target..."

    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT INT TERM

    asset="${BIN_NAME}-${target}.tar.gz"
    base_url="https://github.com/$REPO/releases/download/$version"

    curl -fsSL -o "$tmp_dir/$asset" "$base_url/$asset" \
        || err "failed to download $asset for $version (does that release have this platform's binary?)"
    curl -fsSL -o "$tmp_dir/SHA256SUMS" "$base_url/SHA256SUMS" \
        || err "failed to download SHA256SUMS for $version"

    checksum_verify "$tmp_dir/$asset" "$tmp_dir/SHA256SUMS"

    tar -xzf "$tmp_dir/$asset" -C "$tmp_dir" "$BIN_NAME"

    mkdir -p "$INSTALL_DIR" 2>/dev/null || true
    if [ ! -d "$INSTALL_DIR" ] || [ ! -w "$INSTALL_DIR" ]; then
        err "$INSTALL_DIR isn't writable. Re-run the whole command under sudo, e.g.
       curl -fsSL https://dispatchd.graditya.com | sudo sh
   or set INSTALL_DIR to a path you own for a local (non-service) install, e.g.
       curl -fsSL https://dispatchd.graditya.com | INSTALL_DIR=\"\$HOME/.local/bin\" sh"
    fi
    # Use `install` when available: it sets the mode and, crucially, lets the
    # destination directory's default SELinux context apply to the new file.
    # A plain `mv` preserves the label from $tmp_dir (under /tmp, so
    # `user_tmp_t`), which systemd then refuses to exec as `User=` on an
    # SELinux-enforcing host ("Unable to locate executable: Permission
    # denied"). Fall back to mv+chmod where `install` is missing.
    if command -v install >/dev/null 2>&1; then
        install -m 0755 "$tmp_dir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME" \
            || err "failed to install $BIN_NAME to $INSTALL_DIR"
    else
        mv "$tmp_dir/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
        chmod 0755 "$INSTALL_DIR/$BIN_NAME"
    fi

    # Belt-and-braces: if the box has SELinux tooling, force the label to the
    # directory's policy default regardless of how the file got here.
    if command -v restorecon >/dev/null 2>&1; then
        restorecon "$INSTALL_DIR/$BIN_NAME" 2>/dev/null || true
    fi

    say "Installed $BIN_NAME $version to $INSTALL_DIR/$BIN_NAME"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            say ""
            say "warning: $INSTALL_DIR isn't on your \$PATH. Add this to your shell rc:"
            say "  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac

    say ""
    say "Next: run \`dispatchd init\` (as your normal user, no sudo - it writes"
    say "config.toml/members.toml templates under your \$HOME), then see"
    say "docs/discord-setup.md for wiring up the Discord bot:"
    say "  https://github.com/$REPO/blob/main/docs/discord-setup.md"
}

main "$@"
