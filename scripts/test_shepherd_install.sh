#!/bin/sh
# Offline test of install.sh: serves fake release assets from a local http server and checks that the
# installer installs a matching binary and refuses a corrupted one. No network and no real binary needed.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'kill $SRV 2>/dev/null; rm -rf "$T"' EXIT
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=macos ;; *) echo "skip: unsupported OS"; exit 0 ;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; *) echo "skip: unsupported arch"; exit 0 ;; esac
asset="shepherd-$os-$arch"
mkdir -p "$T/rel" "$T/home"
printf '#!/bin/sh\necho shepherd-test-binary\n' > "$T/rel/$asset"
sum="$(sha256sum "$T/rel/$asset" 2>/dev/null || shasum -a 256 "$T/rel/$asset")"
printf '%s  %s\n' "$(printf '%s' "$sum" | awk '{print $1}')" "$asset" > "$T/rel/SHA256SUMS"
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
(cd "$T/rel" && python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) & SRV=$!
sleep 1
export HOME="$T/home" SHEPHERD_INSTALL_DIR="$T/home/bin" SHEPHERD_RELEASE_BASE="http://127.0.0.1:$PORT"

sh "$ROOT/install.sh" >/dev/null
[ "$("$T/home/bin/shepherd")" = "shepherd-test-binary" ] || { echo "FAIL: installed binary does not run"; exit 1; }
echo "ok: installs a matching binary"

rm -f "$T/home/bin/shepherd"
printf 'tampered\n' >> "$T/rel/$asset"
if sh "$ROOT/install.sh" >/dev/null 2>&1; then echo "FAIL: installed a corrupted binary"; exit 1; fi
[ ! -e "$T/home/bin/shepherd" ] || { echo "FAIL: left a corrupted binary behind"; exit 1; }
echo "ok: refuses a binary that does not match its checksum"
