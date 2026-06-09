#!/usr/bin/env python3
"""Ardent API CLI (zero dependencies).

Commands:
- health
- feed
- wallet
- wallet-balance
- simulate
- execute
- aave-balances
- gmx-create-order
- gmx-cancel-order
- gmx-markets
- gmx-positions
- gmx-orders
- gmx-balances
- gmx-update-order
- gmx-create-deposit
- gmx-create-withdrawal
- gmx-cancel
- gmx-claim
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


VERSION = "0.5.0"
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


def run_aave_balances(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode({"agent_id": args.agent_id, "chain": args.chain})
    url = f"{build_base_url(args)}/protocols/aave-v3/balances?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def resolve_gmx_create_order_body(args: argparse.Namespace) -> dict[str, Any]:
    if args.body_file:
        body = parse_json_file(args.body_file)
        if not isinstance(body, dict):
            raise ValueError("body file must contain a JSON object")
        return body

    body: dict[str, Any] = {
        "agent_id": args.agent_id,
        "chain": args.chain,
        "order_type": args.order_type,
        "market": args.market,
        "initial_collateral_token": args.initial_collateral_token,
        "initial_collateral_delta_amount_raw": args.initial_collateral_delta_amount_raw,
        "execution_fee_raw": args.execution_fee_raw,
    }
    optional_fields = [
        "size_delta_usd_raw",
        "acceptable_price_raw",
        "min_output_amount_raw",
        "trigger_price_raw",
        "callback_gas_limit_raw",
        "valid_from_time_raw",
        "referral_code",
        "receiver",
        "cancellation_receiver",
        "callback_contract",
        "ui_fee_receiver",
        "strategy_id",
        "callback_url",
    ]
    for field in optional_fields:
        value = getattr(args, field, None)
        if value is not None:
            body[field] = value
    if args.swap_path:
        body["swap_path"] = args.swap_path
    if args.is_long is not None:
        body["is_long"] = args.is_long
    if args.should_unwrap_native_token:
        body["should_unwrap_native_token"] = True
    if args.auto_cancel:
        body["auto_cancel"] = True
    return body


def resolve_gmx_cancel_order_body(args: argparse.Namespace) -> dict[str, Any]:
    if args.body_file:
        body = parse_json_file(args.body_file)
        if not isinstance(body, dict):
            raise ValueError("body file must contain a JSON object")
        return body

    body: dict[str, Any] = {
        "agent_id": args.agent_id,
        "chain": args.chain,
        "order_key": args.order_key,
    }
    if args.strategy_id:
        body["strategy_id"] = args.strategy_id
    if args.callback_url:
        body["callback_url"] = args.callback_url
    return body


def resolve_gmx_simple_body(args: argparse.Namespace, fields: list[str]) -> dict[str, Any]:
    if args.body_file:
        body = parse_json_file(args.body_file)
        if not isinstance(body, dict):
            raise ValueError("body file must contain a JSON object")
        return body

    body: dict[str, Any] = {"agent_id": args.agent_id, "chain": args.chain}
    for field in fields:
        value = getattr(args, field, None)
        if value is not None:
            body[field] = value
    if args.strategy_id:
        body["strategy_id"] = args.strategy_id
    if args.callback_url:
        body["callback_url"] = args.callback_url
    return body


def run_gmx_update_order_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_simple_body(
        args,
        [
            "order_key",
            "size_delta_usd_raw",
            "acceptable_price_raw",
            "trigger_price_raw",
            "min_output_amount_raw",
            "valid_from_time_raw",
            "auto_cancel",
        ],
    )
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders/update/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_update_order(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_simple_body(
        args,
        [
            "order_key",
            "size_delta_usd_raw",
            "acceptable_price_raw",
            "trigger_price_raw",
            "min_output_amount_raw",
            "valid_from_time_raw",
            "auto_cancel",
        ],
    )
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders/update"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_create_order_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_create_order_body(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_create_order(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_create_order_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_cancel_order_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_cancel_order_body(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders/cancel/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_cancel_order(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_cancel_order_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders/cancel"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def build_range_query(args: argparse.Namespace, include_agent: bool = True) -> dict[str, Any]:
    qs: dict[str, Any] = {"chain": args.chain}
    if include_agent:
        qs["agent_id"] = args.agent_id
    if args.start is not None:
        qs["start"] = args.start
    if args.end is not None:
        qs["end"] = args.end
    return qs


def run_gmx_markets(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode(build_range_query(args, include_agent=False))
    url = f"{build_base_url(args)}/protocols/gmx-v2/markets?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_positions(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode(build_range_query(args))
    url = f"{build_base_url(args)}/protocols/gmx-v2/positions?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_orders(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode(build_range_query(args))
    url = f"{build_base_url(args)}/protocols/gmx-v2/orders?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_balances(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    qs = parse.urlencode(build_range_query(args))
    url = f"{build_base_url(args)}/protocols/gmx-v2/balances?{qs}"
    status, payload = call_api("GET", url, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def resolve_gmx_deposit_body(args: argparse.Namespace) -> dict[str, Any]:
    body = resolve_gmx_simple_body(
        args,
        [
            "market",
            "initial_long_token",
            "initial_short_token",
            "initial_long_token_amount_raw",
            "initial_short_token_amount_raw",
            "min_market_tokens_raw",
            "execution_fee_raw",
            "receiver",
            "callback_contract",
            "ui_fee_receiver",
            "callback_gas_limit_raw",
            "should_unwrap_native_token",
        ],
    )
    if not args.body_file:
        if args.long_token_swap_path:
            body["long_token_swap_path"] = args.long_token_swap_path
        if args.short_token_swap_path:
            body["short_token_swap_path"] = args.short_token_swap_path
    return body


def resolve_gmx_withdrawal_body(args: argparse.Namespace) -> dict[str, Any]:
    body = resolve_gmx_simple_body(
        args,
        [
            "market",
            "market_token_amount_raw",
            "min_long_token_amount_raw",
            "min_short_token_amount_raw",
            "execution_fee_raw",
            "receiver",
            "callback_contract",
            "ui_fee_receiver",
            "callback_gas_limit_raw",
            "should_unwrap_native_token",
        ],
    )
    if not args.body_file:
        if args.long_token_swap_path:
            body["long_token_swap_path"] = args.long_token_swap_path
        if args.short_token_swap_path:
            body["short_token_swap_path"] = args.short_token_swap_path
    return body


def resolve_gmx_claim_body(args: argparse.Namespace) -> dict[str, Any]:
    body = resolve_gmx_simple_body(args, ["claim_type", "receiver"])
    if not args.body_file:
        body["markets"] = args.market or []
        body["tokens"] = args.token or []
        if args.time_key_raw:
            body["time_keys_raw"] = args.time_key_raw
    return body


def run_gmx_create_deposit_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_deposit_body(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/deposits/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_create_deposit(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_deposit_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/deposits"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_create_withdrawal_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_withdrawal_body(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/withdrawals/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_create_withdrawal(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_withdrawal_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/withdrawals"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_cancel_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_simple_body(args, ["request_type", "key"])
    url = f"{build_base_url(args)}/protocols/gmx-v2/requests/cancel/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_cancel(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_simple_body(args, ["request_type", "key"])
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/requests/cancel"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_claim_simulate(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_claim_body(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/claims/simulate"
    status, payload = call_api("POST", url, body=body, api_key=api_key)
    output({"status": status, "data": payload})
    return 0 if 200 <= status < 300 else 1


def run_gmx_claim(args: argparse.Namespace) -> int:
    api_key = require_api_key(args)
    body = resolve_gmx_claim_body(args)
    payment_proof = resolve_payment_proof(args)
    url = f"{build_base_url(args)}/protocols/gmx-v2/claims"
    status, payload = call_api("POST", url, body=body, api_key=api_key, payment_proof=payment_proof)
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


def add_gmx_create_order_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument(
        "--order-type",
        required=True,
        choices=[
            "market_swap",
            "limit_swap",
            "market_increase",
            "limit_increase",
            "market_decrease",
            "limit_decrease",
            "stop_loss_decrease",
            "stop_increase",
        ],
        help="GMX order type",
    )
    parser.add_argument("--market", required=True, help="GMX market token address")
    parser.add_argument("--initial-collateral-token", required=True, help="Collateral/input token address")
    parser.add_argument(
        "--initial-collateral-delta-amount-raw",
        required=True,
        help="Raw collateral/input token amount",
    )
    parser.add_argument("--size-delta-usd-raw", help="Raw 30-decimal USD size delta; required for market_increase")
    parser.add_argument("--acceptable-price-raw", help="Raw 30-decimal acceptable price; required for market_increase")
    parser.add_argument("--min-output-amount-raw", help="Raw minimum output amount; required for market_swap")
    parser.add_argument("--execution-fee-raw", required=True, help="Raw ETH execution fee in wei")
    direction = parser.add_mutually_exclusive_group()
    direction.add_argument("--long", dest="is_long", action="store_true", help="Create long increase order")
    direction.add_argument("--short", dest="is_long", action="store_false", help="Create short increase order")
    parser.set_defaults(is_long=None)
    parser.add_argument("--receiver", help="Optional receiver; defaults to agent wallet")
    parser.add_argument("--cancellation-receiver", help="Optional cancellation receiver; defaults to receiver")
    parser.add_argument("--callback-contract", help="Optional GMX callback contract")
    parser.add_argument("--ui-fee-receiver", help="Optional UI fee receiver")
    parser.add_argument("--swap-path", action="append", help="Optional GMX swap path market address; repeatable")
    parser.add_argument("--trigger-price-raw", help="Raw 30-decimal trigger price")
    parser.add_argument("--callback-gas-limit-raw", help="Raw callback gas limit")
    parser.add_argument("--valid-from-time-raw", help="Raw timestamp from which the GMX order is valid")
    parser.add_argument("--referral-code", help="bytes32 hex or short ASCII referral code")
    parser.add_argument("--should-unwrap-native-token", action="store_true", help="Ask GMX to unwrap native output when supported")
    parser.add_argument("--auto-cancel", action="store_true", help="Set GMX autoCancel flag")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_cancel_order_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--order-key", required=True, help="GMX bytes32 order key")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_range_flags(parser: argparse.ArgumentParser, *, include_agent: bool = True) -> None:
    if include_agent:
        parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--start", type=int, help="Start index, default 0")
    parser.add_argument("--end", type=int, help="End index, default start + 50; max range 100")


def add_gmx_update_order_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--order-key", required=True, help="GMX bytes32 order key")
    parser.add_argument("--size-delta-usd-raw", required=True, help="Raw 30-decimal USD size delta")
    parser.add_argument("--acceptable-price-raw", required=True, help="Raw 30-decimal acceptable price")
    parser.add_argument("--trigger-price-raw", required=True, help="Raw 30-decimal trigger price")
    parser.add_argument("--min-output-amount-raw", required=True, help="Raw minimum output amount")
    parser.add_argument("--valid-from-time-raw", required=True, help="Raw timestamp from which the order is valid")
    parser.add_argument("--auto-cancel", action="store_true", help="Set GMX autoCancel flag")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_deposit_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--market", required=True, help="GMX market token address")
    parser.add_argument("--initial-long-token", required=True, help="Initial long token address")
    parser.add_argument("--initial-short-token", required=True, help="Initial short token address")
    parser.add_argument("--initial-long-token-amount-raw", help="Raw initial long token amount")
    parser.add_argument("--initial-short-token-amount-raw", help="Raw initial short token amount")
    parser.add_argument("--min-market-tokens-raw", required=True, help="Raw minimum GM market tokens")
    parser.add_argument("--execution-fee-raw", required=True, help="Raw ETH execution fee in wei")
    parser.add_argument("--receiver", help="Optional receiver; defaults to agent wallet")
    parser.add_argument("--callback-contract", help="Optional GMX callback contract")
    parser.add_argument("--ui-fee-receiver", help="Optional UI fee receiver")
    parser.add_argument("--long-token-swap-path", action="append", help="Optional long token swap path market address; repeatable")
    parser.add_argument("--short-token-swap-path", action="append", help="Optional short token swap path market address; repeatable")
    parser.add_argument("--callback-gas-limit-raw", help="Raw callback gas limit")
    parser.add_argument("--should-unwrap-native-token", action="store_true", help="Ask GMX to unwrap native output when supported")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_withdrawal_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--market", required=True, help="GMX market token address")
    parser.add_argument("--market-token-amount-raw", required=True, help="Raw GM market token amount")
    parser.add_argument("--min-long-token-amount-raw", required=True, help="Raw minimum long token amount")
    parser.add_argument("--min-short-token-amount-raw", required=True, help="Raw minimum short token amount")
    parser.add_argument("--execution-fee-raw", required=True, help="Raw ETH execution fee in wei")
    parser.add_argument("--receiver", help="Optional receiver; defaults to agent wallet")
    parser.add_argument("--callback-contract", help="Optional GMX callback contract")
    parser.add_argument("--ui-fee-receiver", help="Optional UI fee receiver")
    parser.add_argument("--long-token-swap-path", action="append", help="Optional long token swap path market address; repeatable")
    parser.add_argument("--short-token-swap-path", action="append", help="Optional short token swap path market address; repeatable")
    parser.add_argument("--callback-gas-limit-raw", help="Raw callback gas limit")
    parser.add_argument("--should-unwrap-native-token", action="store_true", help="Ask GMX to unwrap native output when supported")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_cancel_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--request-type", required=True, choices=["order", "deposit", "withdrawal", "shift"], help="GMX request type")
    parser.add_argument("--key", required=True, help="GMX bytes32 request key")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


def add_gmx_claim_flags(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--agent-id", required=True, help="Agent identifier")
    parser.add_argument("--chain", default="arbitrum", choices=["arbitrum"], help="GMX V2 Arbitrum Sepolia chain label")
    parser.add_argument("--claim-type", required=True, choices=["funding_fees", "collateral", "affiliate_rewards", "ui_fees"], help="GMX claim type")
    parser.add_argument("--market", action="append", help="GMX market address; repeatable")
    parser.add_argument("--token", action="append", help="GMX claim token address; repeatable")
    parser.add_argument("--time-key-raw", action="append", help="Collateral claim time key; repeatable")
    parser.add_argument("--receiver", help="Optional receiver; defaults to agent wallet")
    parser.add_argument("--strategy-id", help="Optional strategy ID")
    parser.add_argument("--callback-url", help="Optional callback webhook URL")
    parser.add_argument("--body-file", help="Path to full request JSON object (overrides payload flags)")


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

    p_aave_balances = subparsers.add_parser("aave-balances", help="GET /protocols/aave-v3/balances")
    add_global_flags(p_aave_balances)
    p_aave_balances.add_argument("--agent-id", required=True, help="Agent identifier")
    p_aave_balances.add_argument("--chain", default="ethereum", choices=["ethereum"], help="Aave V3 Sepolia chain label")
    p_aave_balances.set_defaults(func=run_aave_balances)

    p_gmx_create_order_sim = subparsers.add_parser("gmx-create-order-simulate", help="POST /protocols/gmx-v2/orders/simulate")
    add_global_flags(p_gmx_create_order_sim)
    add_gmx_create_order_flags(p_gmx_create_order_sim)
    p_gmx_create_order_sim.set_defaults(func=run_gmx_create_order_simulate)

    p_gmx_create_order = subparsers.add_parser("gmx-create-order", help="POST /protocols/gmx-v2/orders")
    add_global_flags(p_gmx_create_order)
    add_gmx_create_order_flags(p_gmx_create_order)
    add_payment_proof_flags(p_gmx_create_order)
    p_gmx_create_order.set_defaults(func=run_gmx_create_order)

    p_gmx_cancel_order_sim = subparsers.add_parser("gmx-cancel-order-simulate", help="POST /protocols/gmx-v2/orders/cancel/simulate")
    add_global_flags(p_gmx_cancel_order_sim)
    add_gmx_cancel_order_flags(p_gmx_cancel_order_sim)
    p_gmx_cancel_order_sim.set_defaults(func=run_gmx_cancel_order_simulate)

    p_gmx_cancel_order = subparsers.add_parser("gmx-cancel-order", help="POST /protocols/gmx-v2/orders/cancel")
    add_global_flags(p_gmx_cancel_order)
    add_gmx_cancel_order_flags(p_gmx_cancel_order)
    add_payment_proof_flags(p_gmx_cancel_order)
    p_gmx_cancel_order.set_defaults(func=run_gmx_cancel_order)

    p_gmx_markets = subparsers.add_parser("gmx-markets", help="GET /protocols/gmx-v2/markets")
    add_global_flags(p_gmx_markets)
    add_gmx_range_flags(p_gmx_markets, include_agent=False)
    p_gmx_markets.set_defaults(func=run_gmx_markets)

    p_gmx_positions = subparsers.add_parser("gmx-positions", help="GET /protocols/gmx-v2/positions")
    add_global_flags(p_gmx_positions)
    add_gmx_range_flags(p_gmx_positions)
    p_gmx_positions.set_defaults(func=run_gmx_positions)

    p_gmx_orders = subparsers.add_parser("gmx-orders", help="GET /protocols/gmx-v2/orders")
    add_global_flags(p_gmx_orders)
    add_gmx_range_flags(p_gmx_orders)
    p_gmx_orders.set_defaults(func=run_gmx_orders)

    p_gmx_balances = subparsers.add_parser(
        "gmx-balances",
        help="GET /protocols/gmx-v2/balances (GM market tokens + underlying market assets)",
    )
    add_global_flags(p_gmx_balances)
    add_gmx_range_flags(p_gmx_balances)
    p_gmx_balances.set_defaults(func=run_gmx_balances)

    p_gmx_update_order_sim = subparsers.add_parser("gmx-update-order-simulate", help="POST /protocols/gmx-v2/orders/update/simulate")
    add_global_flags(p_gmx_update_order_sim)
    add_gmx_update_order_flags(p_gmx_update_order_sim)
    p_gmx_update_order_sim.set_defaults(func=run_gmx_update_order_simulate)

    p_gmx_update_order = subparsers.add_parser("gmx-update-order", help="POST /protocols/gmx-v2/orders/update")
    add_global_flags(p_gmx_update_order)
    add_gmx_update_order_flags(p_gmx_update_order)
    add_payment_proof_flags(p_gmx_update_order)
    p_gmx_update_order.set_defaults(func=run_gmx_update_order)

    p_gmx_deposit_sim = subparsers.add_parser("gmx-create-deposit-simulate", help="POST /protocols/gmx-v2/deposits/simulate")
    add_global_flags(p_gmx_deposit_sim)
    add_gmx_deposit_flags(p_gmx_deposit_sim)
    p_gmx_deposit_sim.set_defaults(func=run_gmx_create_deposit_simulate)

    p_gmx_deposit = subparsers.add_parser("gmx-create-deposit", help="POST /protocols/gmx-v2/deposits")
    add_global_flags(p_gmx_deposit)
    add_gmx_deposit_flags(p_gmx_deposit)
    add_payment_proof_flags(p_gmx_deposit)
    p_gmx_deposit.set_defaults(func=run_gmx_create_deposit)

    p_gmx_withdrawal_sim = subparsers.add_parser("gmx-create-withdrawal-simulate", help="POST /protocols/gmx-v2/withdrawals/simulate")
    add_global_flags(p_gmx_withdrawal_sim)
    add_gmx_withdrawal_flags(p_gmx_withdrawal_sim)
    p_gmx_withdrawal_sim.set_defaults(func=run_gmx_create_withdrawal_simulate)

    p_gmx_withdrawal = subparsers.add_parser("gmx-create-withdrawal", help="POST /protocols/gmx-v2/withdrawals")
    add_global_flags(p_gmx_withdrawal)
    add_gmx_withdrawal_flags(p_gmx_withdrawal)
    add_payment_proof_flags(p_gmx_withdrawal)
    p_gmx_withdrawal.set_defaults(func=run_gmx_create_withdrawal)

    p_gmx_cancel_sim = subparsers.add_parser("gmx-cancel-simulate", help="POST /protocols/gmx-v2/requests/cancel/simulate")
    add_global_flags(p_gmx_cancel_sim)
    add_gmx_cancel_flags(p_gmx_cancel_sim)
    p_gmx_cancel_sim.set_defaults(func=run_gmx_cancel_simulate)

    p_gmx_cancel = subparsers.add_parser("gmx-cancel", help="POST /protocols/gmx-v2/requests/cancel")
    add_global_flags(p_gmx_cancel)
    add_gmx_cancel_flags(p_gmx_cancel)
    add_payment_proof_flags(p_gmx_cancel)
    p_gmx_cancel.set_defaults(func=run_gmx_cancel)

    p_gmx_claim_sim = subparsers.add_parser("gmx-claim-simulate", help="POST /protocols/gmx-v2/claims/simulate")
    add_global_flags(p_gmx_claim_sim)
    add_gmx_claim_flags(p_gmx_claim_sim)
    p_gmx_claim_sim.set_defaults(func=run_gmx_claim_simulate)

    p_gmx_claim = subparsers.add_parser("gmx-claim", help="POST /protocols/gmx-v2/claims")
    add_global_flags(p_gmx_claim)
    add_gmx_claim_flags(p_gmx_claim)
    add_payment_proof_flags(p_gmx_claim)
    p_gmx_claim.set_defaults(func=run_gmx_claim)

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
