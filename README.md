# custom-program

The on-chain programs behind [Pepper](https://pepper.fan) token launches: tokens of YouTube creators and
their videos, traded on a bonding curve in USDC until they graduate to a Raydium CPMM pool, with holder
dividends paid in USDC.

| Program | Program ID | What it does |
|---|---|---|
| [`pepper_curve`](programs/pepper_curve) | `CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ` | The bonding curve: launch, buy and sell exact-in, every fee taken inside the trade, creator vesting, graduation to a Raydium CPMM pool with its LP locked, and the one-time migration of the live SOL curves to USDC |
| [`pepper_dividends`](programs/pepper_dividends) | `GdWxRsHF5wneNgLQmdoKS9Zcu6enMVzoVTe9EFaxJxcU` | Monthly holder dividends in USDC: each month's USDC sits in its own vault against a Merkle root, and each holder claims their own amount |
| [`pepper_curve_math`](programs/pepper_curve_math) | (a library) | The curve's integer math, shared by the program and the Jupiter SDK, with test vectors |

Framework: Anchor `0.31.1`. IDLs: [`pepper_curve.json`](lib/solana/pepper-curve/idl/pepper_curve.json),
[`pepper_dividends.json`](lib/solana/pepper-dividends/idl/pepper_dividends.json), at the paths the Jupiter SDK
reads them from; both are also published on chain.

Some identifiers keep the platform's earlier name, Fanex (`config.fanex_fee_wallet`, `FanexError`): they
are part of the deployed account layout and of the IDL every client decodes with.

## What runs on mainnet

| Program | Build | Executable hash (`solana-verify`) |
|---|---|---|
| `pepper_curve` | the verifiable build of this repository's `programs/pepper_curve` | `7d0b4fdee2c0f0f2b23c0c7783c69606e863ea34bab6341061a50487f10b679a` |
| `pepper_dividends` | the verifiable build of `programs/pepper_dividends` | `200a978c9f28a272a75f109856421217485c5b110a303d4cd4ad5f76ce702e08` |

Both programs are upgradeable, and their upgrade authority is the vault of a
[Squads v4](https://squads.xyz) multisig, three signatures of five: no single key can change either
program. Read it from the chain:

```bash
solana program show CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ
solana program show GdWxRsHF5wneNgLQmdoKS9Zcu6enMVzoVTe9EFaxJxcU
```

`main` can be ahead of the binary deployed on a cluster; the table above names what mainnet runs.

## Verify a deployed program

```bash
solana-verify get-program-hash CDPwGWGWbAE8FL5f2oVdzfkTutrnsKJcAcgqWktC3dwZ --url https://api.mainnet-beta.solana.com
solana-verify build --library-name pepper_curve        # in a clone of this repository, Docker running
solana-verify get-executable-hash target/deploy/pepper_curve.so
```

The two hashes are equal when the clone is the source of the deployed binary; the same holds for
`pepper_dividends` with `--library-name pepper_dividends`.

## Repository layout

- `programs/`: the two programs and the math crate.
- `lib/solana/*/idl/`: the IDLs, equal to a fresh `anchor idl build`.
- `tests-onchain/pepper-curve`, `tests-onchain/pepper-dividends`: LiteSVM tests against the built `.so`
  files. Third-party programs the tests load (Raydium, Meteora, Orca, Jupiter, Metaplex, p-token) are not
  committed: dump them from mainnet with `scripts/solana/dump-litesvm-fixtures.sh` (read only, a mainnet RPC
  in `SOLANA_RPC`); the tests pin each file's sha256.
- `tests-onchain/pepper-curve-jupiter`: the Jupiter SDK, an implementation of Jupiter's `Amm` trait that
  quotes a curve from its account and the config alone, with no network call, and builds the exact-in
  swap; its parity tests run the program's own swap in LiteSVM and compare the tokens received.

## Build and test

```bash
cargo test -p pepper_curve -p pepper_curve_math -p pepper_dividends --lib --locked
cargo-build-sbf --manifest-path programs/pepper_curve/Cargo.toml
cargo-build-sbf --manifest-path programs/pepper_dividends/Cargo.toml
SOLANA_RPC=<a mainnet RPC> bash scripts/solana/dump-litesvm-fixtures.sh
(cd tests-onchain/pepper-curve && cargo test --locked)
(cd tests-onchain/pepper-dividends && cargo test --locked)
(cd tests-onchain/pepper-curve-jupiter && cargo test --locked)
```

## Security

Please report a vulnerability privately, through this repository's
[security advisories](https://github.com/Pepperoni-team/custom-program/security/advisories/new), never in a
public issue. Both programs publish this policy in their on-chain security metadata.
