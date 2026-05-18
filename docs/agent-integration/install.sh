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

echo "Installing optional MCP runtime files..."
curl -fsSL "${REPO_RAW_BASE}/mcp_server.py" -o "${RUNTIME_DIR}/mcp_server.py"
curl -fsSL "${REPO_RAW_BASE}/mcp-tools.json" -o "${RUNTIME_DIR}/mcp-tools.json"
curl -fsSL "${REPO_RAW_BASE}/skills.md" -o "${RUNTIME_DIR}/skills.md"
curl -fsSL "${REPO_RAW_BASE}/openapi.yaml" -o "${RUNTIME_DIR}/openapi.yaml"

echo
echo "✅ Install complete"
echo "CLI path: ${INSTALL_DIR}/ardent"
echo "Runtime files: ${RUNTIME_DIR}"
echo
echo "Next steps:"
echo "  export ARDENT_API_KEY=\"your_api_key\""
echo "  export ARDENT_BASE_URL=\"https://api.ardentresearch.xyz\""
echo "  ${INSTALL_DIR}/ardent health"
echo
echo "If 'ardent' is not found, add ~/.local/bin to PATH:"
echo "  export PATH=\"${HOME}/.local/bin:${PATH}\""
