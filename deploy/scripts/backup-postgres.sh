#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
compose_file="$repository_root/docker-compose.prod.yml"
environment_file="$repository_root/deploy/.env.production"
backup_directory="$repository_root/deploy/backups"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup_file="$backup_directory/agent-exec-v2-$timestamp.dump"

if [[ ! -f "$environment_file" ]]; then
    echo "Missing $environment_file" >&2
    exit 1
fi

umask 077
mkdir -p "$backup_directory"

docker compose --env-file "$environment_file" -f "$compose_file" \
    exec -T postgres sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" --format=custom --no-owner --no-privileges' \
    > "$backup_file"

test -s "$backup_file"
echo "Postgres backup written to $backup_file"
