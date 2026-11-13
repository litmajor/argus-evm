# argus-evm

**High-performance, event-driven EVM liquidation & oracle-lag arbitrage runtime.**

Forked from the [Argus](https://github.com/litmajor/argus) **Surface-State Architecture (SSA)** — same core loop (continuous state streams → domain policy evaluation → deterministic actions + JSONL telemetry), re-targeted at Morpho Blue / Euler v2 liquidations.

---

## Status — implementation and integration

| Stage | Status | Notes |
|-------|--------|--------|
| **Alloy WS ingest** | Implemented | Subscribes to blocks and logs; the runtime currently passes no protocol addresses |
| **Domain events and state** | Implemented | Decoders and state logic exist; runtime is not configured with markets or positions |
| **Morpho share math / LIF** | Implemented | Math helpers exist in the state crate |
| **revm simulation** | Partially integrated | Contract address and signer identity are configured; historical Morpho discovery seeds current liquidatable borrowers |
| **VaultLiquidator.sol** | Implemented | Uses Morpho's callback flow; it does not use a flash loan |
| **Executor** | Implemented, unsafe for this dry-run as wired | Defaults to Flashbots bundle submission, not local Anvil submission |
| **Telemetry / receipt polling** | Implemented | These do not make the current pipeline end-to-end |

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

### Windows PowerShell: local Anvil fork smoke test

This builds the contract and runtime, deploys the contract to a local Base fork, scans Morpho borrow events, simulates currently liquidatable positions, and submits eligible transactions only to local Anvil. It does not send transactions to Base itself, but the RPC provider may have request or rate limits.

Prerequisites: Rust/Cargo and Foundry (`forge` and `anvil`) installed and available on `PATH`, plus an RPC URL with Base mainnet fork access.

Open a PowerShell terminal in the repository root. Start the fork in the first terminal:

```powershell
$env:BASE_RPC_URL = "https://base-mainnet.g.alchemy.com/v2/YOUR_KEY"
anvil --fork-url $env:BASE_RPC_URL --chain-id 8453
```

Keep Anvil running. In a second PowerShell terminal, build and deploy to the local node. Use only a private key printed by this local Anvil instance; never use a real wallet key.

```powershell
Set-Location "C:\path\to\argus-evm"
cargo build -p argus-runtime

Set-Location ".\contracts"
forge build

$anvilKey = "PASTE_A_PRIVATE_KEY_PRINTED_BY_ANVIL"
forge create "src/VaultLiquidator.sol:VaultLiquidator" `
  --rpc-url "http://127.0.0.1:8545" `
  --private-key $anvilKey `
  --constructor-args `
    "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb" `
    "0x2626664c2603336e57b271c5c0b26f421741e481" `
    3000
```

The constructor takes Morpho, the swap router, and the default Uniswap V3 fee tier (`3000` above). Record the deployed address for `ARGUS_LIQUIDATOR_ADDRESS`.

To run discovery and simulation, return to the repository root and configure both reads and submissions to use Anvil. Replace the placeholders with the deployment address and a recent Base block. Use only a key printed by this local Anvil instance:

```powershell
Set-Location "C:\path\to\argus-evm"
$env:ARGUS_WS_URL = "ws://127.0.0.1:8545"
$env:ARGUS_RPC_URL = "http://127.0.0.1:8545"
$env:ARGUS_CHAIN_ID = "8453"
$env:ARGUS_LIQUIDATOR_ADDRESS = "PASTE_LOCAL_DEPLOYMENT_ADDRESS"
$env:ARGUS_PRIVATE_KEY = $anvilKey
$env:ARGUS_PRIVATE_RPC = "http://127.0.0.1:8545"
$env:ARGUS_SUBMIT_MODE = "private_rpc"
$env:ARGUS_MORPHO_SCAN_FROM_BLOCK = "PASTE_RECENT_BASE_BLOCK"
$env:RUST_LOG = "info"
cargo run -p argus-runtime
```

Stop the runtime with Ctrl+C. Any submitted transaction is sent only to the local Anvil fork. For a no-submit startup smoke test, leave `ARGUS_PRIVATE_KEY` empty and set `ARGUS_SIM_CALLER_ADDRESS` to the local contract owner.

### Environment

| Variable | Description |
|----------|-------------|
| `ARGUS_WS_URL` | WebSocket RPC URL; the runtime derives the HTTP URL by replacing `ws://` with `http://` or `wss://` with `https://` |
| `ARGUS_RPC_URL` | HTTP RPC URL for historical discovery and fork simulation; optional if derivable from `ARGUS_WS_URL` |
| `ARGUS_CHAIN_ID` | Chain id (default `1`) |
| `ARGUS_MORPHO_ADDRESS` | Morpho Blue singleton address (defaults to the canonical Morpho Blue address) |
| `ARGUS_MORPHO_SCAN_FROM_BLOCK` | First block to scan for Morpho `Borrow` events; requests use 10-block chunks to accommodate restricted RPC tiers. Use a recent block to bound the scan; default `0` is a very long scan |
| `ARGUS_LIQUIDATOR_ADDRESS` | Required deployed `VaultLiquidator` address |
| `ARGUS_PRIVATE_KEY` | Signer key; also supplies the simulation caller unless `ARGUS_SIM_CALLER_ADDRESS` is set |
| `ARGUS_SIM_CALLER_ADDRESS` | Optional simulation caller override; must be the contract owner or an authorized operator |
| `ARGUS_PRIVATE_RPC` | Submission endpoint. When it is localhost/127.0.0.1 and no mode is set, the runtime selects raw `eth_sendRawTransaction` for Anvil |
| `ARGUS_SUBMIT_MODE` | Optional explicit mode: `flashbots_bundle`, `private_tx`, or `private_rpc` |
| `ARGUS_TELEMETRY` | JSONL output path |
| `RUST_LOG` | Tracing filter |

The runtime loads `.env` from the current working directory before reading environment variables. PowerShell environment variables also work and take precedence over values loaded from `.env`.

### Current integration limits

- Historical discovery is limited to borrowers with a `Borrow` event at or after `ARGUS_MORPHO_SCAN_FROM_BLOCK`; choose a block no later than the Morpho deployment to include existing borrowers.
- Historical logs identify borrowers, but the runtime reads each borrower's current position, market totals, and oracle price via `eth_call`. Large history scans can take time and use RPC quota; the 10-block chunks fit the Free-tier limit observed with the configured provider.
- The runtime performs this historical discovery at startup. The existing live event mirror is not a full on-chain state indexer and does not rescan all known borrowers as oracle prices change.
- Simulations pin a live RPC fork block and lazily read contract code and storage through the provider-backed revm database. They fail closed when live state is unavailable and only report success when the deployed liquidator emits its `LiquidationExecuted` profit event; estimates alone cannot authorize submission.
- A local `eth_sendRawTransaction` mutates the local Anvil fork only. Keep `ARGUS_PRIVATE_RPC` pointed at the local node for a local dry run; do not point it at a public RPC if you intend to avoid live submission.

---

## Contracts

```powershell
Set-Location "C:\path\to\argus-evm\contracts"
forge build
```

The current contract entry point is:

```solidity
function executeLiquidation(
    MarketParams calldata mp,
    address borrower,
    uint256 seizedAssets,
    uint256 repaidShares,
    uint256 minNetProfit,
    uint24 poolFee
) external;
```

Only the owner or an authorized operator can call it. The contract reverts with `InsufficientProfit` if the loan-token profit is below `minNetProfit`. Its constructor is `VaultLiquidator(address morpho_, address router_, uint24 defaultPoolFee_)`.

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
3. **Exact Morpho calldata** → encode `MarketParams, borrower, seizedAssets, repaidShares` as the `VaultLiquidator.executeLiquidation` arguments
4. **Multi-tx bundles** → approve + liquidate + swap as separate signed txs in `eth_sendBundle`

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full SSA mapping and latency budget.
