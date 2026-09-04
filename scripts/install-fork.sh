#!/bin/sh
set -eu

BIN="herdr"
REPO="${HERDR_FORK_REPO:-jacksluong/herdr}"
RELEASE="${HERDR_FORK_RELEASE:-latest}"
INSTALL_DIR="${HERDR_INSTALL_DIR:-$HOME/.local/bin}"

main() {
    echo ""
    echo "  herdr fork installer (${REPO})"
    echo ""

    OS="$(uname -s)"
    case "$OS" in
        Darwin) os="macos" ;;
        Linux)  os="linux" ;;
        *)      err "unsupported OS: $OS" ;;
    esac

    ARCH="$(uname -m)"
    case "$ARCH" in
        arm64|aarch64) arch="aarch64" ;;
        x86_64|amd64)  arch="x86_64" ;;
        *)             err "unsupported architecture: $ARCH" ;;
    esac

    ASSET="${BIN}-${os}-${arch}"
    log "detected ${os}/${arch}"

    need curl

    if [ "$RELEASE" = "latest" ]; then
        URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
    else
        URL="https://github.com/${REPO}/releases/download/${RELEASE}/${ASSET}"
    fi

    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT

    log "downloading ${ASSET}..."
    if ! curl -fsSL --retry 3 --connect-timeout 10 --max-time 300 "$URL" -o "${TMP}/${BIN}"; then
        err "download failed from ${URL} (the fork release may not build this target)"
    fi

    chmod +x "${TMP}/${BIN}"
    xattr -d com.apple.quarantine "${TMP}/${BIN}" 2>/dev/null || true

    mkdir -p "$INSTALL_DIR"
    # Rename rather than copy so a running herdr keeps its own inode.
    if ! mv -f "${TMP}/${BIN}" "${INSTALL_DIR}/${BIN}" 2>/dev/null; then
        cat "${TMP}/${BIN}" > "${INSTALL_DIR}/${BIN}"
        chmod +x "${INSTALL_DIR}/${BIN}"
    fi

    log "installed to ${INSTALL_DIR}/${BIN}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            warn "${INSTALL_DIR} is not in your PATH"
            echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
            echo ""
            ;;
    esac

    log "$("${INSTALL_DIR}/${BIN}" --version 2>/dev/null || echo "installed")"
    echo ""
}

log()  { printf '  \033[32m>\033[0m %s\n' "$1"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; }
err()  { printf '  \033[31mx\033[0m %s\n' "$1" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || err "requires '$1'"
}

main "$@"
