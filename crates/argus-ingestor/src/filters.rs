//! Log filter construction for Morpho Blue + Euler v2.

use alloy_primitives::Address;
use alloy_rpc_types::{Filter, Topic};

/// Build a combined filter for all protocol contracts of interest.
/// Topic0 is left open so we decode by signature inside the decoder.
pub fn build_protocol_filter(
    morpho: &[Address],
    euler: &[Address],
) -> Filter {
    let mut addresses = Vec::with_capacity(morpho.len() + euler.len());
    addresses.extend_from_slice(morpho);
    addresses.extend_from_slice(euler);

    Filter::new().address(addresses)
}

/// Helper: single-address filter with specific topic0 (event signature).
pub fn topic_filter(address: Address, topic0: alloy_primitives::B256) -> Filter {
    Filter::new()
        .address(address)
        .event_signature(Topic::from(topic0))
}
