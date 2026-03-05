use alloy::primitives::{B256, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use rust_decimal::Decimal;
use tracing::{debug, info};

use polymarket_client_sdk::ctf;
use polymarket_client_sdk::ctf::types::MergePositionsRequest;
use polymarket_client_sdk::POLYGON;

/// USDC token address on Polygon mainnet
const POLYGON_USDC: alloy::primitives::Address =
    alloy::primitives::address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

/// Provider type produced by `ProviderBuilder::new().wallet(EthereumWallet).connect_http(url)`.
type WalletProvider = alloy::providers::fillers::FillProvider<
    alloy::providers::fillers::JoinFill<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::fillers::WalletFiller<alloy::network::EthereumWallet>,
    >,
    alloy::providers::RootProvider,
>;

/// CTF merge executor for on-chain merge operations.
/// Wraps the SDK's CTF client with an authenticated wallet provider.
pub struct CtfMerger {
    client: ctf::Client<WalletProvider>,
}

impl CtfMerger {
    /// Create a CTF merger connected to Polygon via the given RPC URL.
    /// Uses the provided signer for transaction signing.
    pub async fn new(polygon_rpc_url: &str, signer: PrivateKeySigner) -> Result<Self> {
        let provider = ProviderBuilder::new()
            .wallet(alloy::network::EthereumWallet::new(signer))
            .connect_http(polygon_rpc_url.parse().context("Invalid polygon RPC URL")?);

        let client = ctf::Client::new(provider, POLYGON)
            .context("Failed to create CTF client for Polygon")?;

        info!("CTF merger initialized for Polygon mainnet");
        Ok(Self { client })
    }

    /// Execute a merge operation: combine equal YES + NO tokens into USDC.
    ///
    /// # Arguments
    /// * `condition_id` - The market's condition ID (from Gamma API)
    /// * `amount` - Number of token pairs to merge (in raw units, 1e6 = 1 USDC)
    ///
    /// # Returns
    /// Transaction hash on success.
    pub async fn merge_positions(
        &self,
        condition_id: B256,
        amount: Decimal,
    ) -> Result<B256> {
        // R9-CR6: Safety cap to prevent runaway merge from position/config bugs
        const MAX_MERGE_AMOUNT: Decimal = Decimal::from_parts(100_000, 0, 0, false, 0);
        anyhow::ensure!(
            amount <= MAX_MERGE_AMOUNT,
            "Merge amount {amount} exceeds safety cap of {MAX_MERGE_AMOUNT} USDC"
        );

        // Convert Decimal to U256 (USDC has 6 decimals)
        let amount_raw = decimal_to_u256_6dp(amount)?;

        if amount_raw.is_zero() {
            anyhow::bail!("Merge amount is zero after conversion");
        }

        debug!(
            "Executing merge: condition_id={condition_id}, amount={amount} ({amount_raw} raw)"
        );

        let request = MergePositionsRequest::for_binary_market(
            POLYGON_USDC,
            condition_id,
            amount_raw,
        );

        let response = self.client.merge_positions(&request).await
            .context("CTF mergePositions transaction failed")?;

        info!(
            "Merge succeeded: tx={}, block={}, amount={amount}",
            response.transaction_hash, response.block_number
        );

        Ok(response.transaction_hash)
    }
}

/// Convert a Decimal amount to U256 with 6 decimal places (USDC precision).
/// E.g., Decimal("150.50") → U256(150_500_000)
fn decimal_to_u256_6dp(amount: Decimal) -> Result<U256> {
    use rust_decimal_macros::dec;

    if amount < Decimal::ZERO {
        anyhow::bail!("Merge amount cannot be negative: {amount}");
    }

    let scaled = amount * dec!(1_000_000);
    let truncated = scaled.trunc();

    // Convert to u128 first (Decimal supports this), then to U256
    let as_u128: u128 = truncated.try_into()
        .context("Merge amount too large for U256 conversion")?;

    Ok(U256::from(as_u128))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_decimal_to_u256_basic() {
        let result = decimal_to_u256_6dp(dec!(100.0)).unwrap();
        assert_eq!(result, U256::from(100_000_000u64));
    }

    #[test]
    fn test_decimal_to_u256_fractional() {
        let result = decimal_to_u256_6dp(dec!(150.50)).unwrap();
        assert_eq!(result, U256::from(150_500_000u64));
    }

    #[test]
    fn test_decimal_to_u256_zero() {
        let result = decimal_to_u256_6dp(dec!(0)).unwrap();
        assert_eq!(result, U256::ZERO);
    }

    #[test]
    fn test_decimal_to_u256_negative() {
        let result = decimal_to_u256_6dp(dec!(-10));
        assert!(result.is_err());
    }
}
