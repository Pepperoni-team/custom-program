//! `buy_exact_in`, the USDC curve's buy, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5, §5.2; usdc-curve design §10.3.5).
//!
//! The handler calls the math crate's `quote_buy` and moves exactly what it returns: every buy of
//! the golden vectors is replayed through the program to the base unit (the source, the
//! destination, the fee account, the creator's account, `usdc_vault`, the counters, the phase and
//! the two events). Then the shapes a router sends (an authority that does not own the
//! destination), the launch window (the signer's own ATA, the cap, the waiver, the first second
//! after it), each pause, a donation to `usdc_vault`, account 8 by the counters, a frozen fee
//! account, and the refusals across the two kinds of curve (3002 both ways). The curves are written
//! into the bank (`write_curve_usdc`) until `create_curve_usdc` exists.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::{AccountSerialize, Space};
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token::{self, error::TokenError};
use common::*;
use pepper_curve::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{GraduationReadyUsdc, TradeUsdc};
use pepper_curve::state::{CurveUsdc, UsdcConfig};
use pepper_curve::validation::creator_usdc_account_required;
use pepper_curve_math::{in_launch_window, quote_buy, BuyQuote, CurveState};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::{Transaction, TransactionError},
};

const EQUITY_100: &str = "equity, fresh: a 100 USDC buy";
const REVENUE_FRESH: &str = "revenue, fresh: a buy inside both caps";
const REVENUE_CAPS_FULL: &str = "revenue: a buy after both caps are full";

/// Account 8 and account 11 of the buy, by their positions in §10.3.5's table.
const CREATOR_ACCOUNT: usize = 8;
const WAIVER: usize = 11;

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

fn token(code: TokenError) -> u32 {
    code as u32
}

/// A wallet that buys: SOL for its fees, a USDC account holding what it was given, and its
/// associated token account of the curve's mint, opened beforehand (the buy opens nothing).
struct Buyer {
    wallet: Keypair,
    source: Pubkey,
    destination: Pubkey,
}

/// `owner`'s associated token account of `mint`, opened by the admin when missing.
fn open_ata(env: &mut Env, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &env.admin.pubkey(),
        owner,
        mint,
        &spl_token::ID,
    );
    env.send(&[open], &[]).unwrap();
    ata(owner, mint)
}

fn buyer(env: &mut Env, mint: &Pubkey, usdc: u64) -> Buyer {
    let wallet = env.funded_wallet(SOL);
    let source = env.usdc_account(&wallet.pubkey());
    if usdc > 0 {
        env.mint_usdc(&source, usdc);
    }
    let destination = open_ata(env, &wallet.pubkey(), mint);
    Buyer { wallet, source, destination }
}

/// `buy_exact_in` as a router builds it (§10.5): the 13 accounts in the table's order, account 8
/// by the curve's own counters (`creator_usdc_account_required`), no waiver.
fn buy_ix(
    env: &Env,
    curve: &CurveUsdc,
    authority: &Pubkey,
    source: &Pubkey,
    destination: &Pubkey,
    usdc_in: u64,
    min_tokens_out: u64,
) -> Instruction {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let address = curve_pda(&curve.mint);
    let creator_usdc_account =
        creator_usdc_account_required(curve.creator_split_bps, curve.creator_received_usdc, curve.creator_cap_usdc)
            .then_some(curve.creator_usdc_account);
    ix(
        pepper_curve::accounts::BuyExactIn {
            user_transfer_authority: *authority,
            user_source_usdc: *source,
            user_destination_token: *destination,
            usdc_config: usdc_config_pda(),
            curve: address,
            usdc_vault: usdc_vault_pda(&curve.mint),
            token_vault: ata(&address, &curve.mint),
            fee_usdc_account: config.fee_usdc_account,
            creator_usdc_account,
            usdc_mint: config.usdc_mint,
            token_mint: curve.mint,
            launch_guard_waiver: None,
            token_program: spl_token::ID,
        },
        pepper_curve::instruction::BuyExactIn { usdc_in, min_tokens_out },
    )
}

/// `buyer`'s own buy: it signs, pays its source and receives into its own ATA.
fn own_buy(env: &Env, curve: &CurveUsdc, buyer: &Buyer, usdc_in: u64) -> Instruction {
    buy_ix(env, curve, &buyer.wallet.pubkey(), &buyer.source, &buyer.destination, usdc_in, 0)
}

/// `ix` with the account at `position` replaced: a waiver passed, account 8 dropped or another
/// account in its place. The list keeps its 13 positions.
fn with_account(mut ix: Instruction, position: usize, meta: AccountMeta) -> Instruction {
    assert_eq!(ix.accounts.len(), 13);
    ix.accounts[position] = meta;
    ix
}

/// An absent `Option`: the program's own ID, read-only, in its place.
fn absent() -> AccountMeta {
    AccountMeta::new_readonly(pepper_curve::ID, false)
}

fn send(env: &mut Env, signer: &Keypair, ix: Instruction) -> Result<Vec<String>, TransactionError> {
    let signer = signer.insecure_clone();
    env.send_logged_paid_by(&signer, &[ix], &[])
}

fn unit(result: Result<Vec<String>, TransactionError>) -> Result<(), TransactionError> {
    result.map(|_| ())
}

/// The curve as the bank holds it now.
fn curve_now(env: &Env, mint: &Pubkey) -> CurveUsdc {
    env.fetch(&curve_pda(mint))
}

fn curve_bytes(env: &Env, mint: &Pubkey) -> Vec<u8> {
    env.svm.get_account(&curve_pda(mint)).unwrap().data
}

/// A curve at a fresh mint launched with the launch cap waived: `waive_launch_guard`, signed by the
/// launch guard authority, before the curve exists (§5.2), then the curve written.
fn write_waived_curve(env: &mut Env, state: &CurveState) -> CurveUsdc {
    let guard = Keypair::new();
    let admin = env.admin.insecure_clone();
    env.set_launch_guard_authority_by(&admin, &guard.pubkey()).unwrap();
    let mint = Pubkey::new_unique();
    let creator = env.creator.insecure_clone();
    env.waive(&creator, &guard, &mint).unwrap();
    let curve = env.curve_usdc(mint, state);
    env.put_curve_usdc(&curve);
    curve
}

/// A written config and curve, with the clock at `now`.
fn curve_at(state: &CurveState, now: i64) -> (Env, CurveUsdc) {
    let mut env = Env::new();
    env.write_usdc_config();
    let curve = env.write_curve_usdc(state);
    env.set_time(now);
    (env, curve)
}

/// What every party of a buy holds, in base units: the buyer's source and destination, the fee
/// account, the creator's USDC account (0 when the curve names none), `usdc_vault` and
/// `token_vault`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Holdings {
    source: u64,
    destination: u64,
    fee: u64,
    creator: u64,
    usdc_vault: u64,
    token_vault: u64,
}

fn holdings(env: &Env, curve: &CurveUsdc, source: &Pubkey, destination: &Pubkey) -> Holdings {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let creator = if curve.creator_usdc_account == Pubkey::default() {
        0
    } else {
        env.token_amount(&curve.creator_usdc_account)
    };
    Holdings {
        source: env.token_amount(source),
        destination: env.token_amount(destination),
        fee: env.token_amount(&config.fee_usdc_account),
        creator,
        usdc_vault: env.token_amount(&usdc_vault_pda(&curve.mint)),
        token_vault: env.token_amount(&ata(&curve_pda(&curve.mint), &curve.mint)),
    }
}

/// The log line `E` is first emitted on, so a test can pin the order of two events.
fn first_line_of<E: anchor_lang::Event>(logs: &[String]) -> Option<usize> {
    (0..logs.len()).find(|&i| !events_of::<E>(&logs[i..=i]).is_empty())
}

/// The fields of a `TradeUsdc`, in its order, for one comparison.
fn trade_fields(t: &TradeUsdc) -> (Pubkey, Pubkey, Pubkey, bool, [u64; 7]) {
    (
        t.mint,
        t.authority,
        t.token_owner,
        t.is_buy,
        [t.usdc_amount, t.token_amount, t.creator_fee, t.fanex_fee, t.platform_fee, t.real_usdc_reserves, t.real_token_reserves],
    )
}

/// Asserts that a buy of `quote` landed exactly: each party moved by the quote's figure, the curve
/// holds the quote's counters and its phase, and the logs carry its `TradeUsdc` and, when it
/// graduates, one `GraduationReadyUsdc`.
fn assert_bought(
    env: &Env,
    before: (&CurveUsdc, Holdings),
    after: Holdings,
    quote: &BuyQuote,
    logs: &[String],
    authority: &Pubkey,
    token_owner: &Pubkey,
) {
    let (start, held) = before;
    let what = format!("{:?}", quote);
    assert_eq!(held.source - after.source, quote.charged, "the source pays what is charged: {what}");
    assert_eq!(after.destination - held.destination, quote.tokens_out, "{what}");
    assert_eq!(after.fee - held.fee, quote.fanex_amount + quote.platform_fee, "{what}");
    assert_eq!(after.creator - held.creator, quote.creator_amount, "{what}");
    assert_eq!(after.usdc_vault - held.usdc_vault, quote.curve_amount, "{what}");
    assert_eq!(held.token_vault - after.token_vault, quote.tokens_out, "{what}");

    let curve = curve_now(env, &start.mint);
    assert_eq!(curve.swap_state(), quote.after, "{what}");
    let mut expected = start.clone();
    expected.set_swap_counters(&quote.after);
    expected.phase = if quote.graduates { PHASE_GRADUATING } else { PHASE_BONDING };
    let mut want = Vec::new();
    expected.try_serialize(&mut want).unwrap();
    assert_eq!(&curve_bytes(env, &start.mint)[..want.len()], &want[..], "no other byte of the curve moves: {what}");

    let ready = events_of::<GraduationReadyUsdc>(logs);
    let trades = events_of::<TradeUsdc>(logs);
    assert_eq!(trades.len(), 1, "{what}");
    if quote.graduates {
        assert_eq!(ready.len(), 1, "{what}");
        assert_eq!((ready[0].mint, ready[0].real_usdc_reserves), (start.mint, quote.after.real_reserve));
        // `buy`'s order: the graduation's event first, then the trade's.
        assert!(
            first_line_of::<GraduationReadyUsdc>(logs) < first_line_of::<TradeUsdc>(logs),
            "GraduationReadyUsdc before TradeUsdc: {what}"
        );
    } else {
        assert!(ready.is_empty(), "{what}");
    }
    assert_eq!(
        trade_fields(&trades[0]),
        (
            start.mint,
            *authority,
            *token_owner,
            true,
            [
                quote.charged,
                quote.tokens_out,
                quote.creator_amount,
                quote.fanex_amount,
                quote.platform_fee,
                quote.after.real_reserve,
                quote.after.real_token_reserves,
            ],
        ),
        "{what}"
    );
}

// ── The golden vectors, replayed (§10.9) ─────────────────────────────────────

#[test]
fn every_buy_vector_moves_exactly_what_the_crate_quotes() {
    let mut replayed = 0;
    let mut refused = 0;
    for v in vectors() {
        let VectorOp::Buy { amount, now } = v.op else { continue };
        let mut env = Env::new();
        env.write_usdc_config();
        // The case inside the launch window buys past the cap: its curve was launched with the
        // cap waived, and the buy passes the waiver, as the app's buy does.
        let windowed = in_launch_window(v.state.created_at, now);
        let curve = if windowed { write_waived_curve(&mut env, &v.state) } else { env.write_curve_usdc(&v.state) };
        env.set_time(now);
        assert_eq!(curve.swap_state(), v.state, "{}", v.name);

        let fund = match v.outcome {
            VectorOutcome::Bought(q) => q.charged + USDC,
            _ => amount.min(1_000 * USDC),
        };
        let b = buyer(&mut env, &curve.mint, fund);
        let mut buy = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, amount, 0);
        if windowed {
            buy = with_account(buy, WAIVER, AccountMeta::new_readonly(waiver_pda(&curve.mint), false));
        }
        let held = holdings(&env, &curve, &b.source, &b.destination);
        let raw = curve_bytes(&env, &curve.mint);

        match v.outcome {
            VectorOutcome::Bought(q) => {
                assert_eq!(q.launch_window, windowed, "{}", v.name);
                let logs = send(&mut env, &b.wallet, buy).unwrap_or_else(|e| panic!("{}: {e:?}", v.name));
                let after = holdings(&env, &curve, &b.source, &b.destination);
                assert_bought(&env, (&curve, held), after, &q, &logs, &b.wallet.pubkey(), &b.wallet.pubkey());
                // Only what is charged leaves: on a partial fill that is less than asked.
                assert_eq!(after.source, fund - q.charged, "{}", v.name);
                replayed += 1;
            }
            VectorOutcome::Refused(e) => {
                expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::from(e)));
                assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held, "{}", v.name);
                assert_eq!(curve_bytes(&env, &curve.mint), raw, "{}", v.name);
                refused += 1;
            }
            VectorOutcome::Sold(_) => unreachable!("{}: a buy's outcome", v.name),
        }
    }
    assert_eq!((replayed, refused), (13, 3), "the 16 buys of curve-v1.json");
}

#[test]
fn the_window_s_vector_is_refused_without_its_waiver() {
    // "the launch window's last second" buys 3.65M tokens, over the 1M cap.
    let v = vector("equity, fresh: the launch window's last second");
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    assert!(q.launch_window && q.tokens_out > LAUNCH_WALLET_CAP);
    let (mut env, curve) = curve_at(&v.state, now);
    let b = buyer(&mut env, &curve.mint, amount);
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let buy = own_buy(&env, &curve, &b, amount);
    expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::LaunchWalletCapExceeded));
    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);
}

// ── A router's shape (§10.5) ─────────────────────────────────────────────────

#[test]
fn a_router_buys_into_a_destination_it_does_not_own_once_the_window_is_over() {
    let v = vector(EQUITY_100);
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    // The router's program authority signs and pays from its own USDC; the tokens go to the user.
    let router = buyer(&mut env, &curve.mint, amount);
    let user = Keypair::new();
    let user_ata = open_ata(&mut env, &user.pubkey(), &curve.mint);

    let held = holdings(&env, &curve, &router.source, &user_ata);
    let buy = buy_ix(&env, &curve, &router.wallet.pubkey(), &router.source, &user_ata, amount, q.tokens_out);
    let logs = send(&mut env, &router.wallet, buy).unwrap();
    let after = holdings(&env, &curve, &router.source, &user_ata);
    // `authority` is the router's, `token_owner` the destination's owner: neither need be the user
    // of a routed trade (the indexer drops one whose authority does not own the destination).
    assert_bought(&env, (&curve, held), after, &q, &logs, &router.wallet.pubkey(), &user.pubkey());
    assert_eq!(env.token_amount(&router.destination), 0);

    // Any token account of the mint, an ATA or not: a router's intermediate one.
    let intermediate = Pubkey::new_unique();
    env.put_token_account(intermediate, curve.mint, router.wallet.pubkey(), 0, COption::None);
    env.mint_usdc(&router.source, amount);
    let now_curve = curve_now(&env, &curve.mint);
    let buy = buy_ix(&env, &now_curve, &router.wallet.pubkey(), &router.source, &intermediate, amount, 1);
    send(&mut env, &router.wallet, buy).unwrap();
    assert!(env.token_amount(&intermediate) > 0);
}

#[test]
fn a_wallet_with_no_sol_buys_while_another_key_pays_the_fee() {
    // The app's sponsored buy, and a router's program authority under its CPI: account 0 signs
    // read-only and pays nothing, the transaction's fee payer does (§10.3.5 as built).
    let v = vector(EQUITY_100);
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let wallet = Keypair::new();
    let source = env.usdc_account(&wallet.pubkey());
    env.mint_usdc(&source, amount);
    let destination = open_ata(&mut env, &wallet.pubkey(), &curve.mint);
    assert_eq!(env.lamports(&wallet.pubkey()), 0);

    let buy = buy_ix(&env, &curve, &wallet.pubkey(), &source, &destination, amount, q.tokens_out);
    assert!(buy.accounts[0].is_signer && !buy.accounts[0].is_writable);
    let held = holdings(&env, &curve, &source, &destination);
    // `send_logged`: the admin pays the fee, the wallet only signs.
    let logs = env.send_logged(&[buy], &[&wallet]).unwrap();
    let after = holdings(&env, &curve, &source, &destination);
    assert_bought(&env, (&curve, held), after, &q, &logs, &wallet.pubkey(), &wallet.pubkey());
    assert_eq!(env.lamports(&wallet.pubkey()), 0);
}

#[test]
fn the_source_must_belong_to_the_signer() {
    let v = vector(EQUITY_100);
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let owner = buyer(&mut env, &curve.mint, amount);
    let thief = buyer(&mut env, &curve.mint, 0);
    let held = holdings(&env, &curve, &owner.source, &thief.destination);
    let buy = buy_ix(&env, &curve, &thief.wallet.pubkey(), &owner.source, &thief.destination, amount, 0);
    expect_err(unit(send(&mut env, &thief.wallet, buy)), anchor(ErrorCode::ConstraintTokenOwner));
    assert_eq!(holdings(&env, &curve, &owner.source, &thief.destination), held);
}

// ── The launch window (§10.3.5 step 6, §5.2) ─────────────────────────────────

/// A fresh equity curve ten seconds into its launch window.
fn launched() -> (Env, CurveUsdc) {
    let state = CurveState { created_at: OCT_5_2026, ..vector(EQUITY_100).state };
    curve_at(&state, OCT_5_2026 + 10)
}

#[test]
fn in_the_window_a_buy_delivers_only_to_the_signer_s_own_ata() {
    let (mut env, curve) = launched();
    let b = buyer(&mut env, &curve.mint, 100 * USDC);
    let other = Keypair::new();
    let other_ata = open_ata(&mut env, &other.pubkey(), &curve.mint);
    let own_not_ata = Pubkey::new_unique();
    env.put_token_account(own_not_ata, curve.mint, b.wallet.pubkey(), 0, COption::None);

    for destination in [other_ata, own_not_ata] {
        // Refused before the slippage check: the window's rule comes first.
        let buy = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &destination, 10 * USDC, u64::MAX);
        expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::LaunchDestinationNotOwnAta));
    }
    let buy = own_buy(&env, &curve, &b, 10 * USDC);
    send(&mut env, &b.wallet, buy).unwrap();
    assert!(env.token_amount(&b.destination) > 0);
}

#[test]
fn in_the_window_the_holding_counts_toward_the_cap() {
    let (mut env, curve) = launched();
    let b = buyer(&mut env, &curve.mint, 100 * USDC);
    // 10 USDC buys about 0.38% of the supply: twice fits, a third crosses 1%.
    for _ in 0..2 {
        let now_curve = curve_now(&env, &curve.mint);
        let buy = own_buy(&env, &now_curve, &b, 10 * USDC);
        send(&mut env, &b.wallet, buy).unwrap();
    }
    let holding = env.token_amount(&b.destination);
    assert!(holding < LAUNCH_WALLET_CAP);
    let now_curve = curve_now(&env, &curve.mint);
    let third = quote_buy(&now_curve.swap_state(), 10 * USDC, OCT_5_2026 + 10).unwrap();
    assert!(holding + third.tokens_out > LAUNCH_WALLET_CAP);
    let buy = own_buy(&env, &now_curve, &b, 10 * USDC);
    expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::LaunchWalletCapExceeded));
    assert_eq!(env.token_amount(&b.destination), holding);
}

#[test]
fn the_curve_s_waiver_lifts_the_cap_and_nothing_else_does() {
    let mut env = Env::new();
    env.write_usdc_config();
    let state = CurveState { created_at: OCT_5_2026, ..vector(EQUITY_100).state };
    let curve = write_waived_curve(&mut env, &state);
    let elsewhere = write_waived_curve(&mut env, &state);
    env.set_time(OCT_5_2026 + 10);
    let b = buyer(&mut env, &curve.mint, 100 * USDC);
    let over = own_buy(&env, &curve, &b, 100 * USDC);

    // Not passed, another curve's, or a program account that is not a waiver: the cap applies.
    let cap = code(FanexError::LaunchWalletCapExceeded);
    expect_err(unit(send(&mut env, &b.wallet, over.clone())), cap);
    for stranger in [waiver_pda(&elsewhere.mint), curve_pda(&curve.mint), usdc_config_pda()] {
        let buy = with_account(over.clone(), WAIVER, AccountMeta::new_readonly(stranger, false));
        expect_err(unit(send(&mut env, &b.wallet, buy)), cap);
    }

    // The curve's own waiver lifts the cap, never the own-ATA rule.
    let other = Keypair::new();
    let other_ata = open_ata(&mut env, &other.pubkey(), &curve.mint);
    let elsewhere_bound = with_account(
        buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &other_ata, 100 * USDC, 0),
        WAIVER,
        AccountMeta::new_readonly(waiver_pda(&curve.mint), false),
    );
    expect_err(unit(send(&mut env, &b.wallet, elsewhere_bound)), code(FanexError::LaunchDestinationNotOwnAta));

    let waived = with_account(over, WAIVER, AccountMeta::new_readonly(waiver_pda(&curve.mint), false));
    send(&mut env, &b.wallet, waived).unwrap();
    assert!(env.token_amount(&b.destination) > LAUNCH_WALLET_CAP);
}

#[test]
fn from_the_window_s_last_second_on_there_is_no_cap_and_no_destination_rule() {
    let (mut env, curve) = launched();
    let b = buyer(&mut env, &curve.mint, 200 * USDC);
    env.set_time(OCT_5_2026 + LAUNCH_GUARD_SECONDS - 1);
    let over = own_buy(&env, &curve, &b, 100 * USDC);
    expect_err(unit(send(&mut env, &b.wallet, over.clone())), code(FanexError::LaunchWalletCapExceeded));

    env.set_time(OCT_5_2026 + LAUNCH_GUARD_SECONDS);
    send(&mut env, &b.wallet, over).unwrap();
    assert!(env.token_amount(&b.destination) > LAUNCH_WALLET_CAP);
    let other = Keypair::new();
    let other_ata = open_ata(&mut env, &other.pubkey(), &curve.mint);
    let now_curve = curve_now(&env, &curve.mint);
    let routed = buy_ix(&env, &now_curve, &b.wallet.pubkey(), &b.source, &other_ata, 100 * USDC, 0);
    send(&mut env, &b.wallet, routed).unwrap();
    assert!(env.token_amount(&other_ata) > LAUNCH_WALLET_CAP);
}

// ── The pauses and the phase (§10.3.3) ───────────────────────────────────────

fn pause_ix(signer: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcPaused { authority: *signer, usdc_config: usdc_config_pda() },
        pepper_curve::instruction::SetUsdcPaused { paused },
    )
}

fn curve_pause_ix(signer: &Pubkey, mint: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetCurveUsdcPaused { authority: *signer, usdc_config: usdc_config_pda(), curve: curve_pda(mint) },
        pepper_curve::instruction::SetCurveUsdcPaused { paused },
    )
}

/// A fresh equity curve past its window and a buyer with 300 USDC.
fn trading() -> (Env, CurveUsdc, Buyer) {
    let v = vector(EQUITY_100);
    let VectorOp::Buy { now, .. } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let b = buyer(&mut env, &curve.mint, 300 * USDC);
    let pauser = env.pauser.pubkey();
    env.svm.airdrop(&pauser, SOL).unwrap();
    (env, curve, b)
}

#[test]
fn the_usdc_kill_switch_stops_the_buy_until_the_admin_unpauses() {
    let (mut env, curve, b) = trading();
    let pauser = env.pauser.insecure_clone();
    send(&mut env, &pauser, pause_ix(&pauser.pubkey(), true)).unwrap();
    let buy = own_buy(&env, &curve, &b, 100 * USDC);
    expect_err(unit(send(&mut env, &b.wallet, buy.clone())), code(FanexError::GloballyPaused));

    let admin = env.admin.insecure_clone();
    send(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();
    send(&mut env, &b.wallet, buy).unwrap();
}

#[test]
fn a_curve_s_pause_stops_its_buys_and_no_other_curve_s() {
    let (mut env, curve, b) = trading();
    let other = env.write_curve_usdc(&curve.swap_state());
    let pauser = env.pauser.insecure_clone();
    send(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &curve.mint, true)).unwrap();
    let buy = own_buy(&env, &curve, &b, 100 * USDC);
    expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::CurvePaused));

    let c = buyer(&mut env, &other.mint, 100 * USDC);
    let buy = own_buy(&env, &other, &c, 100 * USDC);
    send(&mut env, &c.wallet, buy).unwrap();
}

#[test]
fn the_sol_pause_leaves_the_usdc_buy_running() {
    let (mut env, curve, b) = trading();
    env.set_paused(true);
    let buy = own_buy(&env, &curve, &b, 100 * USDC);
    send(&mut env, &b.wallet, buy).unwrap();
}

#[test]
fn a_curve_out_of_bonding_takes_no_buy() {
    let (mut env, curve, b) = trading();
    for phase in [PHASE_GRADUATING, PHASE_GRADUATED, PHASE_POOL_CREATED] {
        let mut moved = curve.clone();
        moved.phase = phase;
        env.put(curve_pda(&curve.mint), account_data(&moved, <CurveUsdc as anchor_lang::Space>::INIT_SPACE), pepper_curve::ID);
        let buy = own_buy(&env, &moved, &b, 100 * USDC);
        expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::NotBonding));
    }
}

// ── The counters, never the balances (§10.2) ─────────────────────────────────

#[test]
fn usdc_sent_straight_to_the_vault_moves_no_price() {
    let v = vector(EQUITY_100);
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let usdc_vault = usdc_vault_pda(&curve.mint);
    env.mint_usdc(&usdc_vault, 1_000 * USDC);
    assert_eq!(curve_now(&env, &curve.mint).real_usdc_reserves, 0, "the donation counts for nothing");

    let b = buyer(&mut env, &curve.mint, amount);
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let buy = own_buy(&env, &curve, &b, amount);
    let logs = send(&mut env, &b.wallet, buy).unwrap();
    let after = holdings(&env, &curve, &b.source, &b.destination);
    // The vector's own quote, to the base unit: the donation changed neither price nor counters.
    assert_bought(&env, (&curve, held), after, &q, &logs, &b.wallet.pubkey(), &b.wallet.pubkey());
    assert_eq!(env.token_amount(&usdc_vault), 1_000 * USDC + q.curve_amount);
    assert_eq!(curve_now(&env, &curve.mint).real_usdc_reserves, q.curve_amount);
}

// ── Account 8, the creator's USDC account (§10.3.5) ─────────────────────────

#[test]
fn account_8_is_required_while_the_creator_s_cap_is_open_even_for_a_share_of_zero() {
    let v = vector(REVENUE_FRESH);
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    assert!(creator_usdc_account_required(curve.creator_split_bps, curve.creator_received_usdc, curve.creator_cap_usdc));
    let b = buyer(&mut env, &curve.mint, amount);

    // 1 µUSDC rounds the creator's share to 0, yet the cap is open: the counters decide.
    let tiny = quote_buy(&curve.swap_state(), 1, now).unwrap();
    assert_eq!((tiny.creator_amount, tiny.fanex_amount, tiny.platform_fee), (0, 0, 0));
    for usdc_in in [amount, 1] {
        let buy = with_account(own_buy(&env, &curve, &b, usdc_in), CREATOR_ACCOUNT, absent());
        expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::CreatorUsdcAccountInvalid));
    }
    // Another account in its place is refused by the curve's record.
    let buy = with_account(own_buy(&env, &curve, &b, amount), CREATOR_ACCOUNT, AccountMeta::new(b.source, false));
    expect_err(unit(send(&mut env, &b.wallet, buy)), code(FanexError::AccountMismatch));

    // With it, the tiny buy moves the curve's 1 µUSDC alone: the zero shares are skipped.
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let buy = own_buy(&env, &curve, &b, 1);
    let logs = send(&mut env, &b.wallet, buy).unwrap();
    let after = holdings(&env, &curve, &b.source, &b.destination);
    assert_bought(&env, (&curve, held), after, &tiny, &logs, &b.wallet.pubkey(), &b.wallet.pubkey());
    assert_eq!((after.creator, after.fee, after.usdc_vault), (held.creator, held.fee, held.usdc_vault + 1));
}

#[test]
fn once_the_creator_s_cap_is_full_account_8_may_be_absent_closed_or_frozen() {
    let v = vector(REVENUE_CAPS_FULL);
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    assert_eq!(q.creator_amount, 0);
    let (mut env, curve) = curve_at(&v.state, now);
    assert!(!creator_usdc_account_required(curve.creator_split_bps, curve.creator_received_usdc, curve.creator_cap_usdc));
    let b = buyer(&mut env, &curve.mint, 3 * amount);

    // Absent, as the router's list leaves it.
    let first = own_buy(&env, &curve, &b, amount);
    assert_eq!(first.accounts[CREATOR_ACCOUNT].pubkey, pepper_curve::ID);
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let logs = send(&mut env, &b.wallet, first).unwrap();
    let after = holdings(&env, &curve, &b.source, &b.destination);
    assert_bought(&env, (&curve, held), after, &q, &logs, &b.wallet.pubkey(), &b.wallet.pubkey());

    // Frozen and passed: its share is 0, so nothing is sent to it.
    env.freeze_usdc(&curve.creator_usdc_account);
    let now_curve = curve_now(&env, &curve.mint);
    let passed = with_account(
        own_buy(&env, &now_curve, &b, amount),
        CREATOR_ACCOUNT,
        AccountMeta::new(curve.creator_usdc_account, false),
    );
    send(&mut env, &b.wallet, passed).unwrap();
    // Still bound to the curve's record when passed.
    let now_curve = curve_now(&env, &curve.mint);
    let wrong = with_account(own_buy(&env, &now_curve, &b, amount), CREATOR_ACCOUNT, AccountMeta::new(b.source, false));
    expect_err(unit(send(&mut env, &b.wallet, wrong)), code(FanexError::AccountMismatch));
}

#[test]
fn a_frozen_creator_account_stops_the_buys_only_while_the_cap_is_open() {
    let v = vector(REVENUE_FRESH);
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let b = buyer(&mut env, &curve.mint, amount);
    env.freeze_usdc(&curve.creator_usdc_account);
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let buy = own_buy(&env, &curve, &b, amount);
    expect_err(unit(send(&mut env, &b.wallet, buy)), token(TokenError::AccountFrozen));
    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);
}

fn close_raise_ix(admin: &Pubkey, mint: &Pubkey) -> Instruction {
    ix(
        pepper_curve::accounts::CloseCreatorRaiseUsdc { admin: *admin, usdc_config: usdc_config_pda(), curve: curve_pda(mint) },
        pepper_curve::instruction::CloseCreatorRaiseUsdc {},
    )
}

#[test]
fn the_admin_closes_a_raise_whose_creator_account_is_frozen_and_the_buys_go_on() {
    // The internal audit (2026-10-29): account 8 is the creator's own ATA, the one address a launch
    // names, so it cannot be pointed elsewhere; frozen, every buy owing the creator a share failed
    // until the cap filled, which it never could. The admin closes the raise at what it received.
    let v = vector(REVENUE_FRESH);
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let b = buyer(&mut env, &curve.mint, 2 * amount);
    env.freeze_usdc(&curve.creator_usdc_account);
    let buy = own_buy(&env, &curve, &b, amount);
    expect_err(unit(send(&mut env, &b.wallet, buy)), token(TokenError::AccountFrozen));

    // Neither a stranger nor the pauser closes it.
    let stranger = env.funded_wallet(SOL);
    let pauser = env.pauser.insecure_clone();
    env.svm.airdrop(&pauser.pubkey(), SOL).unwrap();
    for signer in [&stranger, &pauser] {
        let close = close_raise_ix(&signer.pubkey(), &curve.mint);
        expect_err(unit(send(&mut env, signer, close)), code(FanexError::Unauthorized));
    }
    let admin = env.admin.insecure_clone();
    let logs = send(&mut env, &admin, close_raise_ix(&admin.pubkey(), &curve.mint)).unwrap();
    let closed = curve_now(&env, &curve.mint);
    assert_eq!(closed.creator_cap_usdc, closed.creator_received_usdc);
    assert_eq!(closed.creator_usdc_account, Pubkey::default());
    // Nothing else moved: the target, the splits, Fanex's raise, the counters.
    let mut expected = curve.clone();
    expected.creator_cap_usdc = curve.creator_received_usdc;
    expected.creator_usdc_account = Pubkey::default();
    assert_eq!(curve_bytes(&env, &curve.mint), account_data(&expected, CurveUsdc::INIT_SPACE));
    let event: pepper_curve::events::CreatorRaiseClosedUsdc = event(&logs);
    assert_eq!((event.mint, event.creator_cap_usdc, event.by), (curve.mint, closed.creator_cap_usdc, admin.pubkey()));

    // The buy lands as a router builds it, account 8 absent, the creator's share now the curve's.
    let buy = own_buy(&env, &closed, &b, amount);
    assert_eq!(buy.accounts[CREATOR_ACCOUNT].pubkey, pepper_curve::ID);
    send(&mut env, &b.wallet, buy).unwrap();
    let after = curve_now(&env, &curve.mint);
    assert_eq!(after.creator_received_usdc, curve.creator_received_usdc);
    assert!(after.fanex_received_usdc > curve.fanex_received_usdc);
}

#[test]
fn a_raise_closes_only_in_bonding() {
    let v = vector(REVENUE_FRESH);
    let VectorOp::Buy { now, .. } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&v.state, now);
    let mut graduating = curve.clone();
    graduating.phase = PHASE_GRADUATING;
    env.put_curve_usdc(&graduating);
    let admin = env.admin.insecure_clone();
    expect_err(unit(send(&mut env, &admin, close_raise_ix(&admin.pubkey(), &curve.mint))), code(FanexError::NotBonding));
    assert_eq!(curve_now(&env, &curve.mint).creator_cap_usdc, curve.creator_cap_usdc);
}

// ── The fee account, the slippage and a zero delivery ────────────────────────

#[test]
fn a_frozen_fee_account_fails_the_buy() {
    let (mut env, curve, b) = trading();
    let fee = env.fetch::<UsdcConfig>(&usdc_config_pda()).fee_usdc_account;
    env.freeze_usdc(&fee);
    let held = holdings(&env, &curve, &b.source, &b.destination);
    let buy = own_buy(&env, &curve, &b, 100 * USDC);
    expect_err(unit(send(&mut env, &b.wallet, buy.clone())), token(TokenError::AccountFrozen));
    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);
    env.thaw_usdc(&fee);
    send(&mut env, &b.wallet, buy).unwrap();
}

#[test]
fn the_fee_account_is_the_config_s() {
    let (mut env, curve, b) = trading();
    let buy = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, 100 * USDC, 0);
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let fee_position = buy.accounts.iter().position(|m| m.pubkey == config.fee_usdc_account).unwrap();
    assert_eq!(fee_position, 7);
    let elsewhere = with_account(buy, 7, AccountMeta::new(b.source, false));
    expect_err(unit(send(&mut env, &b.wallet, elsewhere)), code(FanexError::AccountMismatch));
}

/// The curve's vesting vault, `["vesting_vault", mint]`: a token account of the mint whose authority
/// is the curve PDA, as `token_vault`'s and `usdc_vault`'s are.
fn vesting_vault_pda(mint: &Pubkey) -> Pubkey {
    pda(&[VESTING_VAULT_SEED, mint.as_ref()])
}

#[test]
fn a_buy_moves_through_the_curve_s_own_vaults_alone() {
    let (mut env, curve, b) = trading();
    // The curve PDA signs for its vesting vault too: in `token_vault`'s place it would pay a buy's
    // tokens out of the creator's vesting while the counters say they left the curve's supply.
    let vesting_vault = vesting_vault_pda(&curve.mint);
    env.put_token_account(vesting_vault, curve.mint, curve_pda(&curve.mint), 2_000_000 * 1_000_000, COption::None);
    // And a USDC account of the buyer's own in `usdc_vault`'s place would take back the curve's
    // amount while the counters say it reached the reserve.
    let pocket = Pubkey::new_unique();
    env.put_token_account(pocket, env.usdc.mint, b.wallet.pubkey(), 0, COption::None);

    let held = holdings(&env, &curve, &b.source, &b.destination);
    let raw = curve_bytes(&env, &curve.mint);
    // 10 USDC buys less than the vesting vault holds, so without the ATA rule it would pay.
    let buy = own_buy(&env, &curve, &b, 10 * USDC);
    let delivered = quote_buy(&curve.swap_state(), 10 * USDC, OCT_5_2026).unwrap().tokens_out;
    assert!(delivered > 0 && delivered < 2_000_000 * 1_000_000);
    let stand_ins = [
        (5, AccountMeta::new(pocket, false), anchor(ErrorCode::ConstraintSeeds)),
        (6, AccountMeta::new(vesting_vault, false), anchor(ErrorCode::ConstraintAssociated)),
    ];
    for (position, meta, expected) in stand_ins {
        let forged = with_account(buy.clone(), position, meta);
        expect_err(unit(send(&mut env, &b.wallet, forged)), expected);
    }
    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);
    assert_eq!(env.token_amount(&vesting_vault), 2_000_000 * 1_000_000);
    assert_eq!(env.token_amount(&pocket), 0);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
}

#[test]
fn a_buy_takes_the_curve_s_own_token_and_the_config_s_usdc() {
    let (mut env, curve, b) = trading();
    let address = curve_pda(&curve.mint);
    // A worthless mint of a stranger's, with the curve PDA's associated account of it (anyone may
    // open one) filled by its maker. With `token_mint` unbound, the buy would charge real USDC for
    // its tokens, count them as gone from this curve's supply, and leave the curve's own in
    // `token_vault`.
    let worthless = Pubkey::new_unique();
    env.put_launched_mint(worthless);
    let curve_worthless = ata(&address, &worthless);
    env.put_token_account(curve_worthless, worthless, address, TOKEN_TOTAL_SUPPLY, COption::None);
    let buyer_worthless = ata(&b.wallet.pubkey(), &worthless);
    env.put_token_account(buyer_worthless, worthless, b.wallet.pubkey(), 0, COption::None);
    // And a USDC look-alike, six decimals too, for the buyer to pay with: SPL Token would refuse it
    // against the vault's and the fee account's mint (3), and the program refuses it first.
    let look_alike = Pubkey::new_unique();
    env.put_launched_mint(look_alike);
    let look_alike_source = Pubkey::new_unique();
    env.put_token_account(look_alike_source, look_alike, b.wallet.pubkey(), 300 * USDC, COption::None);

    let held = holdings(&env, &curve, &b.source, &b.destination);
    let raw = curve_bytes(&env, &curve.mint);
    let buy = own_buy(&env, &curve, &b, 10 * USDC);
    let worthless_buy = [
        (2, AccountMeta::new(buyer_worthless, false)),
        (6, AccountMeta::new(curve_worthless, false)),
        (10, AccountMeta::new_readonly(worthless, false)),
    ]
    .into_iter()
    .fold(buy.clone(), |ix, (position, meta)| with_account(ix, position, meta));
    expect_err(unit(send(&mut env, &b.wallet, worthless_buy)), code(FanexError::AccountMismatch));
    let look_alike_buy = [(1, AccountMeta::new(look_alike_source, false)), (9, AccountMeta::new_readonly(look_alike, false))]
        .into_iter()
        .fold(buy.clone(), |ix, (position, meta)| with_account(ix, position, meta));
    expect_err(unit(send(&mut env, &b.wallet, look_alike_buy)), code(FanexError::AccountMismatch));

    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);
    assert_eq!(env.token_amount(&curve_worthless), TOKEN_TOTAL_SUPPLY);
    assert_eq!(env.token_amount(&buyer_worthless), 0);
    assert_eq!(env.token_amount(&look_alike_source), 300 * USDC);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
    // The same buy with the curve's own accounts lands.
    send(&mut env, &b.wallet, buy).unwrap();
}

#[test]
fn the_slippage_is_checked_before_a_zero_delivery() {
    // A curve whose next token costs more than 100 µUSDC: a small buy delivers 0.
    let state = CurveState {
        virtual_reserve: 1_000_000 * USDC,
        virtual_token_reserves: 1_000_000,
        real_reserve: 0,
        real_token_reserves: 1_000_000,
        ..vector(EQUITY_100).state
    };
    let v = vector(EQUITY_100);
    let VectorOp::Buy { now, .. } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_at(&state, now);
    let b = buyer(&mut env, &curve.mint, USDC);
    let zero = quote_buy(&curve.swap_state(), 100, now).unwrap();
    assert_eq!(zero.tokens_out, 0, "the crate does not refuse it");

    let held = holdings(&env, &curve, &b.source, &b.destination);
    let asking = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, 100, 1);
    expect_err(unit(send(&mut env, &b.wallet, asking)), code(FanexError::SlippageExceeded));
    let any = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, 100, 0);
    expect_err(unit(send(&mut env, &b.wallet, any)), code(FanexError::ZeroAmount));
    assert_eq!(holdings(&env, &curve, &b.source, &b.destination), held);

    // And a buy that delivers, asked for one token more than it gets.
    let (mut env, curve, b) = trading();
    let q = quote_buy(&curve.swap_state(), 100 * USDC, now).unwrap();
    let greedy = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, 100 * USDC, q.tokens_out + 1);
    expect_err(unit(send(&mut env, &b.wallet, greedy)), code(FanexError::SlippageExceeded));
    let exact = buy_ix(&env, &curve, &b.wallet.pubkey(), &b.source, &b.destination, 100 * USDC, q.tokens_out);
    send(&mut env, &b.wallet, exact).unwrap();
}

// ── The two kinds of curve apart (§10.1) ─────────────────────────────────────

#[test]
fn the_sol_buy_refuses_a_usdc_curve() {
    let (mut env, curve, b) = trading();
    let address = curve_pda(&curve.mint);
    let sol_buy = ix(
        pepper_curve::accounts::Buy {
            buyer: b.wallet.pubkey(),
            config: config_pda(),
            curve: address,
            mint: curve.mint,
            sol_vault: pda(&[SOL_VAULT_SEED, curve.mint.as_ref()]),
            token_vault: ata(&address, &curve.mint),
            buyer_token_account: b.destination,
            creator: curve.creator,
            fanex: env.fee_wallet.pubkey(),
            token_program: spl_token::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::Buy { sol_in: SOL / 20, min_tokens_out: 0 },
    );
    expect_err(unit(send(&mut env, &b.wallet, sol_buy)), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

#[test]
fn buy_exact_in_refuses_a_sol_curve() {
    let mut env = Env::new();
    env.write_usdc_config();
    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let sol_mint = env.mint;
    // A `CurveUsdc` shaped like it, never written: the instruction names the SOL curve's address.
    let shape = env.curve_usdc(sol_mint, &vector(EQUITY_100).state);
    let b = buyer(&mut env, &sol_mint, 100 * USDC);
    let buy = own_buy(&env, &shape, &b, 100 * USDC);
    assert_eq!(buy.accounts[4].pubkey, curve_pda(&sol_mint));
    expect_err(unit(send(&mut env, &b.wallet, buy)), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

// ── What a revenue buy costs (§10.5: Jupiter's routes budget compute) ────────

#[test]
fn a_revenue_buy_with_all_four_transfers_fits_a_router_s_budget() {
    let v = vector(REVENUE_FRESH);
    let (VectorOp::Buy { amount, now }, VectorOutcome::Bought(q)) = (v.op, v.outcome) else { panic!("a buy") };
    assert!(q.creator_amount > 0 && q.fanex_amount > 0 && q.platform_fee > 0);
    let (mut env, curve) = curve_at(&v.state, now);
    let b = buyer(&mut env, &curve.mint, amount);
    let buy = own_buy(&env, &curve, &b, amount);
    let tx = Transaction::new_signed_with_payer(&[buy], Some(&b.wallet.pubkey()), &[&b.wallet], env.svm.latest_blockhash());
    let meta = env.svm.send_transaction(tx).expect("the buy lands");
    let units = meta.compute_units_consumed;
    println!("buy_exact_in with four transfers: {units} compute units");
    assert!(units < 100_000, "{units} compute units");
}
