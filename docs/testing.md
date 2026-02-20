# Testing Strategy

This project uses layered tests with explicit coverage gates.

## Layers

1. Unit + component (`cargo test --bin afhttp`)
- Focus: parser/config/body logic, request handlers, websocket/chunked event sequencing.
- Style: table-driven inputs + boundary values + event-order assertions.

2. End-to-end stress (`tests/stress.py`, `tests/cli_stress.py`, `tests/ws_stress.py`)
- Focus: real process behavior, request/response protocol, streaming and websocket integration.
- Note: test ports are configurable via `AFH_TEST_HTTP_PORT` and `AFH_TEST_WS_PORT`.
- Dependency: `tests/ws_stress.py` requires Python package `websockets`.

3. Coverage gate (`scripts/coverage_gate.py`)
- Runs `cargo llvm-cov --all-targets`.
- Enforces thresholds from `coverage-policy.json`.
- Core files are gated separately; exempt files are tracked but not blocking.

## Defect-driven regression policy

Every production bug fix must include a regression test.

- Required regression tests are listed in `tests/regressions.txt`.
- `scripts/check_regressions.sh` fails if any listed test is missing.

## Commands

Run all quality gates:

```bash
./scripts/test.sh
```

Run specific tiers:

```bash
./scripts/test.sh static
./scripts/test.sh unit
./scripts/test.sh e2e
./scripts/test.sh coverage
```
