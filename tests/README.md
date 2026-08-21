# Test Suite

The test suite is split by behavior:

- `tests/integration_tests.rs` exercises the HTTP API against PostgreSQL and Redis.
- Module tests under `src/**` cover serialization, helpers, adapters, queue behavior, wallet encryption, relayer invariants, and webhook delivery.

## Run tests

Start the shared services and apply migrations through normal application or test startup:

```bash
docker compose up -d
cargo test --test integration_tests -- --test-threads=1
```

Run narrower groups when iterating:

```bash
cargo test --test integration_tests -- --test-threads=1
cargo test --lib -- --test-threads=1
```

Single-threaded execution is recommended because integration tests share database and Redis state.

## Coverage focus

The integration suite covers health, public API-key issuance, API-key authentication, wallet provisioning and isolation, balance reads, simulation, execution validation, status reads, protocol routes, request size limits, and public feed behavior.

The highest-value remaining gaps are full worker-loop orchestration with controlled dependencies, broader mocked bundler failure scenarios, DNS-rebinding protection for webhook delivery, and process lifecycle/load tests around `main.rs`.
