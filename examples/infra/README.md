# imgdelta infrastructure

Two deployment options for the PostgreSQL metadata backend.

---

## Option 1 — Local development via docker-compose

From the repo root:

```sh
# Copy environment template and fill in secrets
cp imgdelta/.env.example .env
$EDITOR .env

# Start postgres (and optionally the main diploma-images container)
docker compose up -d postgres

# Run migrations (once sqlx-cli is installed: cargo install sqlx-cli)
cd imgdelta
DATABASE_URL="postgres://imgdelta:imgdelta@localhost/imgdelta" \
  sqlx migrate run --source image-delta-cli/migrations

# Verify
psql "$DATABASE_URL" -c '\dt'
```

The `diploma-images` service depends on `postgres` (healthcheck), so
`docker compose up` starts postgres first automatically.

---

## Option 2 — Yandex Cloud VM

Use the provided script to provision a Ubuntu 22.04 VM with PostgreSQL 16:

```sh
export FOLDER_ID=<your-yc-folder-id>
export PG_PASSWORD=<strong-password>

# Optional overrides
# export ZONE=ru-central1-b
# export SUBNET_ID=<explicit-subnet-id>
# export SSH_KEY=~/.ssh/id_rsa.pub

./setup-postgres-vm.sh
```

After ~2 minutes the VM is ready. Get its internal IP:

```sh
yc compute instance get imgdelta-postgres \
  --format json | \
  python3 -c "import json,sys; i=json.load(sys.stdin); \
    print(i['network_interfaces'][0]['primary_v4_address']['address'])"
```

Set `DATABASE_URL` in your `.env`:

```
DATABASE_URL=postgres://imgdelta:<PASSWORD>@<INTERNAL_IP>/imgdelta
```

Then run migrations from any machine inside the same YC subnet.

---

## Example config

See [imgdelta-example.toml](imgdelta-example.toml) for a complete `imgdelta` configuration file referencing both local and S3/YC storage backends.
