#!/usr/bin/env bash
# Dumps MetaDAO's launchpad v0.7, the program Pepper's outside raises commit to (docs/solana/EXTERNAL-RAISES.md §21),
# from MAINNET, read-only, into tests-onchain/metadao-launchpad/fixtures/, and writes its SHA256SUMS. Nothing runs it in
# CI: the binary is committed, so the suite needs no network, and tests-onchain/metadao-launchpad/tests/launchpad.rs
# pins its sha256 by hand, so a re-dump that brings another binary (an upgrade on mainnet) stays red until someone
# reads what changed and moves the pin on purpose.
#
# ⚠️ Third-party code, committed for the tests only (the owner, 2026-11-14), under the same rule as the pepper-curve
# suite's: kept out of every public mirror of this repository (scripts/solana/dump-litesvm-fixtures.sh).
#
# Reads only, as dump-litesvm-fixtures.sh does: `solana program dump` with the Agave 3.1.14 CLI named by its path and
# an empty CLI config, the RPC from SOLANA_RPC or the env file (FANEX_ENV_FILE, default .env), never printed, only its
# host, and an endpoint whose genesis hash is not mainnet-beta's refused.
#
# Usage, from WSL at the repo root:
#   bash scripts/solana/dump-launchpad-fixture.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

SOLANA="$HOME/.local/share/solana/install/releases/3.1.14/solana-release/bin/solana"
OUT=tests-onchain/metadao-launchpad/fixtures
FILE=launchpad_v0_7.so
PROGRAM=moontUzsdepotRGe5xsfip7vLPTJnVuafqdUWexVnPM
MAINNET_GENESIS=5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d

if ! "$SOLANA" --version 2>/dev/null | grep -q '^solana-cli 3\.1\.14 '; then
  echo "no Agave 3.1.14 CLI at $SOLANA (docs/solana/PEPPER-CURVE-PROGRAM.md §13)" >&2
  exit 1
fi

rpc="${SOLANA_RPC:-}"
if [ -z "$rpc" ]; then
  env_file="${FANEX_ENV_FILE:-.env}"
  if [ ! -f "$env_file" ]; then
    echo "SOLANA_RPC is not set and $env_file does not exist: set FANEX_ENV_FILE" >&2
    exit 1
  fi
  rpc=$(grep -E '^SOLANA_RPC=' "$env_file" | tail -n 1 | cut -d= -f2- || true)
  rpc="${rpc%$'\r'}"
  rpc="${rpc#\"}"; rpc="${rpc%\"}"; rpc="${rpc#\'}"; rpc="${rpc%\'}"
fi
if [ -z "$rpc" ]; then
  echo "no SOLANA_RPC in the environment or the env file" >&2
  exit 1
fi
host="${rpc#*://}"
host="${host%%[/?]*}"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# `program show` asks for a default signer even to read: a throwaway key, made here and gone with the folder, that
# holds nothing and signs nothing (no ~/.config/solana is ever read).
"$(dirname "$SOLANA")/solana-keygen" new --no-bip39-passphrase --silent --force -o "$tmp/throwaway.json" >/dev/null

cli() {
  if ! "$SOLANA" --config "$tmp/no-config.yml" --keypair "$tmp/throwaway.json" --url "$rpc" --commitment finalized "$@" >"$tmp/cli.log" 2>&1; then
    sed -E 's/(api-key=)[^&[:space:]"]+/\1<hidden>/g' "$tmp/cli.log" >&2
    exit 1
  fi
}

echo "  · reading mainnet through $host with $("$SOLANA" --version)"
cli genesis-hash
genesis=$(tr -d '[:space:]' <"$tmp/cli.log")
if [ "$genesis" != "$MAINNET_GENESIS" ]; then
  echo "SOLANA_RPC answers for genesis $genesis, not mainnet-beta: refused" >&2
  exit 1
fi

cli program show "$PROGRAM" --output json
slot=$(grep -o '"lastDeploySlot": *[0-9]*' "$tmp/cli.log" | grep -o '[0-9]*$' || echo '?')
cli program dump "$PROGRAM" "$tmp/$FILE"
echo "  · $FILE  $PROGRAM  last deployed at slot $slot, $(wc -c <"$tmp/$FILE") bytes"

mkdir -p "$OUT"
mv -f "$tmp/$FILE" "$OUT/$FILE"
(cd "$OUT" && sha256sum "$FILE" >SHA256SUMS)
cat "$OUT/SHA256SUMS"
echo "  · written to $OUT: compare with the pin in tests-onchain/metadao-launchpad/tests/launchpad.rs"
