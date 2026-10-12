#![allow(dead_code)]

use anchor_lang::solana_program::keccak::hashv;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, Space, ToAccountMetas};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::token::spl_token;
use fanex_curve::constants::*;
use fanex_curve::state::{Curve, CurvePhase, SolVault};
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{AccountMeta, Instruction, InstructionError},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::{Transaction, TransactionError},
};

pub const SOL: u64 = 1_000_000_000;
pub const DAY: i64 = 86_400;
pub const OCT_5_2026: i64 = 1_791_158_400;
pub const MONTH: i64 = 202_611;

pub const ACCOUNT_DISCRIMINATOR_MISMATCH: u32 = 3002;

pub const ACCOUNT_NOT_SIGNER: u32 = 3010;

pub fn program_so() -> String {
    std::env::var("FANEX_CURVE_SO").unwrap_or_else(|_| "../../target/deploy/fanex_curve.so".to_string())
}

pub fn code(e: fanex_curve::errors::FanexError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

pub fn expect_err(result: Result<(), TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => {
            assert_eq!(c, expected, "custom error {c}, expected {expected}")
        }
        other => panic!("expected custom error {expected}, got {other:?}"),
    }
}

pub fn ix<A: ToAccountMetas, D: InstructionData>(accounts: A, data: D) -> Instruction {
    Instruction { program_id: fanex_curve::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

pub fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &fanex_curve::ID).0
}
pub fn config_pda() -> Pubkey {
    pda(&[CONFIG_SEED])
}
pub fn curve_pda(mint: &Pubkey) -> Pubkey {
    pda(&[CURVE_SEED, mint.as_ref()])
}
pub fn distributor_pda(mint: &Pubkey, month: i64) -> Pubkey {
    pda(&[DISTRIBUTOR_SEED, mint.as_ref(), &month.to_le_bytes()])
}
pub fn claim_status_pda(distributor: &Pubkey, claimant: &Pubkey) -> Pubkey {
    pda(&[CLAIM_SEED, distributor.as_ref(), claimant.as_ref()])
}
pub fn transit_pda(source: &Pubkey) -> Pubkey {
    pda(&[PAYOUT_TRANSIT_SEED, source.as_ref()])
}

pub struct Tree {
    pub root: [u8; 32],
    pub proofs: Vec<Vec<[u8; 32]>>,
}

fn hash_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    if a <= b {
        hashv(&[a, b]).0
    } else {
        hashv(&[b, a]).0
    }
}

pub fn build_tree(leaves: &[[u8; 32]]) -> Tree {
    let mut layers: Vec<Vec<[u8; 32]>> = vec![leaves.to_vec()];
    while layers.last().unwrap().len() > 1 {
        let prev = layers.last().unwrap();
        let next: Vec<[u8; 32]> = prev
            .chunks(2)
            .map(|pair| if pair.len() == 2 { hash_pair(&pair[0], &pair[1]) } else { pair[0] })
            .collect();
        layers.push(next);
    }
    let proofs = (0..leaves.len())
        .map(|leaf| {
            let mut proof = Vec::new();
            let mut idx = leaf;
            for layer in &layers[..layers.len() - 1] {
                let sibling = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
                if sibling < layer.len() {
                    proof.push(layer[sibling]);
                }
                idx /= 2;
            }
            proof
        })
        .collect();
    Tree { root: layers.last().unwrap()[0], proofs }
}

pub struct V2Month {
    pub holders: Vec<(Keypair, u64)>,
    pub tree: Tree,
    pub total: u64,
}

impl V2Month {
    pub fn proof(&self, index: usize) -> Vec<[u8; 32]> {
        self.tree.proofs[index].clone()
    }
}

pub struct Env {
    pub svm: LiteSVM,
    pub admin: Keypair,
    pub creator: Keypair,
    pub mint: Pubkey,
}

impl Env {
    pub fn new() -> Self {
        Self::with_svm(LiteSVM::new())
    }

    pub fn unverified() -> Self {
        Self::with_svm(LiteSVM::new().with_sigverify(false))
    }

    fn with_svm(mut svm: LiteSVM) -> Self {
        svm.add_program_from_file(fanex_curve::ID, program_so())
            .expect("build first: cargo-build-sbf --manifest-path programs/fanex_curve/Cargo.toml");
        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 1_000 * SOL).unwrap();
        let creator = Keypair::new();
        svm.airdrop(&creator.pubkey(), 100 * SOL).unwrap();
        let mut env = Self { svm, admin, creator, mint: Pubkey::new_unique() };
        env.set_time(OCT_5_2026);
        env.init_config();
        env.write_curve();
        env
    }

    pub fn send(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let admin = self.admin.insecure_clone();
        self.send_paid_by(&admin, ixs, extra)
    }

    pub fn send_unverified(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let admin = self.admin.insecure_clone();
        let mut signers: Vec<&Keypair> = vec![&admin];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != admin.pubkey()));
        let mut tx = Transaction::new_unsigned(Message::new(ixs, Some(&admin.pubkey())));
        tx.partial_sign(&signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|_| ()).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    pub fn send_paid_by(&mut self, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let mut signers: Vec<&Keypair> = vec![payer];
        signers.extend(extra.iter().copied().filter(|k| k.pubkey() != payer.pubkey()));
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|_| ()).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    pub fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
    }

    pub fn exists(&self, address: &Pubkey) -> bool {
        self.svm.get_account(address).map(|a| a.lamports > 0).unwrap_or(false)
    }

    pub fn fetch<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let account = self.svm.get_account(address).expect("account exists");
        T::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    fn init_config(&mut self) {
        let init = ix(
            fanex_curve::accounts::InitializeConfig {
                admin: self.admin.pubkey(),
                config: config_pda(),
                system_program: system_program::id(),
            },
            fanex_curve::instruction::InitializeConfig {
                fanex_fee_wallet: self.admin.pubkey(),
                graduation_authority: self.admin.pubkey(),
            },
        );
        self.send(&[init], &[]).unwrap();
    }

    fn write_curve(&mut self) {
        let (address, bump) = Pubkey::find_program_address(&[CURVE_SEED, self.mint.as_ref()], &fanex_curve::ID);
        let curve = Curve {
            mint: self.mint,
            creator: self.creator.pubkey(),
            phase: CurvePhase::Bonding,
            creator_target_lamports: 0,
            cpmm_seed_lamports: 0,
            creator_split_bps: 0,
            fanex_split_bps: 0,
            creator_cap_lamports: 0,
            fanex_cap_lamports: 0,
            virtual_sol_reserves: 0,
            virtual_token_reserves: 0,
            real_sol_reserves: 0,
            real_token_reserves: 0,
            migration_token_reserves: 0,
            creator_received_lamports: 0,
            fanex_received_lamports: 0,
            paused: false,
            created_at: 0,
            graduated_at: 0,
            bump,
            sol_vault_bump: 0,
        };
        let mut data = Vec::with_capacity(8 + Curve::INIT_SPACE);
        curve.try_serialize(&mut data).unwrap();
        data.resize(8 + Curve::INIT_SPACE, 0);
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(address, Account { lamports, data, owner: fanex_curve::ID, executable: false, rent_epoch: 0 })
            .unwrap();
    }

    pub fn write_tradeable_curve(&mut self, created_at: i64) {
        let curve_address = curve_pda(&self.mint);
        let (sol_vault, sol_vault_bump) = Pubkey::find_program_address(&[SOL_VAULT_SEED, self.mint.as_ref()], &fanex_curve::ID);
        let (_, curve_bump) = Pubkey::find_program_address(&[CURVE_SEED, self.mint.as_ref()], &fanex_curve::ID);
        let real_token_reserves = 80_000_000 * 1_000_000;
        let migration_token_reserves = 14_000_000 * 1_000_000;

        let mut mint = vec![0u8; spl_token::state::Mint::LEN];
        spl_token::state::Mint {
            mint_authority: COption::None,
            supply: TOKEN_TOTAL_SUPPLY,
            decimals: TOKEN_DECIMALS,
            is_initialized: true,
            freeze_authority: COption::None,
        }
        .pack_into_slice(&mut mint);
        self.put(self.mint, mint, spl_token::ID);

        let mut vault = vec![0u8; spl_token::state::Account::LEN];
        spl_token::state::Account {
            mint: self.mint,
            owner: curve_address,
            amount: real_token_reserves + migration_token_reserves,
            delegate: COption::None,
            state: spl_token::state::AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        }
        .pack_into_slice(&mut vault);
        self.put(ata(&curve_address, &self.mint), vault, spl_token::ID);

        let mut sol_vault_data = Vec::new();
        SolVault { mint: self.mint, bump: sol_vault_bump }.try_serialize(&mut sol_vault_data).unwrap();
        self.put(sol_vault, sol_vault_data, fanex_curve::ID);

        let curve = Curve {
            mint: self.mint,
            creator: self.creator.pubkey(),
            phase: CurvePhase::Bonding,
            creator_target_lamports: 0,
            cpmm_seed_lamports: 24 * SOL,
            creator_split_bps: 0,
            fanex_split_bps: 0,
            creator_cap_lamports: 0,
            fanex_cap_lamports: 0,
            virtual_sol_reserves: 5 * SOL,
            virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
            real_sol_reserves: 0,
            real_token_reserves,
            migration_token_reserves,
            creator_received_lamports: 0,
            fanex_received_lamports: 0,
            paused: false,
            created_at,
            graduated_at: 0,
            bump: curve_bump,
            sol_vault_bump,
        };
        let mut data = Vec::with_capacity(8 + Curve::INIT_SPACE);
        curve.try_serialize(&mut data).unwrap();
        data.resize(8 + Curve::INIT_SPACE, 0);
        self.put(curve_address, data, fanex_curve::ID);
    }

    fn put(&mut self, address: Pubkey, data: Vec<u8>, owner: Pubkey) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(address, Account { lamports, data, owner, executable: false, rent_epoch: 0 })
            .unwrap();
    }

    pub fn funded_wallet(&mut self, lamports: u64) -> Keypair {
        let wallet = Keypair::new();
        self.svm.airdrop(&wallet.pubkey(), lamports).unwrap();
        wallet
    }

    pub fn buy(&mut self, buyer: &Keypair, sol_in: u64) -> Result<(), TransactionError> {
        let curve = curve_pda(&self.mint);
        let buy = ix(
            fanex_curve::accounts::Buy {
                buyer: buyer.pubkey(),
                config: config_pda(),
                curve,
                mint: self.mint,
                sol_vault: pda(&[SOL_VAULT_SEED, self.mint.as_ref()]),
                token_vault: ata(&curve, &self.mint),
                buyer_token_account: ata(&buyer.pubkey(), &self.mint),
                creator: self.creator.pubkey(),
                fanex: self.admin.pubkey(),
                token_program: spl_token::ID,
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
            },
            fanex_curve::instruction::Buy { sol_in, min_tokens_out: 0 },
        );
        let buyer = buyer.insecure_clone();
        self.send_paid_by(&buyer, &[buy], &[])
    }

    pub fn token_balance(&self, owner: &Pubkey) -> u64 {
        self.svm
            .get_account(&ata(owner, &self.mint))
            .map(|account| spl_token::state::Account::unpack(&account.data).unwrap().amount)
            .unwrap_or(0)
    }

    pub fn set_paused(&mut self, paused: bool) {
        let pause = ix(
            fanex_curve::accounts::UpdateConfig { config: config_pda(), admin: self.admin.pubkey() },
            fanex_curve::instruction::SetPaused { paused },
        );
        self.send(&[pause], &[]).unwrap();
    }

    pub fn v2_month(&mut self, n: usize) -> V2Month {
        let holders: Vec<(Keypair, u64)> = (0..n)
            .map(|i| {
                let kp = Keypair::new();
                self.svm.airdrop(&kp.pubkey(), SOL).unwrap();
                (kp, 10_000_000 + i as u64 * 1_000)
            })
            .collect();
        let leaves: Vec<[u8; 32]> = holders
            .iter()
            .enumerate()
            .map(|(i, (kp, amount))| fanex_curve::math::dividend_leaf_v2(i as u32, &kp.pubkey(), *amount))
            .collect();
        let total = holders.iter().map(|(_, a)| a).sum();
        V2Month { holders, tree: build_tree(&leaves), total }
    }

    pub fn init_v2(
        &mut self,
        funder: &Keypair,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Result<(), TransactionError> {
        let ix = self.init_v2_ix(funder, &self.admin.pubkey(), root, total, expires_at, leaf_count);
        let funder = funder.insecure_clone();
        self.send(&[ix], &[&funder])
    }

    pub fn init_v2_ix(
        &self,
        funder: &Keypair,
        approver: &Pubkey,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Instruction {
        self.init_v2_ix_by(&funder.pubkey(), approver, root, total, expires_at, leaf_count)
    }

    pub fn init_v2_ix_by(
        &self,
        funder: &Pubkey,
        approver: &Pubkey,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
        leaf_count: u32,
    ) -> Instruction {
        ix(
            fanex_curve::accounts::InitDistributorV2 {
                funder: *funder,
                approver: *approver,
                config: config_pda(),
                mint: self.mint,
                curve: curve_pda(&self.mint),
                distributor: distributor_pda(&self.mint, MONTH),
                system_program: system_program::id(),
            },
            fanex_curve::instruction::InitDistributorV2 { month: MONTH, merkle_root: root, total_amount: total, expires_at, leaf_count },
        )
    }

    pub fn init_v1(&mut self, funder: &Keypair, root: [u8; 32], total: u64) -> Result<(), TransactionError> {
        self.init_v1_expiring(funder, root, total, OCT_5_2026 + 365 * DAY)
    }

    pub fn init_v1_expiring(
        &mut self,
        funder: &Keypair,
        root: [u8; 32],
        total: u64,
        expires_at: i64,
    ) -> Result<(), TransactionError> {
        let init = self.init_v1_ix_by(&funder.pubkey(), root, total, expires_at);
        let funder = funder.insecure_clone();
        self.send(&[init], &[&funder])
    }

    pub fn init_v1_ix_by(&self, funder: &Pubkey, root: [u8; 32], total: u64, expires_at: i64) -> Instruction {
        ix(
            fanex_curve::accounts::InitDistributor {
                funder: *funder,
                config: config_pda(),
                mint: self.mint,
                curve: curve_pda(&self.mint),
                distributor: distributor_pda(&self.mint, MONTH),
                system_program: system_program::id(),
            },
            fanex_curve::instruction::InitDistributor { month: MONTH, merkle_root: root, total_amount: total, expires_at },
        )
    }

    pub fn fund_v2(&mut self, month: &V2Month) {
        let admin = self.admin.insecure_clone();
        let expires = OCT_5_2026 + 365 * DAY;
        self.init_v2(&admin, month.tree.root, month.total, expires, month.holders.len() as u32).unwrap();
    }

    pub fn claim_v2(
        &mut self,
        claimant: &Keypair,
        index: u32,
        amount: u64,
        proof: Vec<[u8; 32]>,
        through_transit: bool,
    ) -> Result<(), TransactionError> {
        let distributor = distributor_pda(&self.mint, MONTH);
        let mut claim = ix(
            fanex_curve::accounts::ClaimDividendV2 {
                claimant: claimant.pubkey(),
                distributor,
                system_program: system_program::id(),
            },
            fanex_curve::instruction::ClaimDividendV2 { index, amount, proof },
        );
        if through_transit {
            claim.accounts.push(AccountMeta::new(transit_pda(&distributor), false));
        }
        let claimant = claimant.insecure_clone();
        self.send(&[claim], &[&claimant])
    }

    pub fn close_v2(&mut self, funder: &Keypair) -> Result<(), TransactionError> {
        let close = ix(
            fanex_curve::accounts::CloseDistributorV2 { distributor: distributor_pda(&self.mint, MONTH), funder: funder.pubkey() },
            fanex_curve::instruction::CloseDistributorV2 {},
        );
        let funder = funder.insecure_clone();
        let extra: Vec<&Keypair> = if funder.pubkey() == self.admin.pubkey() { vec![] } else { vec![&funder] };
        self.send(&[close], &extra)
    }

    pub fn fund_v1(&mut self, holders: &[(Keypair, u64)]) -> Tree {
        let leaves: Vec<[u8; 32]> = holders
            .iter()
            .map(|(kp, amount)| fanex_curve::math::dividend_leaf(&kp.pubkey(), *amount))
            .collect();
        let tree = build_tree(&leaves);
        let total = holders.iter().map(|(_, a)| a).sum();
        let admin = self.admin.insecure_clone();
        self.init_v1(&admin, tree.root, total).unwrap();
        tree
    }

    pub fn claim_v1(&mut self, claimant: &Keypair, amount: u64, proof: Vec<[u8; 32]>) -> Result<(), TransactionError> {
        let distributor = distributor_pda(&self.mint, MONTH);
        let claim = ix(
            fanex_curve::accounts::ClaimDividend {
                claimant: claimant.pubkey(),
                rent_payer: self.admin.pubkey(),
                distributor,
                claim_status: claim_status_pda(&distributor, &claimant.pubkey()),
                system_program: system_program::id(),
            },
            fanex_curve::instruction::ClaimDividend { amount, proof },
        );
        let claimant = claimant.insecure_clone();
        self.send(&[claim], &[&claimant])
    }
}
