# Testnet V2 Oracle Deployment

This directory deploys the existing Testnet V2 backend to one Oracle Cloud
Always Free ARM VM. The Rust application starts its HTTP API and background
workers in the same container. Caddy is the only public service; PostgreSQL and
Redis remain private to Docker's internal network.

## Before migration

1. Copy every Railway environment variable to a secure password manager.
2. Preserve the exact `WALLET_ENCRYPTION_KEY`. It decrypts existing agent
   signing keys and the database-backed paymaster signer.
3. Drain queued execution requests and stop Railway before making the database
   dump. Running both backends against different databases would split state.
4. Export Railway PostgreSQL with `pg_dump --format=custom --no-owner
   --no-privileges` and retain an encrypted copy until the migration is proven.

## Oracle VM

Create an Ubuntu ARM `VM.Standard.A1.Flex` instance in your Oracle home region.
Allow only ports 80 and 443 publicly. Restrict SSH to your own IP or use OCI
Bastion. Never open 5432, 6379, or 8080 to the internet.

Install Docker Engine and the Compose plugin, then clone the `V2` branch:

```bash
git clone --branch V2 https://github.com/ardentairesearch/agent-execution-platform.git
cd agent-execution-platform
cp deploy/.env.production.example deploy/.env.production
chmod 600 deploy/.env.production
```

Fill in `deploy/.env.production` using the preserved Railway values. Use
hex-only values from `openssl rand -hex 32` for `POSTGRES_PASSWORD` and
`REDIS_PASSWORD`; this keeps the internally assembled connection URLs valid.
Point `api.ardentresearch.xyz` directly at the VM with a DNS `A` record. The
included Caddy configuration is the trusted edge proxy and writes the private
client-IP header used to limit public API-key creation. Do not put a CDN or
another proxy in front of it unless its client-IP header is explicitly
validated and configured.

## Restore and start

Start only Postgres, copy the Railway dump to the container, and restore it:

```bash
docker compose --env-file deploy/.env.production -f docker-compose.prod.yml up -d postgres
docker cp /path/to/agent-exec-v2-backup.dump ardent-testnet-v2-postgres-1:/tmp/railway.dump
docker compose --env-file deploy/.env.production -f docker-compose.prod.yml \
  exec postgres pg_restore -U agent_exec -d agent_exec --clean --if-exists --no-owner /tmp/railway.dump
```

Build and start the full stack:

```bash
docker compose --env-file deploy/.env.production -f docker-compose.prod.yml up -d --build
docker compose --env-file deploy/.env.production -f docker-compose.prod.yml logs -f api
```

Confirm the API logs say it loaded the existing paymaster signer, then check:

```bash
curl --fail https://api.ardentresearch.xyz/health
```

Point `api.ardentresearch.xyz` to the Oracle VM only after Railway has stopped
accepting traffic and the restored backend has passed an existing API-key,
wallet-read, simulation, and low-value execution check. Caddy obtains and
renews TLS certificates automatically after DNS points at the VM.

## Backups

Create a local restrictive-permission dump with:

```bash
./deploy/scripts/backup-postgres.sh
```

Upload backups to a separate encrypted storage location and test restoring one
before treating the migration as complete. The script intentionally does not
delete old backups; choose and document a retention policy before automating
cleanup.
