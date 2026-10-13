#!/bin/bash
# Creates an Azure VM for chatbench (and the WebSocket benchmark), installs the toolchains, copies
# this checkout and builds everything. Prints the IP and an ssh helper path; delete the resource
# group when done (`az group delete -n <group> --yes`).
#
#   bench/chat/vm/up.sh <resource-group>
#
# Env: AZ_LOCATION (swedencentral), AZ_SIZE (Standard_D48as_v5), SSH_KEY (default: a new key in
# the work directory printed at the end). An auto-shutdown 5 hours out is the backstop.
set -euo pipefail
GROUP="$1"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
LOCATION="${AZ_LOCATION:-swedencentral}"
SIZE="${AZ_SIZE:-Standard_D48as_v5}"
WORK="${WORK:-$(mktemp -d)}"
KEY="${SSH_KEY:-$WORK/key}"
[ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N "" -f "$KEY" -C velt-bench
MYIP="$(curl -fsS https://api.ipify.org)"
az group create -n "$GROUP" -l "$LOCATION" --tags purpose=velt-bench -o none
IP="$(az vm create -g "$GROUP" -n velt-bench -l "$LOCATION" --size "$SIZE" \
  --image Canonical:ubuntu-24_04-lts:server:latest --admin-username velt \
  --ssh-key-values "$KEY.pub" --public-ip-sku Standard --nsg-rule NONE --os-disk-size-gb 64 \
  --accelerated-networking true --query publicIpAddress -o tsv)"
az network nsg rule create -g "$GROUP" --nsg-name velt-benchNSG -n ssh-from-me --priority 100 \
  --source-address-prefixes "$MYIP/32" --destination-port-ranges 22 --protocol Tcp \
  --access Allow -o none
az vm auto-shutdown -g "$GROUP" -n velt-bench \
  --time "$(date -u -v+5H +%H%M 2>/dev/null || date -u -d '+5 hours' +%H%M)" -o none
SSH="$WORK/ssh.sh"
cat > "$SSH" <<EOF
#!/bin/bash
exec ssh -i $KEY -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=$WORK/known_hosts -o ServerAliveInterval=30 velt@$IP "\$@"
EOF
chmod +x "$SSH"
for _ in $(seq 30); do "$SSH" true 2>/dev/null && break; sleep 5; done
"$SSH" 'set -e; sudo apt-get update -qq >/dev/null
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential clang pkg-config python3 unzip curl git >/dev/null
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none >/dev/null 2>&1
  curl -fsSL https://deb.nodesource.com/setup_24.x | sudo -E bash - >/dev/null 2>&1
  sudo apt-get install -y -qq nodejs >/dev/null
  curl -fsSL https://bun.sh/install | bash >/dev/null 2>&1
  curl -fsSL https://go.dev/dl/go1.27.1.linux-amd64.tar.gz | sudo tar -C /usr/local -xz
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq linux-tools-common "linux-tools-$(uname -r)" >/dev/null 2>&1 || true
  sudo sysctl -q -w net.ipv4.ip_local_port_range="10000 65000" net.core.somaxconn=65535 net.ipv4.tcp_tw_reuse=1 kernel.perf_event_paranoid=-1'
rsync -az -e "ssh -i $KEY -o UserKnownHostsFile=$WORK/known_hosts" --exclude target \
  --exclude node_modules --exclude .work --exclude results --exclude .git --exclude chat-go \
  "$ROOT/" "velt@$IP:velt/"
"$SSH" 'cp velt/bench/chat/vm/bench_chat.py velt/bench/websocket/vm/bench_vm.py . &&
  bash velt/bench/chat/vm/build.sh > build.log 2>&1 && bash velt/bench/websocket/vm/build.sh >> build.log 2>&1 && tail -3 build.log'
echo "VM ready: $IP   ssh helper: $SSH   group: $GROUP"
