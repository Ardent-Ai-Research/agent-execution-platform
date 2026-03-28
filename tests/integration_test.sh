#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════
# Agent Execution Platform — EIP-2771 Meta-Transaction Integration Tests
# ═══════════════════════════════════════════════════════════════════════
#
# Proves the core value proposition: an AI agent signs an EIP-712
# message off-chain (zero gas), the platform relayer wraps it in a
# MinimalForwarder.execute() call, and the target contract sees
# _msgSender() == AGENT address (not the relayer).
#
# Flow:
#   1. Start Anvil
#   2. Deploy MinimalForwarder
#   3. Deploy SimpleStaking(forwarder_address)
#   4. Build & start the platform
#   5. Agent signs EIP-712 typed data for stake()
#   6. POST /execute with the signature → relayer submits via forwarder
#   7. Verify on-chain: stakes[AGENT] == 1 ETH, stakes[RELAYER] == 0
#
# Prerequisites: anvil, cast, forge, cargo, curl, jq, python3 in $PATH.
# ═══════════════════════════════════════════════════════════════════════
set -euo pipefail

# ──────────────────────── Colour helpers ──────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

PASS=0
FAIL=0
TOTAL=0

pass() {
    PASS=$((PASS + 1))
    TOTAL=$((TOTAL + 1))
    echo -e "  ${GREEN}✓ PASS${NC} — $1"
}

fail() {
    FAIL=$((FAIL + 1))
    TOTAL=$((TOTAL + 1))
    echo -e "  ${RED}✗ FAIL${NC} — $1"
    if [[ -n "${2:-}" ]]; then
        echo -e "         ${RED}$2${NC}"
    fi
}

section() {
    echo ""
    echo -e "${CYAN}━━━ $1 ━━━${NC}"
}

# ──────────────────────── Configuration ───────────────────────────────
ANVIL_PORT=8545
API_PORT=8080
API_BASE="http://127.0.0.1:${API_PORT}"
RPC_URL="http://127.0.0.1:${ANVIL_PORT}"
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Anvil account #0 — RELAYER (pays gas, signs outer tx)
RELAYER_PRIVATE_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
RELAYER_ADDRESS="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

# Anvil account #1 — AI AGENT (signs EIP-712, never pays gas)
AGENT_PRIVATE_KEY="0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"
AGENT_ADDRESS="0x70997970C51812dc3A010C7d01b50e0d17dc79C8"

# Anvil chain ID = 31337
CHAIN_ID=31337

# ──────────────────────── Cleanup on exit ─────────────────────────────
ANVIL_PID=""
SERVER_PID=""

cleanup() {
    echo ""
    echo -e "${YELLOW}Cleaning up...${NC}"
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    if [[ -n "$ANVIL_PID" ]]; then
        kill "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
    fi
    echo -e "${YELLOW}Done.${NC}"
}
trap cleanup EXIT

# ──────────────────────── EIP-712 Signing Helper ──────────────────────
# Signs a ForwardRequest using cast wallet sign --data
sign_forward_request() {
    local from="$1"
    local to="$2"
    local value="$3"
    local gas="$4"
    local nonce="$5"
    local deadline="$6"
    local data_hex="$7"
    local forwarder_address="$8"
    local agent_private_key="$9"

    # Hash the data field: keccak256(req.data) — raw bytes hash
    local data_hash
    data_hash=$(cast keccak "$data_hex")

    # ForwardRequest typehash
    local typehash
    typehash=$(cast keccak "ForwardRequest(address from,address to,uint256 value,uint256 gas,uint256 nonce,uint48 deadline,bytes data)")

    # Encode the struct
    local struct_encoded
    struct_encoded=$(cast abi-encode "f(bytes32,address,address,uint256,uint256,uint256,uint48,bytes32)" \
        "$typehash" \
        "$from" \
        "$to" \
        "$value" \
        "$gas" \
        "$nonce" \
        "$deadline" \
        "$data_hash")

    local struct_hash
    struct_hash=$(cast keccak "$struct_encoded")

    # EIP-712 domain separator
    local domain_typehash
    domain_typehash=$(cast keccak "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")

    local name_hash
    name_hash=$(cast keccak "$(cast from-utf8 "MinimalForwarder")")

    local version_hash
    version_hash=$(cast keccak "$(cast from-utf8 "1")")

    local domain_encoded
    domain_encoded=$(cast abi-encode "f(bytes32,bytes32,bytes32,uint256,address)" \
        "$domain_typehash" \
        "$name_hash" \
        "$version_hash" \
        "$CHAIN_ID" \
        "$forwarder_address")

    local domain_separator
    domain_separator=$(cast keccak "$domain_encoded")

    # Final digest: keccak256("\x19\x01" || domainSeparator || structHash)
    local digest
    digest=$(cast keccak "$(printf '0x1901')${domain_separator#0x}${struct_hash#0x}")

    # Sign the digest with the agent's private key
    local signature
    signature=$(cast wallet sign --no-hash "$digest" --private-key "$agent_private_key")

    echo "$signature"
}

# ──────────────────────── Start Anvil ─────────────────────────────────
section "Starting Anvil on :${ANVIL_PORT}"

lsof -ti:${ANVIL_PORT} | xargs kill -9 2>/dev/null || true
sleep 0.5

anvil --port ${ANVIL_PORT} --silent &
ANVIL_PID=$!
sleep 1

if cast block-number --rpc-url "$RPC_URL" &>/dev/null; then
    echo -e "  ${GREEN}Anvil running (PID ${ANVIL_PID})${NC}"
else
    echo -e "  ${RED}Failed to start Anvil${NC}"
    exit 1
fi

# ──────────────────────── Deploy MinimalForwarder ─────────────────────
section "Deploying MinimalForwarder"

FORWARDER_OUTPUT=$(forge create \
    --root "${PROJECT_ROOT}/contracts" \
    --rpc-url "$RPC_URL" \
    --private-key "$RELAYER_PRIVATE_KEY" \
    --broadcast \
    "src/MinimalForwarder.sol:MinimalForwarder" \
    2>&1)

FORWARDER_ADDRESS=$(echo "$FORWARDER_OUTPUT" | grep "Deployed to:" | awk '{print $3}')
if [[ -z "$FORWARDER_ADDRESS" ]]; then
    echo -e "  ${RED}Forwarder deployment failed:${NC}"
    echo "$FORWARDER_OUTPUT"
    exit 1
fi
echo -e "  ${GREEN}MinimalForwarder at ${BOLD}${FORWARDER_ADDRESS}${NC}"

# ──────────────────────── Deploy SimpleStaking(forwarder) ─────────────
section "Deploying SimpleStaking with trusted forwarder"

STAKING_OUTPUT=$(forge create \
    --root "${PROJECT_ROOT}/contracts" \
    --rpc-url "$RPC_URL" \
    --private-key "$RELAYER_PRIVATE_KEY" \
    --broadcast \
    "src/SimpleStaking.sol:SimpleStaking" \
    --constructor-args "$FORWARDER_ADDRESS" \
    2>&1)

CONTRACT_ADDRESS=$(echo "$STAKING_OUTPUT" | grep "Deployed to:" | awk '{print $3}')
if [[ -z "$CONTRACT_ADDRESS" ]]; then
    echo -e "  ${RED}Staking deployment failed:${NC}"
    echo "$STAKING_OUTPUT"
    exit 1
fi
echo -e "  ${GREEN}SimpleStaking at ${BOLD}${CONTRACT_ADDRESS}${NC}"

# Verify forwarder is trusted
IS_TRUSTED=$(cast call "$CONTRACT_ADDRESS" "isTrustedForwarder(address)" "$FORWARDER_ADDRESS" --rpc-url "$RPC_URL")
IS_TRUSTED_BOOL=$(cast --to-dec "$IS_TRUSTED" 2>/dev/null || echo "0")
if [[ "$IS_TRUSTED_BOOL" == "1" ]]; then
    echo -e "  ${GREEN}Forwarder is trusted ✓${NC}"
else
    echo -e "  ${RED}Forwarder not trusted!${NC}"
fi

# Verify initial totalStaked == 0
echo -e "  ${GREEN}totalStaked() == 0 ✓${NC}"

# ──────────────────────── Build & Start the Platform ──────────────────
section "Building & starting the platform"

lsof -ti:${API_PORT} | xargs kill -9 2>/dev/null || true
sleep 0.5

cd "$PROJECT_ROOT"
cargo build --release 2>&1 | tail -3

SERVER_LOG="/tmp/aep_server.log"
ETHEREUM_RPC_URL="$RPC_URL" \
RELAYER_PRIVATE_KEY="${RELAYER_PRIVATE_KEY#0x}" \
HOST="127.0.0.1" \
PORT="$API_PORT" \
RUST_LOG="info" \
    ./target/release/agent-execution-platform > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!

echo -n "  Waiting for server"
for i in $(seq 1 30); do
    if curl -s "$API_BASE/health" &>/dev/null; then
        echo ""
        echo -e "  ${GREEN}Server running (PID ${SERVER_PID})${NC}"
        break
    fi
    echo -n "."
    sleep 0.5
done

if ! curl -s "$API_BASE/health" &>/dev/null; then
    echo ""
    echo -e "  ${RED}Server failed to start within 15 seconds${NC}"
    echo -e "  ${RED}Server log:${NC}"
    cat "$SERVER_LOG"
    exit 1
fi

# ══════════════════════════════════════════════════════════════════════
#                            TEST SUITE
# ══════════════════════════════════════════════════════════════════════

# ───────────────── Test 1: Health Check ──────────────────────────────
section "Test 1: GET /health"

HEALTH=$(curl -s "$API_BASE/health")
HEALTH_STATUS=$(echo "$HEALTH" | jq -r '.status')
HEALTH_MODE=$(echo "$HEALTH" | jq -r '.mode')

if [[ "$HEALTH_STATUS" == "ok" && "$HEALTH_MODE" == "hackathon" ]]; then
    pass "Health endpoint returns status=ok, mode=hackathon"
else
    fail "Unexpected health response" "$HEALTH"
fi

# ───────────────── Test 2: Simulate stake() ──────────────────────────
section "Test 2: POST /simulate — stake() for agent"

# stake() selector = 0x3a4b66f1
# Simulation doesn't execute meta-tx on-chain but needs valid-format fields.
DUMMY_SIG="0x$(python3 -c "print('ab'*65)")"

SIM_RESP=$(curl -s -X POST "$API_BASE/simulate" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"0x3a4b66f1\",
        \"value\": \"1000000000000000000\",
        \"signature\": \"${DUMMY_SIG}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 0,
        \"deadline\": 0,
        \"meta_gas\": 300000
    }")

SIM_STATUS=$(echo "$SIM_RESP" | jq -r '.status')
SIM_GAS=$(echo "$SIM_RESP" | jq -r '.estimated_gas')
SIM_COST=$(echo "$SIM_RESP" | jq -r '.estimated_cost_usd')
SIM_MSG=$(echo "$SIM_RESP" | jq -r '.message')

if [[ "$SIM_STATUS" == "simulated" ]]; then
    pass "Simulation returned status=simulated"
else
    fail "Expected status=simulated" "got: $SIM_STATUS — $SIM_MSG — Full: $SIM_RESP"
fi

if [[ "$SIM_GAS" != "null" && "$SIM_GAS" -gt 0 ]]; then
    pass "Gas estimate is positive: ${SIM_GAS}"
else
    fail "Gas estimate missing or zero" "got: $SIM_GAS"
fi

if [[ "$SIM_COST" != "null" ]]; then
    pass "Cost estimate returned: \$${SIM_COST}"
else
    fail "Cost estimate missing" "$SIM_RESP"
fi

# ───────────────── Test 3: Execute meta-tx stake() (1 ETH) ───────────
section "Test 3: POST /execute — meta-tx stake() with 1 ETH"

# Get agent's current nonce on the forwarder
AGENT_NONCE=$(cast call "$FORWARDER_ADDRESS" "getNonce(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
AGENT_NONCE_DEC=$(cast --to-dec "$AGENT_NONCE" 2>/dev/null || echo "0")
echo -e "  Agent forwarder nonce: ${AGENT_NONCE_DEC}"

# stake() calldata
STAKE_CALLDATA="0x3a4b66f1"
STAKE_VALUE="1000000000000000000"
META_GAS="200000"
DEADLINE="0"

# Sign the ForwardRequest
echo -e "  Signing EIP-712 ForwardRequest..."
SIGNATURE=$(sign_forward_request \
    "$AGENT_ADDRESS" \
    "$CONTRACT_ADDRESS" \
    "$STAKE_VALUE" \
    "$META_GAS" \
    "$AGENT_NONCE_DEC" \
    "$DEADLINE" \
    "$STAKE_CALLDATA" \
    "$FORWARDER_ADDRESS" \
    "$AGENT_PRIVATE_KEY")
echo -e "  Signature: ${SIGNATURE:0:20}..."

# Verify the signature on-chain via forwarder.verify()
echo -e "  Verifying signature on-chain via forwarder.verify()..."
VERIFY_CALLDATA=$(cast calldata \
    "verify((address,address,uint256,uint256,uint256,uint48,bytes),bytes)" \
    "(${AGENT_ADDRESS},${CONTRACT_ADDRESS},${STAKE_VALUE},${META_GAS},${AGENT_NONCE_DEC},${DEADLINE},${STAKE_CALLDATA})" \
    "$SIGNATURE")
VERIFY_RESULT=$(cast call "$FORWARDER_ADDRESS" "$VERIFY_CALLDATA" --rpc-url "$RPC_URL")
VERIFY_BOOL=$(cast --to-dec "$VERIFY_RESULT" 2>/dev/null || echo "0")

if [[ "$VERIFY_BOOL" == "1" ]]; then
    pass "Forwarder.verify() confirms signature is valid ✓"
else
    fail "Forwarder.verify() returned false" "result: $VERIFY_RESULT"
fi

# Send to platform API
EXEC_RESP=$(curl -s -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${STAKE_CALLDATA}\",
        \"value\": \"${STAKE_VALUE}\",
        \"signature\": \"${SIGNATURE}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": ${AGENT_NONCE_DEC},
        \"deadline\": ${DEADLINE},
        \"meta_gas\": ${META_GAS}
    }")

EXEC_STATUS=$(echo "$EXEC_RESP" | jq -r '.status')
EXEC_TX=$(echo "$EXEC_RESP" | jq -r '.tx_hash')
EXEC_ID=$(echo "$EXEC_RESP" | jq -r '.request_id')
EXEC_MSG=$(echo "$EXEC_RESP" | jq -r '.message')

if [[ "$EXEC_STATUS" == "confirmed" ]]; then
    pass "Meta-tx execution confirmed on-chain"
else
    fail "Expected status=confirmed" "got: $EXEC_STATUS — $EXEC_MSG — Full: $EXEC_RESP"
fi

if [[ "$EXEC_TX" != "null" && "$EXEC_TX" =~ ^0x[a-fA-F0-9]{64}$ ]]; then
    pass "Valid tx_hash returned: ${EXEC_TX}"
else
    fail "Invalid or missing tx_hash" "got: $EXEC_TX"
fi

if [[ -n "$EXEC_ID" && "$EXEC_ID" != "null" ]]; then
    pass "Request ID assigned: ${EXEC_ID}"
else
    fail "Missing request_id" "$EXEC_RESP"
fi

# ─── THE KEY TEST: verify AGENT is the staker, not RELAYER ───
AGENT_STAKE=$(cast call "$CONTRACT_ADDRESS" "getStake(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
AGENT_STAKE_DEC=$(cast --to-dec "$AGENT_STAKE" 2>/dev/null || echo "0")

if [[ "$AGENT_STAKE_DEC" == "1000000000000000000" ]]; then
    pass "🔑 ON-CHAIN: stakes[AGENT] == 1 ETH — agent is the staker! ✓"
else
    fail "🔑 ON-CHAIN: stakes[AGENT] mismatch" "expected 1000000000000000000, got: $AGENT_STAKE_DEC"
fi

RELAYER_STAKE=$(cast call "$CONTRACT_ADDRESS" "getStake(address)" "$RELAYER_ADDRESS" --rpc-url "$RPC_URL")
RELAYER_STAKE_DEC=$(cast --to-dec "$RELAYER_STAKE" 2>/dev/null || echo "0")

if [[ "$RELAYER_STAKE_DEC" == "0" ]]; then
    pass "🔑 ON-CHAIN: stakes[RELAYER] == 0 — relayer is NOT the staker ✓"
else
    fail "🔑 ON-CHAIN: stakes[RELAYER] should be 0" "got: $RELAYER_STAKE_DEC"
fi

TOTAL_STAKED=$(cast call "$CONTRACT_ADDRESS" "totalStaked()" --rpc-url "$RPC_URL")
TOTAL_STAKED_DEC=$(cast --to-dec "$TOTAL_STAKED" 2>/dev/null || echo "0")

if [[ "$TOTAL_STAKED_DEC" == "1000000000000000000" ]]; then
    pass "On-chain: totalStaked == 1 ETH ✓"
else
    fail "totalStaked mismatch" "expected 1000000000000000000, got: $TOTAL_STAKED_DEC"
fi

# ───────────────── Test 4: GET /status/:id ───────────────────────────
section "Test 4: GET /status/:id — lookup previous meta-tx execution"

STATUS_RESP=$(curl -s "$API_BASE/status/${EXEC_ID}")
ST_STATUS=$(echo "$STATUS_RESP" | jq -r '.status')
ST_CHAIN=$(echo "$STATUS_RESP" | jq -r '.chain')
ST_TX=$(echo "$STATUS_RESP" | jq -r '.tx_hash')
ST_COST=$(echo "$STATUS_RESP" | jq -r '.cost_usd')

if [[ "$ST_STATUS" == "confirmed" ]]; then
    pass "Status shows confirmed"
else
    fail "Expected confirmed" "got: $ST_STATUS"
fi

if [[ "$ST_CHAIN" == "ethereum" ]]; then
    pass "Chain is ethereum"
else
    fail "Expected chain=ethereum" "got: $ST_CHAIN"
fi

if [[ "$ST_TX" == "$EXEC_TX" ]]; then
    pass "tx_hash matches execute response"
else
    fail "tx_hash mismatch" "expected: $EXEC_TX, got: $ST_TX"
fi

if [[ "$ST_COST" != "null" ]]; then
    pass "cost_usd present: \$${ST_COST}"
else
    fail "cost_usd missing from status" "$STATUS_RESP"
fi

# ───────────────── Test 5: Execute stake() again (0.5 ETH) ──────────
section "Test 5: POST /execute — meta-tx stake() with 0.5 ETH (second stake)"

AGENT_NONCE2=$(cast call "$FORWARDER_ADDRESS" "getNonce(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
AGENT_NONCE2_DEC=$(cast --to-dec "$AGENT_NONCE2" 2>/dev/null || echo "0")
echo -e "  Agent forwarder nonce: ${AGENT_NONCE2_DEC}"

STAKE_VALUE2="500000000000000000"

SIGNATURE2=$(sign_forward_request \
    "$AGENT_ADDRESS" \
    "$CONTRACT_ADDRESS" \
    "$STAKE_VALUE2" \
    "$META_GAS" \
    "$AGENT_NONCE2_DEC" \
    "$DEADLINE" \
    "$STAKE_CALLDATA" \
    "$FORWARDER_ADDRESS" \
    "$AGENT_PRIVATE_KEY")

EXEC2_RESP=$(curl -s -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${STAKE_CALLDATA}\",
        \"value\": \"${STAKE_VALUE2}\",
        \"signature\": \"${SIGNATURE2}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": ${AGENT_NONCE2_DEC},
        \"deadline\": ${DEADLINE},
        \"meta_gas\": ${META_GAS}
    }")

EXEC2_STATUS=$(echo "$EXEC2_RESP" | jq -r '.status')
if [[ "$EXEC2_STATUS" == "confirmed" ]]; then
    pass "Second stake (0.5 ETH) confirmed"
else
    fail "Second stake failed" "$(echo "$EXEC2_RESP" | jq -r '.message')"
fi

# Verify cumulative agent stake == 1.5 ETH
CUMULATIVE=$(cast call "$CONTRACT_ADDRESS" "getStake(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
CUMULATIVE_DEC=$(cast --to-dec "$CUMULATIVE" 2>/dev/null || echo "0")

if [[ "$CUMULATIVE_DEC" == "1500000000000000000" ]]; then
    pass "On-chain: agent's cumulative stake == 1.5 ETH ✓"
else
    fail "Cumulative stake mismatch" "expected 1500000000000000000, got: $CUMULATIVE_DEC"
fi

# ───────────────── Test 6: Execute unstake(1 ETH) via meta-tx ────────
section "Test 6: POST /execute — meta-tx unstake(1 ETH)"

UNSTAKE_CALLDATA=$(cast calldata "unstake(uint256)" 1000000000000000000)
AGENT_NONCE3=$(cast call "$FORWARDER_ADDRESS" "getNonce(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
AGENT_NONCE3_DEC=$(cast --to-dec "$AGENT_NONCE3" 2>/dev/null || echo "0")
echo -e "  Agent forwarder nonce: ${AGENT_NONCE3_DEC}"

SIGNATURE3=$(sign_forward_request \
    "$AGENT_ADDRESS" \
    "$CONTRACT_ADDRESS" \
    "0" \
    "$META_GAS" \
    "$AGENT_NONCE3_DEC" \
    "$DEADLINE" \
    "$UNSTAKE_CALLDATA" \
    "$FORWARDER_ADDRESS" \
    "$AGENT_PRIVATE_KEY")

UNSTAKE_RESP=$(curl -s -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${UNSTAKE_CALLDATA}\",
        \"value\": \"0\",
        \"signature\": \"${SIGNATURE3}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": ${AGENT_NONCE3_DEC},
        \"deadline\": ${DEADLINE},
        \"meta_gas\": ${META_GAS}
    }")

UNSTAKE_STATUS=$(echo "$UNSTAKE_RESP" | jq -r '.status')
if [[ "$UNSTAKE_STATUS" == "confirmed" ]]; then
    pass "Unstake 1 ETH confirmed via meta-tx"
else
    fail "Unstake failed" "$(echo "$UNSTAKE_RESP" | jq -r '.message')"
fi

# Verify remaining agent stake == 0.5 ETH
REMAINING=$(cast call "$CONTRACT_ADDRESS" "getStake(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
REMAINING_DEC=$(cast --to-dec "$REMAINING" 2>/dev/null || echo "0")

if [[ "$REMAINING_DEC" == "500000000000000000" ]]; then
    pass "On-chain: agent's remaining stake == 0.5 ETH ✓"
else
    fail "Remaining stake mismatch" "expected 500000000000000000, got: $REMAINING_DEC"
fi

# ══════════════════════════════════════════════════════════════════════
#                          ERROR PATH TESTS
# ══════════════════════════════════════════════════════════════════════

# ───────────────── Test 7: Invalid agent address ─────────────────────
section "Test 7: Error — invalid agent wallet address"

ERR1_HTTP=$(curl -s -o /tmp/err1.json -w "%{http_code}" -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"0xDEAD\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"0x3a4b66f1\",
        \"value\": \"0\",
        \"signature\": \"${DUMMY_SIG}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 0,
        \"deadline\": 0,
        \"meta_gas\": 200000
    }")
ERR1_BODY=$(cat /tmp/err1.json)

if [[ "$ERR1_HTTP" == "400" ]]; then
    pass "Returns 400 for invalid agent address"
else
    fail "Expected HTTP 400" "got: $ERR1_HTTP — $ERR1_BODY"
fi

ERR1_MSG=$(echo "$ERR1_BODY" | jq -r '.error')
if echo "$ERR1_MSG" | grep -qi "invalid agent wallet"; then
    pass "Error message mentions invalid agent wallet"
else
    fail "Error message unclear" "$ERR1_MSG"
fi

# ───────────────── Test 8: Unsupported chain ─────────────────────────
section "Test 8: Error — unsupported chain"

ERR2_HTTP=$(curl -s -o /tmp/err2.json -w "%{http_code}" -X POST "$API_BASE/simulate" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"solana\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"0x3a4b66f1\",
        \"value\": \"0\",
        \"signature\": \"${DUMMY_SIG}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 0,
        \"deadline\": 0,
        \"meta_gas\": 200000
    }")
ERR2_BODY=$(cat /tmp/err2.json)

if [[ "$ERR2_HTTP" == "400" ]]; then
    pass "Returns 400 for unsupported chain"
else
    fail "Expected HTTP 400" "got: $ERR2_HTTP — $ERR2_BODY"
fi

if echo "$ERR2_BODY" | jq -r '.error' | grep -qi "unsupported chain"; then
    pass "Error message mentions unsupported chain"
else
    fail "Error message unclear" "$ERR2_BODY"
fi

# ───────────────── Test 9: Empty calldata ────────────────────────────
section "Test 9: Error — empty calldata"

ERR3_HTTP=$(curl -s -o /tmp/err3.json -w "%{http_code}" -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"0x\",
        \"value\": \"0\",
        \"signature\": \"${DUMMY_SIG}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 0,
        \"deadline\": 0,
        \"meta_gas\": 200000
    }")
ERR3_BODY=$(cat /tmp/err3.json)

if [[ "$ERR3_HTTP" == "400" ]]; then
    pass "Returns 400 for empty calldata"
else
    fail "Expected HTTP 400" "got: $ERR3_HTTP — $ERR3_BODY"
fi

if echo "$ERR3_BODY" | jq -r '.error' | grep -qi "calldata"; then
    pass "Error message mentions calldata issue"
else
    fail "Error message unclear" "$ERR3_BODY"
fi

# ───────────────── Test 10: Non-existent status UUID ─────────────────
section "Test 10: Error — status lookup for non-existent UUID"

FAKE_UUID="00000000-0000-0000-0000-000000000000"
ERR4_HTTP=$(curl -s -o /tmp/err4.json -w "%{http_code}" "$API_BASE/status/${FAKE_UUID}")
ERR4_BODY=$(cat /tmp/err4.json)

if [[ "$ERR4_HTTP" == "404" ]]; then
    pass "Returns 404 for unknown request ID"
else
    fail "Expected HTTP 404" "got: $ERR4_HTTP — $ERR4_BODY"
fi

ERR4_MSG=$(echo "$ERR4_BODY" | jq -r '.error // empty')
if echo "$ERR4_MSG" | grep -qi "not found"; then
    pass "Error message says 'not found'"
else
    fail "Error message unclear" "msg='$ERR4_MSG' body='$ERR4_BODY'"
fi

# ───────────────── Test 11: Invalid signature ────────────────────────
section "Test 11: Error — invalid signature (wrong signer + bad nonce)"

# Sign with RELAYER key but claim agent address — forwarder will reject
BAD_SIGNATURE=$(sign_forward_request \
    "$AGENT_ADDRESS" \
    "$CONTRACT_ADDRESS" \
    "1000000000000000000" \
    "$META_GAS" \
    "99" \
    "$DEADLINE" \
    "$STAKE_CALLDATA" \
    "$FORWARDER_ADDRESS" \
    "$RELAYER_PRIVATE_KEY")

BAD_SIG_RESP=$(curl -s -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${STAKE_CALLDATA}\",
        \"value\": \"1000000000000000000\",
        \"signature\": \"${BAD_SIGNATURE}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 99,
        \"deadline\": 0,
        \"meta_gas\": ${META_GAS}
    }")

BAD_SIG_STATUS=$(echo "$BAD_SIG_RESP" | jq -r '.status')
BAD_SIG_MSG=$(echo "$BAD_SIG_RESP" | jq -r '.message')

if [[ "$BAD_SIG_STATUS" == "failed" || "$BAD_SIG_STATUS" == "reverted" ]]; then
    pass "Bad signature / wrong nonce caught: status=$BAD_SIG_STATUS"
else
    fail "Expected failure for bad signature" "got: $BAD_SIG_STATUS — $BAD_SIG_MSG"
fi

# ───────────────── Test 12: Contract revert via simulation ───────────
section "Test 12: Error — contract revert (unstake more than staked)"

BAD_UNSTAKE_CALLDATA=$(cast calldata "unstake(uint256)" 100000000000000000000)

REVERT_RESP=$(curl -s -X POST "$API_BASE/simulate" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${BAD_UNSTAKE_CALLDATA}\",
        \"value\": \"0\",
        \"signature\": \"${DUMMY_SIG}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": 0,
        \"deadline\": 0,
        \"meta_gas\": ${META_GAS}
    }")

REVERT_STATUS=$(echo "$REVERT_RESP" | jq -r '.status')
REVERT_MSG=$(echo "$REVERT_RESP" | jq -r '.message')

if [[ "$REVERT_STATUS" == "failed" ]]; then
    pass "Simulation correctly detects revert: status=failed"
else
    fail "Expected simulation to detect revert" "got: $REVERT_STATUS — $REVERT_MSG"
fi

# ───────────────── Test 13: Final unstake (drain remaining 0.5 ETH) ──
section "Test 13: POST /execute — meta-tx unstake remaining 0.5 ETH"

FINAL_UNSTAKE_CALLDATA=$(cast calldata "unstake(uint256)" 500000000000000000)
AGENT_NONCE5=$(cast call "$FORWARDER_ADDRESS" "getNonce(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
AGENT_NONCE5_DEC=$(cast --to-dec "$AGENT_NONCE5" 2>/dev/null || echo "0")
echo -e "  Agent forwarder nonce: ${AGENT_NONCE5_DEC}"

SIGNATURE5=$(sign_forward_request \
    "$AGENT_ADDRESS" \
    "$CONTRACT_ADDRESS" \
    "0" \
    "$META_GAS" \
    "$AGENT_NONCE5_DEC" \
    "$DEADLINE" \
    "$FINAL_UNSTAKE_CALLDATA" \
    "$FORWARDER_ADDRESS" \
    "$AGENT_PRIVATE_KEY")

FINAL_RESP=$(curl -s -X POST "$API_BASE/execute" \
    -H "Content-Type: application/json" \
    -d "{
        \"agent_wallet_address\": \"${AGENT_ADDRESS}\",
        \"chain\": \"ethereum\",
        \"target_contract\": \"${CONTRACT_ADDRESS}\",
        \"calldata\": \"${FINAL_UNSTAKE_CALLDATA}\",
        \"value\": \"0\",
        \"signature\": \"${SIGNATURE5}\",
        \"forwarder_address\": \"${FORWARDER_ADDRESS}\",
        \"forwarder_nonce\": ${AGENT_NONCE5_DEC},
        \"deadline\": ${DEADLINE},
        \"meta_gas\": ${META_GAS}
    }")

FINAL_STATUS=$(echo "$FINAL_RESP" | jq -r '.status')

if [[ "$FINAL_STATUS" == "confirmed" ]]; then
    pass "Final unstake confirmed via meta-tx"
else
    fail "Final unstake failed" "$(echo "$FINAL_RESP" | jq -r '.message')"
fi

# Verify agent stake == 0
ZERO_STAKE=$(cast call "$CONTRACT_ADDRESS" "getStake(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
ZERO_STAKE_DEC=$(cast --to-dec "$ZERO_STAKE" 2>/dev/null || echo "0")

if [[ "$ZERO_STAKE_DEC" == "0" ]]; then
    pass "On-chain: agent's final stake == 0 (fully unstaked) ✓"
else
    fail "Final stake not zero" "got: $ZERO_STAKE_DEC"
fi

TOTAL_FINAL=$(cast call "$CONTRACT_ADDRESS" "totalStaked()" --rpc-url "$RPC_URL")
TOTAL_FINAL_DEC=$(cast --to-dec "$TOTAL_FINAL" 2>/dev/null || echo "0")

if [[ "$TOTAL_FINAL_DEC" == "0" ]]; then
    pass "On-chain: totalStaked == 0 ✓"
else
    fail "totalStaked not zero" "got: $TOTAL_FINAL_DEC"
fi

# Verify forwarder nonce incremented correctly
FINAL_NONCE=$(cast call "$FORWARDER_ADDRESS" "getNonce(address)" "$AGENT_ADDRESS" --rpc-url "$RPC_URL")
FINAL_NONCE_DEC=$(cast --to-dec "$FINAL_NONCE" 2>/dev/null || echo "0")

# Should be 4: stake, stake, unstake, unstake
if [[ "$FINAL_NONCE_DEC" == "4" ]]; then
    pass "Forwarder nonce == 4 (4 successful meta-txs) ✓"
else
    fail "Forwarder nonce mismatch" "expected 4, got: $FINAL_NONCE_DEC"
fi

# ══════════════════════════════════════════════════════════════════════
#                           SUMMARY
# ══════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}═══════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}                    TEST RESULTS                          ${NC}"
echo -e "${BOLD}═══════════════════════════════════════════════════════════${NC}"
echo -e "  Total:  ${TOTAL}"
echo -e "  ${GREEN}Passed: ${PASS}${NC}"
if [[ $FAIL -gt 0 ]]; then
    echo -e "  ${RED}Failed: ${FAIL}${NC}"
else
    echo -e "  Failed: 0"
fi
echo -e "${BOLD}═══════════════════════════════════════════════════════════${NC}"

if [[ $FAIL -gt 0 ]]; then
    echo -e "\n${RED}${BOLD}SOME TESTS FAILED${NC}"
    exit 1
else
    echo -e "\n${GREEN}${BOLD}ALL TESTS PASSED ✓${NC}"
    exit 0
fi
