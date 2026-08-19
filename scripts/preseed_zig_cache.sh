#!/bin/sh
set -eu

cache=${ZIG_GLOBAL_CACHE_DIR:-$(zig env | sed -n 's/.*\.global_cache_dir = "\(.*\)",/\1/p')}

if [ -z "$cache" ]; then
  echo "could not resolve Zig's global cache directory" >&2
  exit 1
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/herdr-zig-cache.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

find vendor/libghostty-vt -name build.zig.zon -exec awk '
  /\.url =/ {
    url = ""
    if ($0 ~ /"https:\/\//) {
      url = $0
      sub(/^[^"]*"/, "", url)
      sub(/".*/, "", url)
    }
    next
  }
  url != "" && /\.hash =/ {
    hash = $0
    sub(/^[^"]*"/, "", hash)
    sub(/".*/, "", hash)
    print url "\t" hash
    url = ""
  }
' {} + > "$temporary/dependencies"

# vaxis declares these only after its own archive has been imported.
printf '%s\t%s\n' \
  https://github.com/ivanstepanovftw/zigimg/archive/d7b7ab0ba0899643831ef042bd73289510b39906.tar.gz \
  zigimg-0.1.0-8_eo2vHnEwCIVW34Q14Ec-xUlzIoVg86-7FU2ypPtxms \
  https://github.com/jacobsandlund/uucode/archive/5f05f8f83a75caea201f12cc8ea32a2d82ea9732.tar.gz \
  uucode-0.1.0-ZZjBPj96QADXyt5sqwBJUnhaDYs_qBeeKijZvlRa0eqM \
  >> "$temporary/dependencies"
sort -u "$temporary/dependencies" -o "$temporary/dependencies"

while IFS="$(printf '\t')" read -r url expected; do
  if [ -d "$cache/p/$expected" ]; then
    continue
  fi
  archive="$temporary/${url##*/}"
  echo "fetching $url"
  curl --fail --location --retry 3 --retry-all-errors --silent --show-error \
    --output "$archive" "$url"
  if ! actual=$(zig fetch --global-cache-dir "$cache" "$archive"); then
    echo "zig could not import $url" >&2
    exit 1
  fi
  if [ "$actual" != "$expected" ]; then
    echo "hash mismatch for $url: expected $expected, got $actual" >&2
    exit 1
  fi
  echo "seeded $cache/p/$expected"
done < "$temporary/dependencies"
