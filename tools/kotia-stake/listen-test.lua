-- raw P2P test against the listener: version, then things an attacker might try, then one block request
local MAGIC = "\x3d\xb5\x80\x00"
local function sha256d_hex(bin)
  local f = io.open("/tmp/claude-0/-/fb8a538f-f08a-4f73-8db4-f16869453c77/scratchpad/m.bin", "wb"); f:write(bin); f:close()
  local h = io.popen("sha256sum m.bin | cut -c1-64 | xxd -r -p | sha256sum | cut -c1-8"):read("l")
  return h
end
local function unhex(h) return (h:gsub("..", function(x) return string.char(tonumber(x, 16)) end)) end
local function le32(n) return string.pack("<I4", n) end
local function msg(cmd, payload)
  return MAGIC .. cmd .. string.rep("\0", 12 - #cmd) .. le32(#payload) .. unhex(sha256d_hex(payload)) .. payload
end
local function rev(hex) local t = {} for i = #hex - 1, 1, -2 do t[#t + 1] = hex:sub(i, i + 1) end return table.concat(t) end
local best = arg[1]
local version = string.pack("<i4", 70013) .. string.rep("\0", 8) .. string.pack("<i8", os.time()) .. string.rep("\0", 52) .. "\1\2\3\4\5\6\7\8" .. "\0" .. le32(0) .. "\1"
local fake_tx = unhex("0100000000000000" .. "01" .. string.rep("11", 32) .. "00000000" .. "00" .. "ffffffff" .. "01" .. "00e1f50500000000" .. "1976a914" .. string.rep("22", 20) .. "88ac" .. "00000000")
local inv_tx = "\1" .. le32(1) .. unhex(string.rep("33", 32))
local out = msg("version", version) .. msg("verack", "")
  .. msg("tx", fake_tx) .. msg("inv", inv_tx) .. msg("mempool", "") .. msg("getdata", inv_tx)
  .. msg("sendrawtx", "x") .. msg("getdata", "\1" .. le32(2) .. unhex(rev(best)))
io.open("out.bin", "wb"):write(out)
