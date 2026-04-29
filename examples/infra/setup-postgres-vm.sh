#!/usr/bin/env bash
# setup-postgres-vm.sh — provision a PostgreSQL server on a Yandex Cloud VM
# via cloud-init/SSH for use as imgdelta metadata backend.
#
# Prerequisites:
#   - yc CLI installed and authenticated (yc init)
#   - SSH key pair: default is ~/.ssh/id_ed25519 / id_ed25519.pub
#   - A YC folder configured (yc config set folder-id <FOLDER_ID>)
#
# Usage:
#   FOLDER_ID=b1g... ./setup-postgres-vm.sh
#   FOLDER_ID=b1g... SUBNET_ID=e9b... ./setup-postgres-vm.sh

set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────────────────

VM_NAME="${VM_NAME:-imgdelta-postgres}"
ZONE="${ZONE:-ru-central1-a}"
FOLDER_ID="${FOLDER_ID:?Please set FOLDER_ID}"
SUBNET_ID="${SUBNET_ID:-}"           # auto-detect if empty
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519.pub}"

PG_VERSION="16"
PG_DB="imgdelta"
PG_USER="${PG_USER:-imgdelta}"
PG_PASSWORD="${PG_PASSWORD:?Please set PG_PASSWORD}"

PLATFORM="standard-v3"
CORES="2"
MEMORY="4GB"
DISK_SIZE="20GB"
IMAGE_FAMILY="ubuntu-2204-lts"

# ── Auto-detect subnet ────────────────────────────────────────────────────────

if [[ -z "$SUBNET_ID" ]]; then
  echo "Detecting subnet in zone $ZONE ..."
  SUBNET_ID=$(yc vpc subnet list \
    --folder-id "$FOLDER_ID" \
    --format json \
    | python3 -c "
import json, sys
subnets = json.load(sys.stdin)
for s in subnets:
    if s.get('zone_id') == '$ZONE':
        print(s['id'])
        break
")
  echo "Using subnet: $SUBNET_ID"
fi

# ── Cloud-init script ─────────────────────────────────────────────────────────

read -r -d '' CLOUD_INIT <<CLOUD_INIT_EOF || true
#cloud-config
package_update: true
package_upgrade: false
packages:
  - postgresql-${PG_VERSION}
  - postgresql-client-${PG_VERSION}

runcmd:
  # Start postgres service (may already be running)
  - systemctl enable postgresql
  - systemctl start postgresql

  # Create user + database
  - |
    sudo -u postgres psql <<SQL
    DO \$\$
    BEGIN
      IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '${PG_USER}') THEN
        CREATE ROLE ${PG_USER} LOGIN PASSWORD '${PG_PASSWORD}';
      END IF;
    END
    \$\$;
    CREATE DATABASE ${PG_DB} OWNER ${PG_USER};
    SQL

  # Allow internal YC network access (10.0.0.0/8)
  - |
    PG_HBA=/etc/postgresql/${PG_VERSION}/main/pg_hba.conf
    echo "host  ${PG_DB}  ${PG_USER}  10.0.0.0/8  scram-sha-256" >> \$PG_HBA
    systemctl reload postgresql

  # Listen on all interfaces (YC internal network is private)
  - |
    PG_CONF=/etc/postgresql/${PG_VERSION}/main/postgresql.conf
    sed -i "s/#listen_addresses = 'localhost'/listen_addresses = '*'/" \$PG_CONF
    systemctl restart postgresql
CLOUD_INIT_EOF

# ── Create VM ─────────────────────────────────────────────────────────────────

echo "Creating VM $VM_NAME in $ZONE ..."

yc compute instance create \
  --name "$VM_NAME" \
  --folder-id "$FOLDER_ID" \
  --zone "$ZONE" \
  --platform "$PLATFORM" \
  --cores "$CORES" \
  --memory "$MEMORY" \
  --create-boot-disk \
    "size=$DISK_SIZE,image-family=$IMAGE_FAMILY,image-folder-id=standard-images" \
  --network-interface \
    "subnet-id=$SUBNET_ID,nat-ip-version=ipv4" \
  --ssh-key "$SSH_KEY" \
  --metadata-from-file "user-data=<(echo \"$CLOUD_INIT\")" \
  --async

echo ""
echo "VM creation started (async). Wait ~2 minutes for cloud-init to finish."
echo ""
echo "Get internal IP:"
echo "  yc compute instance get $VM_NAME --format json | python3 -c \\"
echo "    \"import json,sys; ifaces=json.load(sys.stdin)['network_interfaces'];"
echo "     print(ifaces[0]['primary_v4_address']['address'])\""
echo ""
echo "Test connection (from another VM in the same subnet):"
echo "  psql 'postgres://${PG_USER}:***@<INTERNAL_IP>/${PG_DB}'"
echo ""
echo "DATABASE_URL for imgdelta:"
echo "  postgres://${PG_USER}:<PASSWORD>@<INTERNAL_IP>/${PG_DB}"
