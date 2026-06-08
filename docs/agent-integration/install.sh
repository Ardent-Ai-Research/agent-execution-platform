#!/usr/bin/env bash
set -euo pipefail

REPO_RAW_BASE="https://raw.githubusercontent.com/Ardent-Ai-Research/agent-execution-platform/master/docs/agent-integration"

INSTALL_DIR="${HOME}/.local/bin"
RUNTIME_DIR="${HOME}/.ardent"

# Require Python 3.8+
if ! command -v python3 &>/dev/null; then
  echo "❌ python3 is required but was not found. Install it from https://python.org and re-run."
  exit 1
fi
PY_VERSION=$(python3 -c 'import sys; print("%d%02d" % sys.version_info[:2])')
if [[ "${PY_VERSION}" -lt 308 ]]; then
  echo "❌ python3.8 or newer is required (found $(python3 --version)). Please upgrade."
  exit 1
fi

mkdir -p "${INSTALL_DIR}" "${RUNTIME_DIR}"

echo "Installing Ardent CLI..."
curl -fsSL "${REPO_RAW_BASE}/ardent_cli.py" -o "${INSTALL_DIR}/ardent"
chmod +x "${INSTALL_DIR}/ardent"

ensure_path_persisted() {
  local line='export PATH="${HOME}/.local/bin:${PATH}"'
  local updated=0
  local candidates=()
  local primary_profile="${HOME}/.zshrc"

  case "${SHELL:-}" in
    */zsh)
      primary_profile="${HOME}/.zshrc"
      candidates+=("${HOME}/.zshrc")
      ;;
    */bash)
      primary_profile="${HOME}/.bashrc"
      candidates+=("${HOME}/.bashrc" "${HOME}/.bash_profile")
      ;;
  esac

  # macOS defaults to zsh; include common shell files so a missing SHELL env
  # does not leave the CLI invisible in the next terminal session.
  candidates+=("${HOME}/.zshrc" "${HOME}/.bashrc" "${HOME}/.bash_profile")

  local seen=":"
  local profile
  for profile in "${candidates[@]}"; do
    [[ "${seen}" == *":${profile}:"* ]] && continue
    seen="${seen}${profile}:"
    [[ -e "${profile}" || "${profile}" == "${primary_profile}" ]] || continue
    touch "${profile}"
    if ! grep -Fq '.local/bin' "${profile}"; then
      {
        echo
        echo "# Ardent CLI"
        echo "${line}"
      } >> "${profile}"
      echo "  ✅ Added ~/.local/bin to PATH in ${profile}"
      updated=1
    fi
  done

  if [[ "${updated}" -eq 0 ]]; then
    echo "  ✅ ~/.local/bin already appears in your shell PATH config."
  fi
}

ensure_path_persisted

echo "Installing optional MCP runtime files..."
curl -fsSL "${REPO_RAW_BASE}/mcp_server.py" -o "${RUNTIME_DIR}/mcp_server.py"
curl -fsSL "${REPO_RAW_BASE}/mcp-tools.json" -o "${RUNTIME_DIR}/mcp-tools.json"
curl -fsSL "${REPO_RAW_BASE}/skills.md" -o "${RUNTIME_DIR}/skills.md"
curl -fsSL "${REPO_RAW_BASE}/openapi.yaml" -o "${RUNTIME_DIR}/openapi.yaml"

# ── Shared helper: patch an mcpServers-style config (Claude, Cursor, Windsurf)
patch_mcp_servers_config() {
  local config_path="${1}"
  local mcp_script="${2}"
  local label="${3}"
  python3 - "${config_path}" "${mcp_script}" "${label}" <<'PYEOF'
import json, sys, pathlib

config_path = pathlib.Path(sys.argv[1])
mcp_script  = sys.argv[2]
label       = sys.argv[3]

try:
    config = json.loads(config_path.read_text(encoding="utf-8"))
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

existing = config.get("mcpServers", {}).get("ardent", {})
existing_env = existing.get("env", {}) if isinstance(existing, dict) else {}
existing_api_key = existing_env.get("ARDENT_API_KEY", "your_api_key_here")

config.setdefault("mcpServers", {})["ardent"] = {
    "command": "python3",
    "args": [mcp_script],
    "env": {"ARDENT_API_KEY": existing_api_key}
}

config_path.parent.mkdir(parents=True, exist_ok=True)
config_path.write_text(json.dumps(config, indent=2), encoding="utf-8")
print(f"  ✅ {label} config updated.")
print(f"  ⚠️  Set your real API key in: {config_path}")
PYEOF
}

# ── Codex ────────────────────────────────────────────────────────────────────
patch_codex_config() {
  local codex_config="${1}"
  local mcp_script="${2}"

  mkdir -p "$(dirname "${codex_config}")"
  touch "${codex_config}"

  if grep -Eq '^\[mcp_servers\.ardent\]' "${codex_config}"; then
    echo "  ✅ Codex config already has an Ardent MCP server entry."
    echo "  ⚠️  Confirm ARDENT_API_KEY is set in: ${codex_config}"
    return
  fi

  cat >> "${codex_config}" <<EOF

[mcp_servers.ardent]
command = "python3"
args = ["${mcp_script}"]

[mcp_servers.ardent.env]
ARDENT_API_KEY = "your_api_key_here"
EOF

  echo "  ✅ Codex config updated."
  echo "  ⚠️  Set your real API key in: ${codex_config}"
}

CODEX_HOME_DIR="${CODEX_HOME:-${HOME}/.codex}"
if [[ -d "${CODEX_HOME_DIR}" || -d "/Applications/Codex.app" || -n "$(command -v codex || true)" ]]; then
  echo "Detected Codex — patching MCP config..."
  patch_codex_config \
    "${CODEX_HOME_DIR}/config.toml" \
    "${RUNTIME_DIR}/mcp_server.py"
else
  echo "Codex not detected — skipping."
fi

# ── Claude Desktop ───────────────────────────────────────────────────────────
CLAUDE_CONFIG_DIR="${HOME}/Library/Application Support/Claude"
if [[ -d "${CLAUDE_CONFIG_DIR}" ]]; then
  echo "Detected Claude Desktop — patching MCP config..."
  patch_mcp_servers_config \
    "${CLAUDE_CONFIG_DIR}/claude_desktop_config.json" \
    "${RUNTIME_DIR}/mcp_server.py" \
    "Claude Desktop"
else
  echo "Claude Desktop not detected — skipping."
fi

# ── ChatGPT Desktop ──────────────────────────────────────────────────────────
CHATGPT_CONFIG_DIR="${HOME}/Library/Application Support/com.openai.chat"
CHATGPT_CONFIG="${CHATGPT_CONFIG_DIR}/mcp_servers.json"

if [[ -d "${CHATGPT_CONFIG_DIR}" ]]; then
  echo "Detected ChatGPT Desktop — patching MCP config..."
  python3 - "${CHATGPT_CONFIG}" "${RUNTIME_DIR}/mcp_server.py" <<'PYEOF'
import json, sys, pathlib

config_path = pathlib.Path(sys.argv[1])
mcp_script  = sys.argv[2]

try:
    config = json.loads(config_path.read_text(encoding="utf-8"))
except (FileNotFoundError, json.JSONDecodeError):
    config = {}

# ChatGPT Desktop: file IS the servers object — no wrapper key
existing = config.get("ardent", {})
existing_env = existing.get("env", {}) if isinstance(existing, dict) else {}
existing_api_key = existing_env.get("ARDENT_API_KEY", "your_api_key_here")

config["ardent"] = {
    "command": "python3",
    "args": [mcp_script],
    "env": {"ARDENT_API_KEY": existing_api_key}
}

config_path.parent.mkdir(parents=True, exist_ok=True)
config_path.write_text(json.dumps(config, indent=2), encoding="utf-8")
print("  ✅ ChatGPT Desktop config updated.")
print("  ⚠️  Set your real API key in:", config_path)
PYEOF
else
  echo "ChatGPT Desktop not detected — skipping."
fi

# ── Cursor ───────────────────────────────────────────────────────────────────
CURSOR_CONFIG_DIR="${HOME}/.cursor"
if [[ -d "${CURSOR_CONFIG_DIR}" ]]; then
  echo "Detected Cursor — patching MCP config..."
  patch_mcp_servers_config \
    "${CURSOR_CONFIG_DIR}/mcp.json" \
    "${RUNTIME_DIR}/mcp_server.py" \
    "Cursor"
else
  echo "Cursor not detected — skipping."
fi

# ── Windsurf ─────────────────────────────────────────────────────────────────
WINDSURF_CONFIG_DIR="${HOME}/.codeium/windsurf"
if [[ -d "${WINDSURF_CONFIG_DIR}" ]]; then
  echo "Detected Windsurf — patching MCP config..."
  patch_mcp_servers_config \
    "${WINDSURF_CONFIG_DIR}/mcp_config.json" \
    "${RUNTIME_DIR}/mcp_server.py" \
    "Windsurf"
else
  echo "Windsurf not detected — skipping."
fi

echo
echo "✅ Install complete"
echo "CLI path: ${INSTALL_DIR}/ardent"
echo "Runtime files: ${RUNTIME_DIR}"
echo
echo "Next steps:"
echo "  1. Replace 'your_api_key_here' with your real key in any config files updated above"
echo "  2. Fully quit and reopen any patched apps (Cmd+Q) — Codex, Claude, ChatGPT, Cursor, Windsurf"
echo "  3. Restart your terminal, or run: export PATH=\"${HOME}/.local/bin:\${PATH}\""
echo "  4. Look for the tools icon/list in your AI app to confirm Ardent is loaded"
echo
echo "Or use the CLI directly:"
echo "  export ARDENT_API_KEY=\"your_api_key\""
echo "  ${INSTALL_DIR}/ardent health"
echo
echo "If 'ardent' is not found, add ~/.local/bin to PATH:"
echo "  export PATH=\"${HOME}/.local/bin:${PATH}\""
