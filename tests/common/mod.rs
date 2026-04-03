//! Shared helpers for integration tests in this crate.

/// Solana JSON-RPC URL for tests.
///
/// Set `SOLANA_RPC_URL` to your endpoint (e.g. Helius, QuickNode). If unset, uses public mainnet-beta
/// (rate-limited; fine for local `cargo test`).
pub fn solana_rpc_url() -> String {
    std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string())
}
