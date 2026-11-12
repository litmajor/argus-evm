# argus-evm

**High-performance, event-driven EVM liquidation & oracle-lag arbitrage runtime.**

Forked from the [Argus](https://github.com/litmajor/argus) **Surface-State Architecture (SSA)** — same core loop (continuous state streams → domain policy evaluation → deterministic actions + JSONL telemetry), re-targeted at Morpho Blue / Euler v2 liquidations.

---

## Status — what works today

| Stage | Status | Notes |
|-------|--------|--------|
| **Alloy WS ingest** | ✅ | Real `subscribe_blocks` + `subscribe_logs` via Alloy `RootProvider` |
| **Domain events** | ✅ | Morpho Blue + Euler v2 `sol!` decoders → `DomainEvent` broadcast |
| **In-memory state** | ✅ | Markets, positions, oracles in `DashMap`; HF recompute on oracle/borrow |
| **Morpho share math** | ✅ | Exact `SharesMathLib` (`VIRTUAL_SHARES=1e6`, `toAssetsUp/Down`) |
| **Morpho LIF curve** | ✅ | `LIF = min(1.15, 1/(β·LLTV+(1−β)))`, β=0.3; **no close factor** (up to 100% debt) |
| **revm simulation** | ✅ | Live Alloy prefetch → warm `CacheDB` + full `Evm::transact` (zero RPC on hot path) |
| **VaultLiquidator.sol** | ✅ | Foundry skeleton: `executeLiquidation` → flash-loan → liquidate → profit gate |
| **Private bundle submit** | ✅ | Sign EIP-1559 + `eth_sendBundle` / `eth_sendPrivateTransaction` / private RPC |
| **JSONL telemetry** | ✅ | Block / candidate / sim / exec ticks |
| Receipt polling | ✅ | `eth_getTransactionReceipt` with timeout |
| Storage-slot AlloyDB / warm CacheDB | ✅ | Prefetch slots + AlloyDB lazy recipe |
| Exact Morpho `liquidate` calldata | ✅ | `MarketParams` + seize/shares encoding |

---

## SSA → EVM mapping

| SSA Layer | Original Argus | argus-evm |
|-----------|----------------|-----------|
| **State** | `core/` process models | `argus-types` — `Market`, `Position`, `HealthFactor`, `LiquidationCandidate` |
| **Collectors** | OS process/CPU | `argus-ingestor` — Alloy WS `newHeads` + Morpho/Euler logs |
| **Domain engines** | rule / threat analysis | `argus-state` — mirror + exact Morpho HF/LIF |
| **Actions** | domain events | `DomainEvent` + `LiquidationCandidate` over channels |
| **Surfaces** | TUI / console | `argus-sim` (revm) + `argus-executor` (private bundles) |
| **Runtime** | event bus + main | `argus-runtime` — Tokio orchestration |

---

## Workspace layout

```
argus-evm/
├── Cargo.toml
├── ARCHITECTURE.md
├── crates/
│   ├── argus-types/        # Shared domain language (SSA State Layer)
│   ├── argus-ingestor/     # Alloy WS → DomainEvent broadcast
│   ├── argus-state/        # In-memory mirror + HF/LIF evaluation
│   ├── argus-sim/          # revm CacheDB + Evm::transact
│   ├── argus-executor/     # Sign + private bundle / private tx
│   └── argus-runtime/      # Binary — wires the pipeline
└── contracts/
    ├── foundry.toml
    └── src/VaultLiquidator.sol
```

---

## Data flow

```
Alloy WS (newHeads + Morpho/Euler logs)
        │
        ▼  broadcast<DomainEvent>
argus-ingestor
        │
        ▼
argus-state  (DashMap mirror, exact Morpho share math + LIF)
        │  mpsc<LiquidationCandidate>   debt_to_cover = 100% debt
        ▼
argus-sim    (CacheDB + Evm::transact → SimulationResult)
        │  mpsc<SimulationResult>
        ▼
argus-executor
        ├─ sign EIP-1559
        ├─ eth_sendBundle / eth_sendPrivateTransaction / private RPC
        └─ JSONL telemetry
```

All stages are independent Tokio tasks. Bounded mpsc applies back-pressure; broadcast fans out raw domain events.

---

## Morpho math (reference)

**Shares** (`SharesMathLib`):
```
toAssetsUp(shares)   = shares × (totalAssets + 1) / (totalShares + 1e6)   # ceil — debt
toAssetsDown(shares) = shares × (totalAssets + 1) / (totalShares + 1e6)   # floor — supply
```

**Liquidation Incentive Factor**:
```
LIF = min(1.15, 1 / (0.3 × LLTV + 0.7))
```
No protocol close factor — liquidator may repay any amount up to full debt.

**Health factor**:
```
HF = (collateral_value × LLTV) / borrow_value     # liquidatable when HF < 1.0
```

---

## Quick start

```bash
# Prerequisites: Rust 1.80+, Foundry (optional)

cd argus-evm
cargo build -p argus-runtime

# Run
export ARGUS_WS_URL=wss://eth-mainnet.g.alchemy.com/v2/YOUR_KEY
export ARGUS_PRIVATE_KEY=0x...          # executor signer
export ARGUS_PRIVATE_RPC=https://relay.flashbots.net
export RUST_LOG=info

cargo run -p argus-runtime -- --chain-id 1
```

### Environment

| Variable | Description |
|----------|-------------|
| `ARGUS_WS_URL` | WebSocket RPC (ingest) |
| `ARGUS_CHAIN_ID` | Chain id (default 1) |
| `ARGUS_PRIVATE_KEY` | Hex private key for signing liquidations |
| `ARGUS_PRIVATE_RPC` | Builder / private RPC URL |
| `ARGUS_TELEMETRY` | JSONL output path |
| `RUST_LOG` | Tracing filter |

HTTP fork URL for revm is derived automatically (`wss://` → `https://`).

---

## Contracts

```bash
cd contracts
forge build
# forge create src/VaultLiquidator.sol:VaultLiquidator --rpc-url $ETH_RPC_URL --private-key $PK
```

Entry point (ABI locked with `argus-sim`):

```solidity
function executeLiquidation(
    address flashLender,
    address borrowToken,
    uint256 borrowAmount,
    address targetVault,
    bytes calldata liquidationData,
    uint256 minNetProfit
) external onlyOwner;
```

Strict `InsufficientProfit` revert if residual < `minNetProfit`.

---

## Executor modes

| Mode | RPC method | Use case |
|------|------------|----------|
| `FlashbotsBundle` (default) | `eth_sendBundle` | Builder relays (Flashbots, Titan, …) |
| `PrivateTx` | `eth_sendPrivateTransaction` | Protect-style private inclusion |
| `PrivateRpc` | `eth_sendRawTransaction` | Any private sequencer / RPC |

---

## Extending

1. **New protocol** → `sol!` events in `argus-ingestor` + filter addresses
2. **Live AlloyDB sim** → `WrapDatabaseAsync(AlloyDB::new(provider, block))` in `argus-sim`
3. **Exact Morpho calldata** → encode `marketId, borrower, seizedAssets, repaidShares` into `liquidationData`
4. **Multi-tx bundles** → approve + liquidate + swap as separate signed txs in `eth_sendBundle`

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full SSA mapping and latency budget.
