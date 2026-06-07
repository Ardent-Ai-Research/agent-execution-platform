#!/usr/bin/env python3
"""Ardent API CLI (zero dependencies).

Commands:
- health
- feed
- wallet
- wallet-balance
- simulate
- execute
- status
- self-update
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any
from urllib import parse, request
from urllib.error import HTTPError, URLError


VERSION = "0.2.0"
REPO_RAW_BASE = "https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration"
RUNTIME_DIR = Path(os.getenv("ARDENT_RUNTIME_DIR", str(Path.home() / ".ardent")))


def parse_json_file(path: str) -> Any:
    with Path(path).expanduser().open("r", encoding="utf-8") as file_handle:
        return json.load(file_handle)


def parse_json_string(raw: str) -> Any:
    return json.loads(raw)


def output(payload: Any) -> None:
    print(json.dumps(payload, indent=2, ensure_ascii=False))


def build_api_key(args: argparse.Namespace) -> str | None:
    return args.api_key or os.getenv("ARDENT_API_KEY")


def build_base_url(args: argparse.Namespace) -> str:
    return (args.base_url or os.getenv("ARDENT_BASE_URL") or "https://api.ardentresearch.xyz").rstrip("/")


def call_api(
    method: str,
    url: str,
    *,
    body: dict[str, Any] | None = None,
    api_key: str | None = None,
    payment_proof: dict[str, Any] | None = None,
) -> tuple[int, Any]:
    headers: dict[str, str] = {
        "Accept": "application/json",
        "User-Agent": f"ardent-cli/{VERSION}",
    }

    if api_key:
        headers["X-API-Key"] = api_key

    if payment_proof is not None:
        headers["X-Payment-Proof"] = json.dumps(payment_proof, separators=(",", ":"))

    data: bytes | None = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")

    req = request.Request(url, headers=headers, data=data, method=method.upper())

    try:
        with request.urlopen(req, timeout=30) as response:
            status_code = response.getcode()
            raw = response.read().decode("utf-8", errors="replace")
            return status_code, safe_json(raw)
    except HTTPError as exc:
        raw = exc.read().decode("utf-8", errors="replace")
        return exc.code, safe_json(raw)
    except URLError as exc:
        raise RuntimeError(f"network error: {exc}") from exc


def download_text(url: str) -> str:
    try:
        with request.urlopen(url, timeout=30) as response:
            return response.read().decode("utf-8")
    except URLError as exc:
        raise RuntimeError(f"failed downloading {url}: {exc}") from exc


def safe_json(raw: str) -> Any:
    raw = (raw or "").strip()
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"raw": raw}


def require_api_key(args: argparse.Namespace) -> str:
    api_key = build_api_key(args)
    if not api_key:
        raise ValueError("missing API key: set --api-key or ARDENT_API_KEY")
    return api_key


def resolve_request_body(args: argparse.Namespace) -> dict[str, Any]:
    if args.body_file:
        body = parse_json_file(args.body_file)
        if not isinstance(body, dict):
            raise ValueError("body file must contain a JSON object")
        return body

    body: dict[str, Any] = {
        "agent_id": args.agent_id,
        "chain": args.chain,
        "target_contract": args.target_contract or "",
        "calldata": args.calldata or "",
        "value": args.value,
    }

    if args.strategy_id:
        body["strategy_id"] = args.strategy_id
    if args.callback_url:
        body["callback_url"] = args.callback_url

    if args.batch_file:
        batch_calls = parse_json_file(args.batch_file)
        if not isinstance(batch_calls, list):
            raise ValueError("batch file must contain a JSON array")
        body["batch_calls"] = batch_calls

    return body


def resolve_payment_proof(args: argparse.Namespace) -> dict[str, Any] | None:
    if args.payment_proof_file:
        proof = parse_json_file(args.payment_proof_file)
        if not isinstance(proof, dict):
            raise ValueError("payment proof file must contain a JSON object")
        return proof

    if args.payment_proof_json:
        proof = parse_json_string(args.payment_proof_json)
        if not isinstance(proof, dict):
            raise ValueError("--payment-proof-json must be a JSON object")
        return proof

    proof_fields = [
        args.proof_request_id,
        args.proof_payer,
        args.proof_token,
        args.proof_chain,
        args.proof_tx_hash,
    ]
    if any(proof_fields):
        if not all(proof_fields):
            raise ValueError(
                "when using --proof-* flags, provide all fields: request-id, payer, token, chain, tx-hash"
            )
        return {
            "request_id": args.proof_request_id,
            "payer": args.proof_payer,
            "token": args.proof_token,
            "chain": args.proof_chain,
            "tx_hash": args.proof_tx_hash,
        }

    return None


def run_health(args: argparse.Namespace) -> int:
    url = f"{build_base_url(args)}/health"
    status, payload = call_api("GET", url)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_feed(args: argparse.Namespace) -> int:
    qs = parse.urlencode({"limit": args.limit})
    url = f"{build_base_url(args)}/feed/recent?{qs}"
    status, payload = call_api("GET", url)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_wallet(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode({"agent_id": args.agent_id, "chain": args.chain})
    url = f"{build_base_url(args)}/wallet?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_wallet_balance(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode({"agent_id": args.agent_id, "chain": args.chain})
    url = f"{build_base_url(args)}/wallet/balance?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_request_body(args)
    url = f"{build_base_url(args)}/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_execute(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_request_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/execute"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def resolve_aave_action_body(args: argparse.Namespace) -> dict[str, Any]:
    if args.body_file:
        body = parse_json_file(args.body_file)
        if not isinstance(body, dict):
            raise ValueError("body file must contain a JSON object")
        return body

    body: dict[str, Any] = {
        "agent_id": args.agent_id,
        "chain": args.chain,
        "asset": args.asset,
    }
    if args.amount is not None:
        body["amount"] = args.amount
    if args.amount_raw is not None:
        body["amount_raw"] = args.amount_raw
    if getattr(args, "referral_code", None) is not None:
        body["referral_code"] = args.referral_code
    if getattr(args, "interest_rate_mode", None) is not None:
        body["interest_rate_mode"] = args.interest_rate_mode
    if getattr(args, "min_health_factor", None):
        body["min_health_factor"] = args.min_health_factor
    if getattr(args, "to", None):
        body["to"] = args.to
    if getattr(args, "on_behalf_of", None):
        body["on_behalf_of"] = args.on_behalf_of
    if args.strategy_id:
        body["strategy_id"] = args.strategy_id
    if args.callback_url:
        body["callback_url"] = args.callback_url
    return body


def run_aave_supply_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/supply/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_supply(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/supply"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_withdraw_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/withdraw/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_withdraw(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/withdraw"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_repay_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/repay/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_repay(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/repay"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_borrow_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/borrow/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_borrow(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_aave_action_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/aave-v3/borrow"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_aave_position(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode({"agent_id": args.agent_id, "chain": args.chain})
    url = f"{build_base_url(args)}/protocols/aave-v3/position?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_status(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    request_id = args.request_id
    url = f"{build_base_url(args)}/status/{parse.quote(request_id)}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_self_update(args: argparse.Namespace) -> int:
    cli_path = Path(__file__).resolve()
    cli_url = f"{REPO_RAW_BASE}/ardent_cli.py"

    cli_source = download_text(cli_url)
    cli_path.write_text(cli_source, encoding="utf-8")
    cli_path.chmod(0o755)

    # Parse the version from the downloaded file rather than reporting the current process version
    new_version = VERSION
    for line in cli_source.splitlines():
        if line.startswith("VERSION"):
            try:
                new_version = line.split('"')[1]
            except IndexError:
                pass
            break

    refreshed_files: list[str] = [str(cli_path)]

    if args.with_runtime:
        RUNTIME_DIR.mkdir(parents=True, exist_ok=True)
        runtime_files = ["mcp_server.py", "mcp-tools.json", "skills.md", "openapi.yaml"]
        for name in runtime_files:
            raw = download_text(f"{REPO_RAW_BASE}/{name}")
            target = RUNTIME_DIR / name
            target.write_text(raw, encoding="utf-8")
            if name.endswith(".py"):
                target.chmod(0o755)
            refreshed_files.append(str(target))

    output(
        {
            "message": "update complete",
            "version": new_version,
            "updated": refreshed_files,
        }
    )
    return 0


def add_global_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--base-url", help="API base URL (default env ARDENT_BASE_URL)")
    parser.add_argument("--api-key", help="API key (default env ARDENT_API_KEY)")


def add_execution_payload_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="ethereum", choices=["ethereum", "base", "arbitrum"], help="Target chain")
    parser.add_argument("--target-contract", help="Target contract address (single-call mode)")
    parser.add_argument("--calldata", help="Hex calldata (single-call mode)")
    parser.add_argument("--value", default="0", help="Value as string (default: 0)")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--batch-file", help="Path to JSON array for batch_calls")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_aave_supply_flags(parser: argparse.ArgumentParser) -> None:
    add_aave_amount_action_flags(parser)
    parser.add_argument("--referral-code", type=int, help="Optional Aave referral code")


def add_aave_amount_action_flags(parser: argparse.ArgumentParser, *, allow_max: bool = False) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="ethereum", choices=["ethereum"], help="Aave V3 Sepolia chain label")
    parser.add_argument(
        "--asset",
        required=True,
        choices=["AAVE", "DAI", "EURS", "GHO", "LINK", "USDC", "USDT", "WBTC", "WETH"],
        help="Aave V3 Sepolia reserve asset",
    )
    amount_help = "Human-readable token amount, e.g. 1.25"
    raw_help = "Raw token base-unit amount"
    if allow_max:
        amount_help += "; also supports max"
        raw_help += "; also supports max"
    parser.add_argument("--amount", help=amount_help)
    parser.add_argument("--amount-raw", help=raw_help)
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_payment_proof_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--payment-proof-json", help="Inline JSON object for X-Payment-Proof")
    parser.add_argument("--payment-proof-file", help="Path to JSON object for X-Payment-Proof")
    parser.add_argument("--proof-request-id", help="Payment proof request_id")
    parser.add_argument("--proof-payer", help="Payment proof payer")
    parser.add_argument("--proof-token", help="Payment proof token, e.g. USDC")
    parser.add_argument("--proof-chain", help="Payment proof chain")
    parser.add_argument("--proof-tx-hash", help="Payment proof transaction hash")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Ardent API CLI")
    parser.add_argument("--version", action="version", version=f"ardent {VERSION}")
    subparsers = parser.add_subparsers(dest="command", required=True)

    p_health = subparsers.add_parser("health", help="GET /health")
    add_global_flags(p_health)
    p_health.set_defaults(func=run_health)

    p_feed = subparsers.add_parser("feed", help="GET /feed/recent")
    add_global_flags(p_feed)
    p_feed.add_argument("--limit", type=int, default=12, help="Feed item limit (1-50)")
    p_feed.set_defaults(func=run_feed)

    p_wallet = subparsers.add_parser("wallet", help="GET /wallet")
    add_global_flags(p_wallet)
    p_wallet.add_argument("--agent-id", required=True, help="Agent identifier")
    p_wallet.add_argument("--chain", default="ethereum", choices=["ethereum", "base", "arbitrum"], help="Target chain")
    p_wallet.set_defaults(func=run_wallet)

    p_wallet_balance = subparsers.add_parser("wallet-balance", help="GET /wallet/balance")
    add_global_flags(p_wallet_balance)
    p_wallet_balance.add_argument("--agent-id", required=True, help="Agent identifier")
    p_wallet_balance.add_argument("--chain", default="ethereum", choices=["ethereum", "base", "arbitrum"], help="Target chain")
    p_wallet_balance.set_defaults(func=run_wallet_balance)

    p_sim = subparsers.add_parser("simulate", help="POST /simulate")
    add_global_flags(p_sim)
    add_execution_payload_flags(p_sim)
    p_sim.set_defaults(func=run_simulate)

    p_exec = subparsers.add_parser("execute", help="POST /execute")
    add_global_flags(p_exec)
    add_execution_payload_flags(p_exec)
    add_payment_proof_flags(p_exec)
    p_exec.set_defaults(func=run_execute)

    p_aave_sim = subparsers.add_parser("aave-supply-simulate", help="POST /protocols/aave-v3/supply/simulate")
    add_global_flags(p_aave_sim)
    add_aave_supply_flags(p_aave_sim)
    p_aave_sim.set_defaults(func=run_aave_supply_simulate)

    p_aave_exec = subparsers.add_parser("aave-supply", help="POST /protocols/aave-v3/supply")
    add_global_flags(p_aave_exec)
    add_aave_supply_flags(p_aave_exec)
    add_payment_proof_flags(p_aave_exec)
    p_aave_exec.set_defaults(func=run_aave_supply)

    p_aave_withdraw_sim = subparsers.add_parser("aave-withdraw-simulate", help="POST /protocols/aave-v3/withdraw/simulate")
    add_global_flags(p_aave_withdraw_sim)
    add_aave_amount_action_flags(p_aave_withdraw_sim, allow_max=True)
    p_aave_withdraw_sim.add_argument("--to", help="Optional recipient address; defaults to agent wallet")
    p_aave_withdraw_sim.set_defaults(func=run_aave_withdraw_simulate)

    p_aave_withdraw = subparsers.add_parser("aave-withdraw", help="POST /protocols/aave-v3/withdraw")
    add_global_flags(p_aave_withdraw)
    add_aave_amount_action_flags(p_aave_withdraw, allow_max=True)
    p_aave_withdraw.add_argument("--to", help="Optional recipient address; defaults to agent wallet")
    add_payment_proof_flags(p_aave_withdraw)
    p_aave_withdraw.set_defaults(func=run_aave_withdraw)

    p_aave_repay_sim = subparsers.add_parser("aave-repay-simulate", help="POST /protocols/aave-v3/repay/simulate")
    add_global_flags(p_aave_repay_sim)
    add_aave_amount_action_flags(p_aave_repay_sim, allow_max=True)
    p_aave_repay_sim.add_argument("--interest-rate-mode", type=int, choices=[1, 2], default=2, help="1 stable, 2 variable")
    p_aave_repay_sim.add_argument("--on-behalf-of", help="Optional debt owner; defaults to agent wallet")
    p_aave_repay_sim.set_defaults(func=run_aave_repay_simulate)

    p_aave_repay = subparsers.add_parser("aave-repay", help="POST /protocols/aave-v3/repay")
    add_global_flags(p_aave_repay)
    add_aave_amount_action_flags(p_aave_repay, allow_max=True)
    p_aave_repay.add_argument("--interest-rate-mode", type=int, choices=[1, 2], default=2, help="1 stable, 2 variable")
    p_aave_repay.add_argument("--on-behalf-of", help="Optional debt owner; defaults to agent wallet")
    add_payment_proof_flags(p_aave_repay)
    p_aave_repay.set_defaults(func=run_aave_repay)

    p_aave_borrow_sim = subparsers.add_parser("aave-borrow-simulate", help="POST /protocols/aave-v3/borrow/simulate")
    add_global_flags(p_aave_borrow_sim)
    add_aave_amount_action_flags(p_aave_borrow_sim, allow_max=True)
    p_aave_borrow_sim.add_argument("--interest-rate-mode", type=int, choices=[1, 2], default=2, help="1 stable, 2 variable")
    p_aave_borrow_sim.add_argument("--referral-code", type=int, help="Optional Aave referral code")
    p_aave_borrow_sim.add_argument("--on-behalf-of", help="Optional debt owner; defaults to agent wallet")
    p_aave_borrow_sim.add_argument("--min-health-factor", help="Minimum projected health factor after borrow; default 1.05")
    p_aave_borrow_sim.set_defaults(func=run_aave_borrow_simulate)

    p_aave_borrow = subparsers.add_parser("aave-borrow", help="POST /protocols/aave-v3/borrow")
    add_global_flags(p_aave_borrow)
    add_aave_amount_action_flags(p_aave_borrow, allow_max=True)
    p_aave_borrow.add_argument("--interest-rate-mode", type=int, choices=[1, 2], default=2, help="1 stable, 2 variable")
    p_aave_borrow.add_argument("--referral-code", type=int, help="Optional Aave referral code")
    p_aave_borrow.add_argument("--on-behalf-of", help="Optional debt owner; defaults to agent wallet")
    p_aave_borrow.add_argument("--min-health-factor", help="Minimum projected health factor after borrow; default 1.05")
    add_payment_proof_flags(p_aave_borrow)
    p_aave_borrow.set_defaults(func=run_aave_borrow)

    p_aave_position = subparsers.add_parser("aave-position", help="GET /protocols/aave-v3/position")
    add_global_flags(p_aave_position)
    p_aave_position.add_argument("--agent-id", required=True, help="Agent identifier")
    p_aave_position.add_argument("--chain", default="ethereum", choices=["ethereum"], help="Aave V3 Sepolia chain label")
    p_aave_position.set_defaults(func=run_aave_position)

    p_status = subparsers.add_parser("status", help="GET /status/{id}")
    add_global_flags(p_status)
    p_status.add_argument("--request-id", required=True, help="Request UUID")
    p_status.set_defaults(func=run_status)

    p_update = subparsers.add_parser("self-update", help="Update CLI from GitHub")
    p_update.add_argument(
        "--with-runtime",
        action="store_true",
        help="Also refresh ~/.ardent runtime files (mcp_server.py, mcp-tools.json, skills.md, openapi.yaml)",
    )
    p_update.set_defaults(func=run_self_update)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    if args.command == "feed" and not (1 <= args.limit <= 50):
        parser.error("--limit must be between 1 and 50")

    try:
        return args.func(args)
    except ValueError as exc:
        output({"error": str(exc)})
        return 2
    except Exception as exc:
        output({"error": f"unexpected error: {exc}"})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
