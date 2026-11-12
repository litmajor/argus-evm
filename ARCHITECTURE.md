# argus-evm Architecture — Surface-State Architecture (SSA) for EVM MEV

This document maps the original Argus SSA pattern onto a high-frequency
liquidation / oracle-lag arbitrage runtime.

## Core Principle (unchanged)

**Separate concerns across five layers: State, Domains, Actions, Surfaces, Runtime.**

The same benefits apply:

- Extensibility — new protocols or strategies without touching the hot path
- Testability — each crate is independently unit-testable
- Clarity — unidirectional data flow
- Performance — no blocking RPC on the evaluation path

---

## Layer Mapping

| SSA Layer | Original Argus | argus-evm crate / role |
|-----------|----------------|------------------------|
| **State** | `src/core/` (Process, …) | `argus-types` — `Market`, `Position`, `HealthFactor`, `LiquidationCandidate`, `SimulationResult` |
| **Collectors** | process/cpu/memory collectors | `argus-ingestor` — Alloy WS `newHeads` + Morpho/Euler log filters |
| **Domains / Engines** | cpu/memory/security engines | `argus-state` — in-memory mirror + real-time HF evaluation |
| **Actions** | ProcessEvent, SecurityEvent, … | `DomainEvent` enum + `LiquidationCandidate` |
| **Surfaces** | console, overview, process UI | `argus-sim` (revm) + `argus-executor` (bundles + JSONL) |
| **Runtime** | main + event_bus | `argus-runtime` — Tokio orchestration, broadcast + mpsc |

---

## Concurrency Model

```
┌─────────────┐   broadcast<DomainEvent>   ┌──────────────┐
│  Ingestor   │ ─────────────────────────► │    State     │
│  (Alloy WS) │                            │   Engine     │
└─────────────┘                            └──────┬───────┘
                                                  │ mpsc<LiquidationCandidate>
                                                  ▼
                                           ┌──────────────┐
                                           │     Sim      │
                                           │   (revm)     │
                                           └──────┬───────┘
                                                  │ mpsc<SimulationResult>
                                                  ▼
                                           ┌──────────────┐
                                           │  Executor    │
                                           │ + JSONL log  │
                                           └──────────────┘
```

- **broadcast** for domain events → multiple surfaces can subscribe without coupling
- **bounded mpsc** for candidate / result pipelines → natural back-pressure
- Each stage is a dedicated Tokio task with its own `watch` shutdown receiver
- No shared mutable state across tasks except the `StateStore` (DashMap) inside the state engine

---

## Hot Path Latency Budget

| Stage | Target | Notes |
|-------|--------|-------|
| Log decode + emit | < 50 µs | alloy-sol-types, no allocation on common path |
| State update + HF | < 100 µs | pure in-memory, no RPC |
| revm simulation | < 2 ms | warm CacheDB fork |
| Bundle submit | < 5 ms | private RPC RTT |

Total detection → submission goal: **well under one block** on L2 / fast L1 slots.

---

## Telemetry

Every critical tick writes one JSON object per line:

```json
{"type":"block","ts":"...","block_number":123,"lag_ms":12,"positions_evaluated":40,"candidates_emitted":1}
{"type":"candidate","ts":"...","candidate_id":"...","health_factor":"0.97",...}
{"type":"simulation","ts":"...","success":true,"net_profit":"...","sim_latency_us":1400,...}
{"type":"execution","ts":"...","success":false,"error":"no private key configured",...}
```

Use these logs to answer “why did this candidate miss the block?” without replaying the chain.

---

## What was stripped vs swapped

**Stripped**

- Windows / OS process collectors
- TUI (ratatui) and Tauri UI
- Host-specific surfaces (security, findings, overview)

**Swapped in**

1. **alloy** — WebSocket provider, log filters, `sol!` event decoding
2. **revm** — in-process EVM for atomic path simulation
3. **Foundry contracts/** — `VaultLiquidator.sol` sharing ABI with the Rust simulator via `alloy::sol!`

---

## Adding a new protocol (checklist)

1. Define events with `sol!` in `argus-ingestor`
2. Implement `decode_log` → `DomainEvent`
3. Register contract addresses in `IngestorConfig`
4. Extend `StateEngine::handle_event` if new state transitions are required
5. Update `VaultLiquidator.sol` (or add a strategy-specific liquidator)
6. Add decoder unit tests with real log fixtures

---

## Production hardening (next steps)

- [ ] Wire real `Provider::subscribe_blocks` / `subscribe_logs` (Alloy 0.9)
- [ ] AlloyDB / CacheDB fork at specific block for revm
- [ ] Exact Morpho Blue share math + close-factor rules
- [ ] Exact Euler v2 EVC liquidate path
- [ ] Flashbots / Titan / Beaver / private sequencer endpoints
- [ ] Key management (HSM / AWS KMS / encrypted env)
- [ ] Multi-chain support via config
- [ ] Metrics (Prometheus) alongside JSONL


## Morpho Blue liquidation parameters (implemented)

- **No close factor** — liquidator may repay up to 100% of debt.
- **LIF** = min(1.15, 1 / (0.3 · LLTV + 0.7)), entire bonus to liquidator.
- Share conversions use SharesMathLib virtual offsets (1e6 shares, 1 asset).

## Executor submission paths (implemented)

1. `eth_sendBundle` (Flashbots-style) — default
2. `eth_sendPrivateTransaction` (Protect-style)
3. `eth_sendRawTransaction` (generic private RPC)

EIP-1559 txs signed with `alloy-signer-local`; optional `X-Flashbots-Signature` header.
