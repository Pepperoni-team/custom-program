//! A month's leaf and its proof (spec §5), on plain values.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::keccak::hashv;

use crate::constants::DIVIDEND_LEAF_DOMAIN;

/// Verify a sorted-pair keccak256 merkle proof of `leaf` against `root`: each
/// pair hashed in byte order, as `pepper_curve`'s v1 and v2 (copied from its
/// `math.rs`), so one tree builder serves the three.
pub fn verify_merkle_proof(proof: &[[u8; 32]], root: [u8; 32], leaf: [u8; 32]) -> bool {
    let mut computed = leaf;
    for sibling in proof.iter() {
        computed = if computed <= *sibling {
            hashv(&[&computed, sibling]).0
        } else {
            hashv(&[sibling, &computed]).0
        };
    }
    computed == root
}

/// A month's leaf: keccak256(`DIVIDEND_LEAF_DOMAIN` ‖ distributor ‖ usdc_mint ‖
/// index u32 LE ‖ claimant ‖ amount u64 LE). The distributor's address binds a
/// root to one month of one token, the mint to USDC, and the index to the one
/// bit of the claim registry its claim sets.
pub fn dividend_leaf(distributor: &Pubkey, usdc_mint: &Pubkey, index: u32, claimant: &Pubkey, amount: u64) -> [u8; 32] {
    hashv(&[
        DIVIDEND_LEAF_DOMAIN,
        distributor.as_ref(),
        usdc_mint.as_ref(),
        &index.to_le_bytes(),
        claimant.as_ref(),
        &amount.to_le_bytes(),
    ])
    .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn key(byte: u8) -> Pubkey {
        Pubkey::new_from_array([byte; 32])
    }

    fn hex(bytes: [u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn usdc() -> Pubkey {
        Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap()
    }

    fn pair(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
        if a <= b {
            hashv(&[&a, &b]).0
        } else {
            hashv(&[&b, &a]).0
        }
    }

    // The TypeScript leaf must reproduce this vector (spec §5): a client that hashes
    // a leaf differently builds trees no claim can prove. Fixed inputs, no PDA of the
    // program id, so the id's swap moved nothing: distributor [1; 32], mainnet USDC,
    // index 5, claimant [7; 32] (as v1's and v2's vectors), 1,000,000 µUSDC.
    #[test]
    fn the_leaf_matches_its_golden_vector() {
        assert_eq!(
            hex(dividend_leaf(&key(1), &usdc(), 5, &key(7), 1_000_000)),
            "e7dc3a5440277fd51aeac42e419aee7e69e7d411cc32909df895f241142c3f85"
        );
    }

    #[test]
    fn the_leaf_is_never_a_sol_months() {
        // pepper_curve's v2 leaf of the same index, holder and amount, by hand, at
        // its own pinned vector (pepper_curve's math.rs), then ours apart from it.
        let v2 = hashv(&[b"fanex:dividend:v2", &5u32.to_le_bytes(), key(7).as_ref(), &1_000_000u64.to_le_bytes()]).0;
        assert_eq!(hex(v2), "1268ad549425a568c5d9c291e109de5725f594e9e565d52f8e6400c610456105");
        assert_ne!(dividend_leaf(&key(1), &usdc(), 5, &key(7), 1_000_000), v2);
    }

    #[test]
    fn the_leaf_binds_every_input() {
        let base = dividend_leaf(&key(1), &usdc(), 5, &key(7), 1_000_000);
        assert_ne!(dividend_leaf(&key(2), &usdc(), 5, &key(7), 1_000_000), base, "another month");
        assert_ne!(dividend_leaf(&key(1), &key(3), 5, &key(7), 1_000_000), base, "another mint");
        assert_ne!(dividend_leaf(&key(1), &usdc(), 6, &key(7), 1_000_000), base, "another index");
        assert_ne!(dividend_leaf(&key(1), &usdc(), 5, &key(8), 1_000_000), base, "another holder");
        assert_ne!(dividend_leaf(&key(1), &usdc(), 5, &key(7), 1_000_001), base, "another amount");
    }

    #[test]
    fn a_single_leaf_is_its_own_root() {
        let leaf = dividend_leaf(&key(1), &usdc(), 0, &key(7), 500);
        assert!(verify_merkle_proof(&[], leaf, leaf));
    }

    #[test]
    fn each_proof_of_a_four_leaf_tree_verifies_its_own_leaf_only() {
        let distributor = key(1);
        let leaves: Vec<[u8; 32]> =
            (0..4u32).map(|i| dividend_leaf(&distributor, &usdc(), i, &key(10 + i as u8), 1_000 * (i as u64 + 1))).collect();
        let (left, right) = (pair(leaves[0], leaves[1]), pair(leaves[2], leaves[3]));
        let root = pair(left, right);
        let proofs = [[leaves[1], right], [leaves[0], right], [leaves[3], left], [leaves[2], left]];
        for (i, proof) in proofs.iter().enumerate() {
            assert!(verify_merkle_proof(proof, root, leaves[i]), "leaf {i}");
            // The same holder and amount under another index, or another month, prove nothing.
            let index = i as u32;
            let amount = 1_000 * (i as u64 + 1);
            assert!(!verify_merkle_proof(proof, root, dividend_leaf(&distributor, &usdc(), index + 1, &key(10 + i as u8), amount)));
            assert!(!verify_merkle_proof(proof, root, dividend_leaf(&key(2), &usdc(), index, &key(10 + i as u8), amount)));
            // Nor does a larger amount, nor a proof cut short.
            assert!(!verify_merkle_proof(proof, root, dividend_leaf(&distributor, &usdc(), index, &key(10 + i as u8), amount + 1)));
            assert!(!verify_merkle_proof(&proof[..1], root, leaves[i]));
        }
    }
}
