#!/bin/bash
# Runs the WebSocket benchmark on a fresh Azure VM and deletes it afterwards (bench/websocket/
# README.md "Many cores"). Needs `az` logged in, rsync and ssh.
#
#   bench/websocket/vm/run.sh [suite ...]     suites of bench_vm.py (default: all)
#
# Env: AZ_LOCATION (swedencentral), AZ_SIZE (Standard_D48as_v5), AZ_GROUP (velt-bench-<time>),
# KEEP=1 leaves the VM running (delete the group yourself). The resource group is deleted on exit,
# also when a step fails; an auto-shutdown 4 hours out is the backstop.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
LOCATION="${AZ_LOCATION:-swedencentral}"
SIZE="${AZ_SIZE:-Standard_D48as_v5}"
GROUP="${AZ_GROUP:-velt-bench-$(date +%Y%m%d-%H%M%S)}"
WORK="$(mktemp -d)"
KEY="$WORK/key"
OUT="$HERE/results-$(date +%Y%m%d-%H%M%S)-${SIZE#Standard_}.jsonl"

cleanup() {
  if [ "${KEEP:-0}" = 1 ]; then
    echo "KEEP=1: VM left running in resource group $GROUP" >&2
  else
    echo "deleting resource group $GROUP" >&2
    az group delete -n "$GROUP" --yes --no-wait || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N "" -f "$KEY" -C velt-bench
MYIP="$(curl -fsS https://api.ipify.org)"
az group create -n "$GROUP" -l "$LOCATION" --tags purpose=velt-websocket-bench -o none
IP="$(az vm create -g "$GROUP" -n velt-bench -l "$LOCATION" --size "$SIZE" \
  --image Canonical:ubuntu-24_04-lts:server:latest --admin-username velt \
  --ssh-key-values "$KEY.pub" --public-ip-sku Standard --nsg-rule NONE --os-disk-size-gb 64 \
  --accelerated-networking true --query publicIpAddress -o tsv)"
az network nsg rule create -g "$GROUP" --nsg-name velt-benchNSG -n ssh-from-me --priority 100 \
  --source-address-prefixes "$MYIP/32" --destination-port-ranges 22 --protocol Tcp \
  --access Allow -o none
az vm auto-shutdown -g "$GROUP" -n velt-bench --time "$(date -u -v+4H +%H%M 2>/dev/null || date -u -d '+4 hours' +%H%M)" -o none

SSH=(ssh -i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$WORK/known_hosts"
  -o ServerAliveInterval=30 "velt@$IP")
for _ in $(seq 30); do "${SSH[@]}" true 2>/dev/null && break; sleep 5; done

"${SSH[@]}" 'set -e; sudo apt-get update -qq >/dev/null
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential clang pkg-config python3 unzip curl git >/dev/null
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none >/dev/null 2>&1
  curl -fsSL https://deb.nodesource.com/setup_24.x | sudo -E bash - >/dev/null 2>&1
  sudo apt-get install -y -qq nodejs >/dev/null
  curl -fsSL https://bun.sh/install | bash >/dev/null 2>&1'
rsync -az -e "ssh -i $KEY -o UserKnownHostsFile=$WORK/known_hosts" --exclude target \
  --exclude node_modules --exclude .work --exclude results --exclude .git "$ROOT/" "velt@$IP:velt/"
scp -q -i "$KEY" -o UserKnownHostsFile="$WORK/known_hosts" "$HERE/build.sh" "$HERE/bench_vm.py" "velt@$IP:"
"${SSH[@]}" 'chmod +x build.sh bench_vm.py && ./build.sh'
"${SSH[@]}" 'sudo sysctl -q -w net.ipv4.ip_local_port_range="10000 65000" net.core.somaxconn=65535 net.ipv4.tcp_tw_reuse=1'
"${SSH[@]}" "ulimit -n 1048576; export PATH=\$HOME/.bun/bin:\$PATH; python3 bench_vm.py $*"
"${SSH[@]}" 'cat results.jsonl' > "$OUT"
echo "results: $OUT ($(wc -l < "$OUT") rows)"
