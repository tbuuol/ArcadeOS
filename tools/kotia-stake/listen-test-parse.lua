local b = io.open("resp.bin", "rb"):read("a")
local i, seen = 1, {}
while true do
  local j = b:find("\x3d\xb5\x80\x00", i, true); if not j then break end
  local cmd = b:sub(j + 4, j + 15):gsub("%z.*", "")
  local len = string.unpack("<I4", b, j + 16)
  print(("received %-9s %7d bytes"):format(cmd, len))
  i = j + 24 + len
end
