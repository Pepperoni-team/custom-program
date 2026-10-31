#!/usr/bin/env bash
# Dumps the third-party programs and accounts the LiteSVM suite of pepper_curve loads into
# tests-onchain/pepper-curve/fixtures/, from MAINNET, read-only, and writes their SHA256SUMS
# (docs/solana/PEPPER-CURVE-PROGRAM.md §13). Nothing runs it in CI: the fixtures are committed, so the
# suite needs no network, and tests-onchain/pepper-curve/tests/fixtures.rs pins each file's sha256 by
# hand, so a re-dump that brings another binary (an upgrade on mainnet) stays red until someone reads
# what changed and moves the pin on purpose. The create-pool fee receiver's balance moves with every
# Raydium pool created on mainnet, so its JSON, and its pin, change at every re-dump.
#
# ⚠️ Third-party code: Metaplex Token Metadata, Raydium's CPMM and Raydium's LP lock (closed source).
# Committed for the tests only, and kept out of every public mirror of this repository
# (Pepperoni-team/custom-program, the Jupiter SDK's; Rick, 2026-10-22).
#
# Reads only: `solana program dump` and `solana account`, with the Agave 3.1.14 CLI named by its path
# (the release on PATH may be another) and an empty CLI config, so ~/.config/solana is never read. The
# RPC is SOLANA_RPC (the Helius key): from the environment, else from FANEX_ENV_FILE (default
# .env.local at the repo root). It is never printed, only its host, and an endpoint whose genesis hash
# is not mainnet-beta's is refused.
#
# Usage, from WSL at the repo root (a worktree has no .env.local of its own):
#   bash scripts/solana/dump-litesvm-fixtures.sh
#   FANEX_ENV_FILE=/mnt/c/Users/Rick/Desktop/Fanex-dev/.env.local bash scripts/solana/dump-litesvm-fixtures.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

SOLANA="$HOME/.local/share/solana/install/releases/3.1.14/solana-release/bin/solana"
OUT=tests-onchain/pepper-curve/fixtures
MAINNET_GENESIS=5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d

# file, then address. Programs: the ProgramData's bytes after its 45-byte header, as LiteSVM's
# add_program_from_file loads them. Accounts: the CLI's JSON (address, lamports, data in base64, owner).
PROGRAMS=(
  "mpl_token_metadata.so metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s"
  "raydium_cpmm.so CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C"
  "raydium_lp_lock.so LockrWmn6K5twhz3y9w1dQERbmgSaRkfnTeTKbpofwE"
)
ACCOUNTS=(
  "raydium_cpmm_amm_config.json G95xxie3XbkCqtE39GgQ9Ggc7xBC8Uceve7HFDEFApkc"
  "raydium_cpmm_create_pool_fee.json DNXgeM9EiiaAbaWvwjHj9fQQLAX5ZsfHyvmYUNRAdNC8"
)

if ! "$SOLANA" --version 2>/dev/null | grep -q '^solana-cli 3\.1\.14 '; then
  echo "no Agave 3.1.14 CLI at $SOLANA (docs/solana/PEPPER-CURVE-PROGRAM.md §13)" >&2
  exit 1
fi

rpc="${SOLANA_RPC:-}"
if [ -z "$rpc" ]; then
  env_file="${FANEX_ENV_FILE:-.env.local}"
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
mkdir "$tmp/out"

# One CLI call: a config file that does not exist (the CLI falls back to its defaults), the RPC passed
# by flag, finalized reads. On a failure its output is printed with any Helius key masked.
cli() {
  if ! "$SOLANA" --config "$tmp/no-config.yml" --url "$rpc" --commitment finalized "$@" >"$tmp/cli.log" 2>&1; then
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

names=()
for entry in "${PROGRAMS[@]}"; do
  read -r file address <<<"$entry"
  cli program show "$address" --output json
  slot=$(grep -o '"lastDeploySlot": *[0-9]*' "$tmp/cli.log" | grep -o '[0-9]*$' || echo '?')
  cli program dump "$address" "$tmp/out/$file"
  echo "  · $file  $address  last deployed at slot $slot, $(wc -c <"$tmp/out/$file") bytes"
  names+=("$file")
done
for entry in "${ACCOUNTS[@]}"; do
  read -r file address <<<"$entry"
  cli account "$address" --output json --output-file "$tmp/out/$file"
  echo "  · $file  $address"
  names+=("$file")
done

mkdir -p "$OUT"
for file in "${names[@]}"; do
  mv -f "$tmp/out/$file" "$OUT/$file"
done
sorted=$(printf '%s\n' "${names[@]}" | LC_ALL=C sort)
# shellcheck disable=SC2086 # the names hold no spaces
(cd "$OUT" && sha256sum $sorted >SHA256SUMS)
cat "$OUT/SHA256SUMS"
echo "  · written to $OUT: compare with the pins in tests-onchain/pepper-curve/tests/fixtures.rs"
