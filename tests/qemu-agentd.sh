#!/bin/sh
# Testet agentd (MCP ueber SSH) in QEMU: initialize, tools/list, tools/call, Fehlerfaelle,
# Transaktion ueber MCP (config_set + config_confirm).
# Aufruf: tests/qemu-agentd.sh   (KEY=... fuer anderen SSH-Key). Dauer ca. 1–2 min.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "$ROOT/tests/lib.sh"

start_vm
echo "== Start"
wait_ssh && ok "SSH" || { bad "kein SSH"; tail -20 $SP/serial.log; exit 1; }

echo "== MCP-Sitzung (eine SSH-Verbindung, wie ein Agent sie oeffnet)"
python3 -I - "$KEY" <<'PY' > $SP/mcp.out
import json, subprocess, sys
key = sys.argv[1]
p = subprocess.Popen(["timeout", "120", "ssh", "-i", key, "-p", "2222", "-o", "StrictHostKeyChecking=no",
                      "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-o", "BatchMode=yes",
                      "root@127.0.0.1", "agentd"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
def send(obj, expect_reply=True):
    p.stdin.write((obj if isinstance(obj, str) else json.dumps(obj)) + "\n"); p.stdin.flush()
    return json.loads(p.stdout.readline()) if expect_reply else None
def call(i, name, args=None):
    r = send({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args or {}}})
    res = r["result"]; return res, json.loads(res["content"][0]["text"])
out = {}
out["init"] = send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}})
send({"jsonrpc": "2.0", "method": "notifications/initialized"}, expect_reply=False)
out["ping"] = send({"jsonrpc": "2.0", "id": 2, "method": "ping"})
out["list"] = send({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
out["status"] = call(4, "system_status")
out["logs"] = call(5, "logs", {"source": "audit", "lines": 5})
out["set"] = call(6, "config_set", {"values": {"hostname": "via-mcp"}})
out["confirm"] = call(7, "config_confirm")
out["badtool"] = send({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "rm_rf", "arguments": {}}})
out["badmethod"] = send({"jsonrpc": "2.0", "id": 9, "method": "resources/list"})
out["parse"] = send("{kaputt")
out["badkey"] = call(10, "config_set", {"values": {"x;reboot": "1"}})
p.stdin.close(); p.wait()
print(json.dumps(out))
PY
R=$(cat $SP/mcp.out)
[ -n "$R" ] && ok "Sitzung lief durch" || { bad "keine Antwort"; finish; exit 1; }
q() { python3 -I -c "import json,sys; d=json.loads(sys.argv[1]); print(eval(sys.argv[2]))" "$R" "$1" 2>&1; }
check "$(q 'd["init"]["result"]["serverInfo"]["name"]')" "arcadeos-agentd" "initialize: Server meldet sich"
check "$(q 'd["init"]["result"]["protocolVersion"]')" "2025-06-18" "Protokollversion"
check "$(q 'd["ping"]["result"]')" "{}" "ping"
check "$(q 'len(d["list"]["result"]["tools"])')" "7" "tools/list: 7 Werkzeuge"
check "$(q 'd["list"]["result"]["tools"][0]["inputSchema"]["properties"]')" "{}" "leeres Schema ist ein Objekt"
check "$(q 'd["status"][1]["release"]["channel"]')" "dev" "system_status: Kanal"
check "$(q 'd["status"][1]["update"]["running"]')" "A" "system_status: laufender Slot"
check "$(q 'd["status"][1]["state"]["mode"]')" "persistent" "system_status: Zustands-Partition"
check "$(q 'd["status"][1]["memory"]["total_kb"] > 100000')" "True" "system_status: Speicher"
check "$(q 'd["logs"][1]["source"]')" "audit" "logs"
check "$(q 'd["set"][1]["pending"]')" "True" "config_set ueber MCP: Transaktion offen"
check "$(q 'd["confirm"][1]["confirmed"]')" "True" "config_confirm ueber MCP"
check "$(q 'd["badtool"]["error"]["code"]')" "-32602" "unbekanntes Werkzeug -> Fehler"
check "$(q 'd["badmethod"]["error"]["code"]')" "-32601" "unbekannte Methode -> Fehler"
check "$(q 'd["parse"]["error"]["code"]')" "-32700" "kaputtes JSON -> Parse error"
check "$(q 'd["badkey"][0]["isError"]')" "True" "Shell-Sonderzeichen im Schluessel abgelehnt"
check "$($S hostname)" "via-mcp" "Hostname ueber MCP gesetzt"
$S 'grep -q "set hostname=via-mcp" /state/log/audit.log' && ok "MCP-Aenderung im Audit-Log" || bad "nicht im Audit-Log"

finish
