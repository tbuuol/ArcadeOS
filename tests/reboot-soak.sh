#!/bin/sh
# Neustart-Dauerlauf: startet die VM wiederholt neu, bis sie haengt (oder N Neustarts geschafft sind).
# Je Neustart: Dauer und Zahl der CPU-Resets im QEMU-Log. Bei einem Haenger wird die VM ueber QMP
# abgefragt (Register, Befehlszeiger-Stichproben), solange sie noch haengt; alle Logs werden nach
# test-logs/reboot-soak-<zeit>/ gesichert.
# Aufruf: [QEMU=…] [TCG_THREAD=single] tests/reboot-soak.sh [N=150] [SMP=2] [SSH_PORT=2224]
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
N=${1:-150}; SMPN=${2:-2}; PORT=${3:-2224}
OUT="$ROOT/test-logs/reboot-soak-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT/debug"
VM=""
trap '[ -n "$VM" ] && kill $VM 2>/dev/null' EXIT
S="timeout 20 ssh -p $PORT -i ${KEY:-$HOME/.ssh/aios_ed25519} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3 -o ServerAliveInterval=5 -o ServerAliveCountMax=4 -o BatchMode=yes root@127.0.0.1"
cp "$ROOT/output/images/disk.img" "$OUT/disk.img"
DEBUG_DIR="$OUT/debug" DISK="$OUT/disk.img" VARS="$OUT/vars.fd" SMP=$SMPN SSH_PORT=$PORT \
    "$ROOT/run.sh" -serial "file:$OUT/serial.log" -monitor none -display none >"$OUT/qemu-stderr.log" 2>&1 &
VM=$!
bid() { $S cat /proc/sys/kernel/random/boot_id 2>/dev/null; }
resets() { grep -c 'CPU Reset' "$OUT/debug/qemu.log" 2>/dev/null || echo 0; }
for i in $(seq 1 300); do [ -n "$(bid)" ] && break; sleep 1; done
echo "Start: SMP=$SMPN QEMU=${QEMU:-qemu-system-x86_64} ($("${QEMU:-qemu-system-x86_64}" --version | head -1)) TCG_THREAD=${TCG_THREAD:-multi}, bis zu $N Neustarts, Logs in $OUT" | tee "$OUT/soak.txt"
for r in $(seq 1 "$N"); do
    b=$(bid); r0=$(resets); t0=$(date +%s)
    $S reboot >/dev/null 2>&1
    got=0
    for i in $(seq 1 300); do
        sleep 1; n=$(bid); [ -n "$n" ] && [ "$n" != "$b" ] && { got=1; break; }
    done
    d=$(( $(date +%s) - t0 )); dr=$(( $(resets) - r0 ))
    if [ $got = 1 ]; then
        echo "$r $(date +%H:%M:%S) ok ${d}s resets=$dr load=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/soak.txt"
    else
        if kill -0 "$VM" 2>/dev/null; then what="HAENGER (QEMU laeuft noch)"; else what="QEMU ABGESTUERZT"; fi
        echo "$r $(date +%H:%M:%S) $what nach ${d}s resets=$dr load=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/soak.txt"
        journalctl -k --since "-15min" 2>/dev/null | grep -i qemu > "$OUT/kernel-qemu.txt"
        python3 -I "$ROOT/tools/qmp.py" "$OUT/debug/qmp.sock" dump 10 > "$OUT/hang-qmp.txt" 2>&1
        python3 -I "$ROOT/tools/qmp.py" "$OUT/debug/qmp.sock" hmp "info mtree -f" > "$OUT/hang-mtree.txt" 2>&1
        echo "Haenger gesichert: $OUT/hang-qmp.txt" | tee -a "$OUT/soak.txt"
        rm -f "$OUT/disk.img"
        exit 1
    fi
done
rm -f "$OUT/disk.img"
echo "Ergebnis: $N Neustarts ohne Haenger" | tee -a "$OUT/soak.txt"
