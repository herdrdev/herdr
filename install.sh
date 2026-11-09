#!/bin/sh
# Shepherd installer: downloads the prebuilt binary for your system from a GitHub release,
# checks its SHA-256 and installs it into ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/Rivaldito/shepherd/main/install.sh | sh
#
# Variables:
#   SHEPHERD_VERSION      release tag to install (default: the newest release, including betas), e.g. v0.1.0-beta
#   SHEPHERD_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   SHEPHERD_RELEASE_BASE base URL holding the assets, for mirrors and tests (skips the GitHub lookup)
set -eu

REPO="Rivaldito/shepherd"
BIN="shepherd"
INSTALL_DIR="${SHEPHERD_INSTALL_DIR:-$HOME/.local/bin}"

main() {
    echo ""
    echo "  shepherd installer"
    echo "  https://shepherdr.rivaldito.com"
    echo ""

    case "$(uname -s)" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)      err "unsupported OS: $(uname -s). Shepherd has binaries for Linux and macOS." ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             err "unsupported architecture: $(uname -m)" ;;
    esac
    ASSET="${BIN}-${os}-${arch}"
    log "detected ${os}/${arch}"

    need curl
    need awk

    if [ -n "${SHEPHERD_RELEASE_BASE:-}" ]; then
        BASE="${SHEPHERD_RELEASE_BASE%/}"
        TAG="custom"
    else
        TAG="${SHEPHERD_VERSION:-}"
        if [ -z "$TAG" ]; then
            log "looking for the newest release..."
            # /releases (not /releases/latest): the latest release may be a beta, which /latest ignores
            TAG="$(curl -fsSL --retry 3 --connect-timeout 10 --max-time 20 \
                "https://api.github.com/repos/${REPO}/releases?per_page=1" \
                | awk -F '"' '/"tag_name"[[:space:]]*:/ { print $4; exit }')" \
                || err "can't reach github.com. Please try again later."
            [ -n "$TAG" ] || err "no release found for ${REPO}"
        fi
        BASE="https://github.com/${REPO}/releases/download/${TAG}"
    fi
    log "installing ${TAG}"

    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT

    curl -fsSL --retry 3 --connect-timeout 10 --max-time 180 "${BASE}/${ASSET}" -o "${TMP}/${ASSET}" \
        || err "download failed: ${BASE}/${ASSET} (does release ${TAG} have a binary for ${os}/${arch}?)"
    curl -fsSL --retry 3 --connect-timeout 10 --max-time 20 "${BASE}/SHA256SUMS" -o "${TMP}/SHA256SUMS" \
        || err "download failed: ${BASE}/SHA256SUMS"

    EXPECTED="$(awk -v name="$ASSET" '$2 == name || $2 == "*" name { print tolower($1); exit }' "${TMP}/SHA256SUMS")"
    [ "${#EXPECTED}" -eq 64 ] || err "SHA256SUMS has no valid checksum for ${ASSET}"

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum < "${TMP}/${ASSET}" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 < "${TMP}/${ASSET}" | awk '{ print $1 }')"
    elif command -v openssl >/dev/null 2>&1; then
        ACTUAL="$(openssl dgst -sha256 < "${TMP}/${ASSET}" | awk '{ print $NF }')"
    else
        err "checking the download needs sha256sum, shasum or openssl"
    fi
    [ "$ACTUAL" = "$EXPECTED" ] || err "the downloaded file does not match its checksum; nothing was installed"
    log "checksum ok"

    mkdir -p "$INSTALL_DIR"
    mv "${TMP}/${ASSET}" "${INSTALL_DIR}/${BIN}"
    chmod +x "${INSTALL_DIR}/${BIN}"
    log "installed ${INSTALL_DIR}/${BIN}"

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            echo ""
            warn "${INSTALL_DIR} is not in your PATH. Add it to your shell config:"
            echo ""
            echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
            ;;
    esac

    echo ""
    log "next: enable the orchestrator and start it"
    echo "    mkdir -p ~/.config/shepherd && printf '[shepherd]\\nenabled = true\\n' > ~/.config/shepherd/config.toml"
    echo "    shepherd"
    echo ""
}

log()  { printf '  \033[32m>\033[0m %s\n' "$1"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; }
err()  { printf '  \033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "requires '$1'; install it first"; }

main "$@"
