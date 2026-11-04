#!/bin/sh
set -eu
case "$(uname -s):$(uname -m)" in
  Darwin:arm64) pacote=gh_2.96.0_macOS_arm64.zip; soma=f23a0c37d963aacc3bed703ccbd59b41c5ca22101fab7f00eb2b7cad23aba463; alvo=macos-arm64;;
  Linux:x86_64) pacote=gh_2.96.0_linux_amd64.tar.gz; soma=83d5c2ccad5498f58bf6368acb1ab32588cf43ab3a4b1c301bf36328b1c8bd60; alvo=linux-x64;;
  *) echo 'Esta plataforma ainda não possui instalador aprovado.' >&2; exit 1;;
esac
umask 077
temporario=$(mktemp -d)
trap 'rm -rf "$temporario"' EXIT HUP INT TERM
curl -fLsS "https://github.com/cli/cli/releases/download/v2.96.0/$pacote" -o "$temporario/$pacote"
if command -v sha256sum >/dev/null; then
  atual=$(sha256sum "$temporario/$pacote" | cut -d ' ' -f 1)
else
  atual=$(shasum -a 256 "$temporario/$pacote" | cut -d ' ' -f 1)
fi
[ "$atual" = "$soma" ] || { echo 'Integridade do GitHub CLI não confirmada.' >&2; exit 1; }
case "$pacote" in *.zip) unzip -q "$temporario/$pacote" -d "$temporario/gh";; *) mkdir "$temporario/gh"; tar -xzf "$temporario/$pacote" -C "$temporario/gh";; esac
binario=$(find "$temporario/gh" -type f -path '*/bin/gh' -print)
[ -n "$binario" ] && [ -x "$binario" ] || { echo 'GitHub CLI não encontrado no pacote.' >&2; exit 1; }
GH_CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/pretorian/github"
export GH_CONFIG_DIR
mkdir -p "$GH_CONFIG_DIR"
unset GH_TOKEN GITHUB_TOKEN
if ! "$binario" auth status --hostname github.com >/dev/null 2>&1; then
  "$binario" auth login --hostname github.com --git-protocol https --web
fi
PATH="$(dirname "$binario"):$PATH"
export PATH
nome="pretorian-instalar-$alvo"
versao=$("$binario" api repos/lamarck-distribution-center/pretorian-cli/releases/latest --jq .tag_name)
printf '%s\n' "$versao" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || { echo 'Versão de distribuição inválida.' >&2; exit 1; }
digest=$("$binario" api "repos/lamarck-distribution-center/pretorian-cli/releases/tags/$versao" --jq ".assets[] | select(.name == \"$nome\") | .digest")
case "$digest" in sha256:*) ;; *) echo 'Instalador não possui hash verificável.' >&2; exit 1;; esac
"$binario" release download "$versao" --repo lamarck-distribution-center/pretorian-cli --pattern "$nome" --dir "$temporario"
if command -v sha256sum >/dev/null; then
  atual=$(sha256sum "$temporario/$nome" | cut -d ' ' -f 1)
else
  atual=$(shasum -a 256 "$temporario/$nome" | cut -d ' ' -f 1)
fi
[ "sha256:$atual" = "$digest" ] || { echo 'Integridade do instalador não confirmada.' >&2; exit 1; }
chmod 700 "$temporario/$nome"
"$temporario/$nome" "$@"
