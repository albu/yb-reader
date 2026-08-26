--[[ Screen Mirror — show a Mac window on the Kindle, tap to page.

Mac side: yb-mirror/mac/server.py (uv run mac/server.py --app Safari …)
Left third = /prev (← arrow key on the Mac), rest = /next (→).
Swipe down (or any vertical swipe) to exit.

Also carries "Fetch book from Mac": with yb-mirror/mac/send.py serving one
file on the Mac (uv run mac/send.py book.epub), Tools → Fetch book from
Mac streams it into /documents — the Mac-side server exits by itself once
the file is acked as received.

Networking, in order of what it buys:
  - one persistent HTTP/1.1 connection: no TCP handshake per page turn,
    real timeouts (3 s connect / 5 s io) so a dead Mac can't freeze the UI;
  - a lightweight /ping every 25 s keeps that connection through the
    server's 30 s idle reap and notices a sleeping Mac early;
  - UDP-broadcast autodiscovery: the Mac answers probes on udp/<port+1>,
    the reply's source address becomes SERVER=… in mirror.conf — a DHCP
    address change self-heals on the next tap, no configuration needed;
  - if the server's 3 s wait-for-change ends unchanged (slow site), poll
    with plain GETs until the frame's X-Seq moves — never re-press the key,
    which would skip a page.
]]

local Device = require("device")
local Dispatcher = require("dispatcher")
local GestureRange = require("ui/gesturerange")
local ImageWidget = require("ui/widget/imagewidget")
local InfoMessage = require("ui/widget/infomessage")
local InputContainer = require("ui/widget/container/inputcontainer")
local UIManager = require("ui/uimanager")
local WidgetContainer = require("ui/widget/container/widgetcontainer")
local ffi_util = require("ffi/util")
local logger = require("logger")
local PluginShare = require("pluginshare")
local socket = require("socket")
local _ = require("gettext")

local Screen = Device.screen

-- KOReader owns the radio on Kindle; sockets just fail if Wi-Fi dropped
-- (USB sessions often leave it off). Guarded require for builds without it.
local ok_nm, NetworkMgr = pcall(require, "ui/network/manager")
local ensure_wifi = ok_nm and NetworkMgr or nil

local CONF_PATH = "/mnt/us/extensions/mirror/mirror.conf"
local FRAME_PATH = "/tmp/mirror_frame.png"
local DEFAULT_PORT = 8765
local DISCOVER_PORT = DEFAULT_PORT + 1 -- udp; reply carries the tcp port
local FETCH_PORT = DEFAULT_PORT + 2    -- mac/send.py, the one-file sender
local SAVE_DIR = "/mnt/us/documents"   -- where fetched files land
local CONNECT_TIMEOUT = 3              -- s; a dead Mac must not stall a tap
local REQUEST_TIMEOUT = 5              -- s; server waits at most 3 s
local PING_EVERY = 15                  -- s; server reaps idle conns at 30 s,
                                       -- and a warmer radio doesn't drop the
                                       -- first packet after idle

-- ffi_util.gettime is monotonic wall-clock seconds; fall back to os.time
-- (1 s resolution) rather than crash a tap on builds without it.
local gettime = ffi_util.gettime or os.time

-- Timings land here so the page-turn budget can be read off-device:
-- the server log has activate/site+capture/encode, this has the Kindle's
-- share (Wi-Fi + HTTP); the rest of a felt turn is PNG decode + the e-ink
-- partial refresh.
local function plog(...)
    local parts = {}
    for i = 1, select("#", ...) do parts[i] = tostring(select(i, ...)) end
    local f = io.open("/mnt/us/extensions/mirror/plugin.log", "a")
    if f then
        f:write(string.format("[%s] %s\n", os.date("%H:%M:%S"),
                              table.concat(parts, " ")))
        f:close()
    end
end

-- ---------------------------------------------------------------- conf ---

-- Returns server, refresh_every (nil when absent; 0 disables the periodic
-- anti-ghosting flash — tune it in mirror.conf without re-copying the plugin).
local function readServerConf()
    local f = io.open(CONF_PATH, "r")
    if not f then return nil, nil end
    local server, refresh_every
    for line in f:lines() do
        local s = line:match("^%s*SERVER%s*=%s*(%S+)")
        if s then server = s end
        local r = tonumber(line:match("^%s*REFRESH_EVERY%s*=%s*(%d+)"))
        if r then refresh_every = r end
    end
    f:close()
    return server, refresh_every
end

-- Remember a discovered address so the next start connects without the
-- ~1 s discovery round. Other conf lines are preserved.
local function writeServerConf(server)
    local lines = {}
    local f = io.open(CONF_PATH, "r")
    if f then
        for line in f:lines() do
            if not line:match("^%s*SERVER=") then lines[#lines + 1] = line end
        end
        f:close()
    end
    lines[#lines + 1] = "SERVER=" .. server
    f = io.open(CONF_PATH, "w")
    if f then
        f:write(table.concat(lines, "\n"), "\n")
        f:close()
    end
end

-- "http://192.0.2.1:8765" / "192.0.2.1:8765" / "192.0.2.1" /
-- "mybook.local:8765" -> host, port. Note: [%d%.] alone would be wrong for
-- hostnames and a bare %d+ pattern silently never matches an IP with dots
-- (the exact bug that killed the conf fast path once) — split on the colon
-- instead of pattern-matching the address shape.
local function parseServer(s)
    s = (s or ""):gsub("^http://", "")
    s = s:match("^%s-(.-)%s-$") or ""
    local host, port = s:match("^([^:]+):(%d+)$")
    if not host or host == "" then
        host, port = s ~= "" and s or nil, nil
    end
    return host, tonumber(port or DEFAULT_PORT)
end

-- ------------------------------------------------------- fetch (send.py) ---

-- %XX -> byte, '+' -> space; byte-transparent afterwards so UTF-8 file
-- names (X-Filename arrives percent-encoded) survive verbatim.
local function urldecode(s)
    s = s:gsub("+", " ")
    return (s:gsub("%%(%x%x)", function(h)
        return string.char(tonumber(h, 16))
    end))
end

-- A server-supplied name still only ever becomes one file inside SAVE_DIR:
-- basename, no control bytes, nothing hidden/dot-like left.
local function sanitizeFetchName(name)
    name = (name or ""):gsub("\\", "/")
    name = name:match("([^/]+)$") or ""
    name = name:gsub("[%c]", "")
    name = name:gsub("^%.(.+)", "%1")
    if name == "" or name == "." or name == ".." then return nil end
    return name
end

-- Small-body GET (status lines, ack): returns the body string or nil.
local function getBody(conn, path)
    local parts, n = {}, 0
    local status = conn:request("GET", path, function(chunk)
        n = n + #chunk
        parts[#parts + 1] = chunk
        return 1
    end)
    if status ~= 200 then return nil end
    if n > 65536 then return nil end
    return table.concat(parts)
end

-- ---------------------------------------------------------- discovery ---

-- Broadcast a probe; the first ybmirror server to answer wins. The Mac's
-- IP is the reply's *source address* (nothing in the payload to misparse),
-- its tcp port comes from the reply text. Tries the limited broadcast
-- first, then the local /24 — some APs only forward subnet-directed ones.
local function discover(timeout_s)
    timeout_s = timeout_s or 1.0
    local targets = { "255.255.255.255" }
    local u = socket.udp()
    u:settimeout(0)
    if u:setpeername("192.0.2.1", 9) then -- TEST-NET: picks a route, sends nothing
        local ip = u:getsockname()
        if ip and ip ~= "0.0.0.0" then
            local sub = ip:gsub("%d+$", "255")
            if sub ~= "255.255.255.255" then targets[#targets + 1] = sub end
        end
    end
    u:close()
    for _, bcast in ipairs(targets) do
        local s = socket.udp()
        s:settimeout(timeout_s)
        pcall(s.setoption, s, "broadcast", true)
        local sent = s:sendto("ybmirror-discover", bcast, DISCOVER_PORT)
        local data, ip
        if sent then data, ip = s:receivefrom() end
        s:close()
        if data and ip then
            local port = tonumber(data:match("ybmirror%s+(%d+)")) or DEFAULT_PORT
            return ip, port
        end
    end
    return nil
end

-- ------------------------------------------------------------ client ---

local Conn = {}
Conn.__index = Conn

local function connNew(host, port)
    return setmetatable({ host = host, port = port, sock = nil }, Conn)
end

function Conn:close()
    if self.sock then
        pcall(self.sock.close, self.sock)
        self.sock = nil
    end
end

function Conn:open()
    self:close()
    local s = socket.tcp()
    s:settimeout(CONNECT_TIMEOUT)
    local ok, err = s:connect(self.host, self.port)
    if not ok then
        s:close()
        return nil, err
    end
    s:settimeout(REQUEST_TIMEOUT)
    self.sock = s
    return true
end

-- One HTTP/1.1 request on the kept-alive socket, streaming the body to
-- sink(chunk). Returns status (number), headers (lower-cased keys), stage.
-- On failure status is nil and stage says where it broke; the socket is
-- closed so the next request reconnects cleanly. Stages matter for
-- retries: "connect"/"send" mean the Mac never saw the request (safe to
-- repeat even for a page turn), the read stages mean it may have.
function Conn:request(method, path, sink)
    if not self.sock then
        local ok = self:open()
        if not ok then return nil, nil, "connect" end
    end
    local s = self.sock
    local req = string.format(
        "%s %s HTTP/1.1\r\nHost: %s:%d\r\nConnection: keep-alive\r\n" ..
        "Content-Length: 0\r\n\r\n", method, path, self.host, self.port)
    local ok = s:send(req)
    if not ok then
        self:close()
        return nil, nil, "send"
    end
    local line = s:receive("*l")
    local status = line and tonumber(line:match("^HTTP/%d%.%d%s+(%d+)"))
    if not status then
        self:close()
        return nil, nil, "status"
    end
    local headers = {}
    while true do
        line = s:receive("*l")
        if not line then
            self:close()
            return nil, nil, "headers"
        end
        if line == "" then break end
        local k, v = line:match("^(.-):%s*(.-)%s*$")
        if k then headers[k:lower()] = v end
    end
    local clen = tonumber(headers["content-length"] or "") or -1
    if clen < 0 then
        -- keep-alive without Content-Length would desync the stream; the
        -- server always sends it, so this is a protocol break
        self:close()
        return nil, nil, "headers"
    end
    if sink and clen > 0 then
        local remaining = clen
        while remaining > 0 do
            local chunk = s:receive(math.min(16384, remaining))
            if not chunk then
                self:close()
                return nil, nil, "body"
            end
            sink(chunk)
            remaining = remaining - #chunk
        end
    end
    return status, headers
end

-- -------------------------------------------------------------- view ---

local MirrorView = InputContainer:extend({
    name = "MirrorView",
    is_modal = true,
    covers_fullscreen = true,
    server = nil,      -- "http://ip:port" from conf or discovery
    conn = nil,        -- persistent Conn
    image_widget = nil,
})

function MirrorView:init()
    self.dimen = Screen:getSize()
    self.server, self.refresh_every = readServerConf()
    -- Keep the device awake while mirroring (the AutoSuspend plugin honours
    -- this flag), so the screensaver doesn't interrupt a reading session.
    PluginShare.pause_auto_suspend = true
    self.ges_events = {
        Tap = { GestureRange:new({ ges = "tap", range = self.dimen }) },
        Swipe = { GestureRange:new({ ges = "swipe", range = self.dimen }) },
        TwoFingerTap = { GestureRange:new({ ges = "two_finger_tap", range = self.dimen }) },
    }
end

function MirrorView:getSize()
    return self.dimen
end

function MirrorView:paintTo(bb, x, y)
    if self.image_widget then
        self.image_widget:paintTo(bb, x, y)
    end
end

-- Connect, using the remembered address if it still works, discovering the
-- Mac otherwise (missing conf, DHCP change, laptop woke on a new network).
function MirrorView:ensureConn()
    if self.conn then return self.conn end
    -- No radio, no mirror: sockets against a down Wi-Fi just time out and
    -- read as "Mac not found". Re-enable quietly — the Kindle remembers
    -- its AP (this blocks a few seconds, only on the rare reconnect).
    if ensure_wifi and ensure_wifi.isWifiOn and not ensure_wifi:isWifiOn() then
        plog("wifi down — turning it back on")
        pcall(function() ensure_wifi:turnOnWifi() end)
    end
    if self.server then
        local host, port = parseServer(self.server)
        if host then
            local conn = connNew(host, port)
            if conn:open() then
                self.conn = conn
                self.host, self.port = host, port
                return conn
            end
            plog("conf server unreachable:", self.server)
        end
    end
    local ip, port = discover(1.0)
    if ip then
        local conn = connNew(ip, port)
        if conn:open() then
            self.conn = conn
            self.host, self.port = ip, port
            self.server = string.format("http://%s:%d", ip, port)
            writeServerConf(self.server)
            plog("discovered Mac at", self.server)
            return conn
        end
        plog("discovered", ip, "but connect failed")
    else
        plog("discovery: no server answered (conf:",
             tostring(self.server), ")")
    end
    return nil
end

-- Fresh-connection liveness probe for after a failed turn: distinguishes
-- "the Mac is gone" (worth a message) from "one packet got lost" (worth
-- silence). A live connection is kept as the new self.conn.
function MirrorView:_probeAlive()
    if not self.host then return false end
    local conn = connNew(self.host, self.port or DEFAULT_PORT)
    if not conn:open() then return false end
    local status = conn:request("GET", "/ping")
    if status == 200 then
        self.conn = conn
        return true
    end
    conn:close()
    return false
end

-- Frame parameters travel on every frame-bearing request (also the page
-- turn POST), so the server is (re)configured even if it restarted
-- mid-session. bpp=4: e-ink only shows 16 gray levels, so a 4-bit PNG
-- halves the frame size and cuts Wi-Fi transfer time.
function MirrorView:frameQuery()
    return string.format("w=%d&h=%d&bpp=4",
                         Screen:getWidth(), Screen:getHeight())
end

-- Stream one request's body to FRAME_PATH. Returns the response headers
-- table, or nil (after one safe retry).
function MirrorView:_fetch(method, path)
    local conn = self:ensureConn()
    if not conn then return nil end
    local out = io.open(FRAME_PATH, "wb")
    if not out then return nil end
    local nbytes = 0
    local sink = function(chunk)
        if chunk then nbytes = nbytes + #chunk return out:write(chunk) end
        return 1
    end
    local t0 = gettime()
    local status, headers, stage = conn:request(method, path, sink)
    if not status then
        -- Retrying is safe for GETs, and for POSTs because page turns
        -- carry an idempotency key: the server answers a repeated id with
        -- the current frame instead of pressing the key again.
        status, headers, stage = conn:request(method, path, sink)
    end
    out:close()
    plog(string.format("%s %s %.0fms %dB status=%s stage=%s",
                       method, path, (gettime() - t0) * 1000, nbytes,
                       tostring(status), tostring(stage)))
    if status ~= 200 then return nil end
    return headers
end

-- force_full: a "full" refresh flashes black/white — good for clearing
-- e-ink ghosting, annoying to watch on every turn. Normal turns are always
-- partial; a tap in the top-right corner cleans the screen on demand, and
-- a distant safety net (every REFRESH_EVERY frames, default 60, 0 = never)
-- keeps residue from accumulating forever if nobody ever does.
function MirrorView:showFrame(force_full)
    self.image_widget = ImageWidget:new({
        file = FRAME_PATH,
        file_do_cache = false,
        alpha = false,
    })
    self.frame_count = (self.frame_count or 0) + 1
    local every = self.refresh_every
    local refresh = "partial"
    if force_full or self.frame_count == 1
       or (every ~= 0 and self.frame_count % (every or 60) == 0) then
        refresh = "full"
    end
    UIManager:setDirty(self, refresh)
end

function MirrorView:_noServer()
    UIManager:show(InfoMessage:new({
        text = _("Mirror: Mac not found — is the server running?")
            .. "\n" .. _("will retry on next tap"),
        timeout = 3,
    }))
end

function MirrorView:refresh()
    if not self:_fetch("GET", "/frame.png?" .. self:frameQuery()) then
        self:_noServer()
        return
    end
    self:showFrame()
end

function MirrorView:turn(endpoint)
    if self.busy then return end
    self.busy = true
    self:_turn(endpoint)
    self.busy = false
end

function MirrorView:_turn(endpoint)
    -- Idempotency key: one id per tap, reused across retries, so the server
    -- can tell "retry of a turn I may already have done" from "new turn".
    self.turn_seq = (self.turn_seq or 0) + 1
    local headers = self:_fetch(
        "POST", string.format("%s?wait=1&id=%d&%s",
                              endpoint, self.turn_seq, self:frameQuery()))
    if not headers then
        -- Transient radio loss mid-connection used to raise the "Mac not
        -- found" modal right here, mid-reading. Instead: probe the Mac on
        -- a fresh connection; if it's alive, quietly show the *current*
        -- frame (which also reveals a turn that did land after all) and
        -- leave the reader in peace.
        if self:_probeAlive() then
            plog("turn failed, Mac alive — transient, showing current frame")
            if self:_fetch("GET", "/frame.png?" .. self:frameQuery()) then
                self:showFrame()
                return
            end
        end
        self.conn = nil -- force full reconnect (+ re-discovery) next time
        self:_noServer()
        return
    end
    -- Not done yet: either the site hadn't changed within the server's wait
    -- window (slow site), or it changed but hadn't settled — chapter
    -- boundaries render a loading screen first, and shipping that as final
    -- left the Kindle a page behind Safari. Poll with plain GETs (never a
    -- re-POST — that would skip a page) until the server reports settled.
    if headers["x-changed"] == "0" or headers["x-settled"] == "0" then
        local deadline = gettime() + 8
        while gettime() < deadline do
            socket.sleep(0.5)
            local h = self:_fetch("GET", "/frame.png?" .. self:frameQuery())
            if not h or h["x-settled"] ~= "0" then break end
        end
    end
    self:showFrame()
end

function MirrorView:onTap(_, ges)
    if self.busy then return true end
    if not ges or not ges.pos then return true end
    -- Screen-clean lives in the top-right corner. It used to be a
    -- long-press, but e-ink page turns take ~0.5 s to become visible, so a
    -- finger resting "waiting for the turn" crossed the hold threshold and
    -- turned into an accidental flash-and-no-page-turn — duration-based
    -- gestures don't survive slow feedback. A corner can't be tapped by
    -- accident while page-turning.
    if ges.pos.x > Screen:getWidth() * 0.85
       and ges.pos.y < Screen:getHeight() * 0.12 then
        self.busy = true
        if self:_fetch("GET", "/frame.png?" .. self:frameQuery()) then
            self:showFrame(true)
        end
        self.busy = false
        return true
    end
    local endpoint = "/next"
    if ges.pos.x < Screen:getWidth() / 3 then
        endpoint = "/prev"
    end
    self:turn(endpoint)
    return true
end

-- Two-finger tap opens the frontlight dialog without leaving the mirror.
function MirrorView:onTwoFingerTap()
    Device:showLightDialog()
    return true
end

function MirrorView:onSwipe(_, ges)
    if not ges then return true end
    if ges.direction == "east" then
        if not self.busy then self:turn("/prev") end
    elseif ges.direction == "west" then
        if not self.busy then self:turn("/next") end
    else -- down/up/anything else: exit, even mid-sync
        UIManager:close(self)
    end
    return true
end

-- Keep-alive heartbeat: holds the socket through the server's idle reap
-- and keeps the Kindle's radio in light power-save (it never idles deeper
-- than PING_EVERY, so a tap never meets a cold radio). A ping that meets
-- a dozing radio must NOT tear the connection down while its reply is in
-- flight — that closes with unread data (TCP reset, guaranteed dead
-- socket), which is exactly how the old heartbeat killed connections it
-- was meant to keep. So: one retry through a fresh connection, and only
-- give up after both fail.
function MirrorView:onShow()
    UIManager:setDirty(self, "full")
    if not self._ping_chain then
        self._ping_chain = true
        local function tick()
            if not self._ping_chain then return end
            if self.conn and self.conn.sock then
                local ok = self.conn:request("GET", "/ping") == 200
                if not ok and self._ping_chain then
                    ok = self.conn:request("GET", "/ping") == 200 -- reconnects
                end
                if not ok then
                    plog("keepalive: two pings failed — dropping connection")
                    self.conn:close()
                end
            end
            UIManager:scheduleIn(PING_EVERY, tick)
        end
        tick()
    end
    return true
end

function MirrorView:onCloseWidget()
    PluginShare.pause_auto_suspend = false
    self._ping_chain = false
    if self.conn then self.conn:close() end
    UIManager:setDirty(self, "full")
    return true
end

local ScreenMirror = WidgetContainer:extend({
    name = "mirror",
})

function ScreenMirror:_log(...)
    plog(...)
end

function ScreenMirror:init()
    self:_log("plugin init")
    local ok, err = pcall(function()
        self.ui.menu:registerToMainMenu(self)
    end)
    if ok then
        self:_log("registered to main menu OK")
    else
        self:_log("registerToMainMenu FAILED:", err)
    end
end

function ScreenMirror:addToMainMenu(menu_items)
    menu_items.mirror = {
        text = _("Screen mirror (Mac)"),
        sorting_hint = "tools",
        callback = function()
            self:startMirror()
        end,
    }
    menu_items.fetch = {
        text = _("Fetch book from Mac"),
        sorting_hint = "tools",
        callback = function()
            self:fetchFile()
        end,
    }
    menu_items.probe = {
        text = _("Run hardware probe"),
        sorting_hint = "tools",
        callback = function()
            self:runProbe()
        end,
    }
end

-- The yb-reader project ships a static aarch64 probe binary + KUAL
-- extension — but this device has no KUAL. This device does have this
-- menu, so the probe gets launched from here instead: same start.sh,
-- same output (/mnt/us/probe.out), no launcher dependency.
function ScreenMirror:runProbe()
    local script = "/mnt/us/extensions/probe/bin/start.sh"
    if not io.open(script, "r") then
        UIManager:show(InfoMessage:new({
            text = _("probe not installed\n(missing ") .. script .. ")",
            timeout = 4,
        }))
        return
    end
    local msg = InfoMessage:new({ text = _("Probing hardware…") })
    UIManager:show(msg)
    UIManager:forceRePaint()
    local rc = os.execute("sh " .. script)
    UIManager:close(msg)
    local size = -1
    local f = io.open("/mnt/us/probe.out", "r")
    if f then
        size = f:seek("end")
        f:close()
    end
    plog("probe: rc=" .. tostring(rc) .. " probe.out=" .. tostring(size) .. "B")
    local ok = (rc == 0 or rc == true) and size > 0
    UIManager:show(InfoMessage:new({
        text = ok and (_("Probe done — probe.out ") .. size .. _(" B"))
            or (_("Probe failed (rc=") .. tostring(rc)
                .. _(", out=") .. tostring(size) .. _(" B) — see plugin.log")),
        timeout = 5,
    }))
end

-- Expose the mirror to KOReader's Dispatcher so it can be bound to a gesture
-- (or a profile) and launched straight from the start screen / anywhere.
function ScreenMirror:onDispatcherRegisterActions()
    Dispatcher:registerAction("screen_mirror", {
        category = "none",
        event = "ScreenMirror",
        title = _("Screen mirror (Mac)"),
        general = true,
    })
    -- Also surface as a SimpleUI Quick Action (home screen tile), if SimpleUI
    -- is installed. This event fires after all plugins load, so the module
    -- is require-able even though "mirror" sorts before "simpleui".
    local ok_sui, QA = pcall(require, "sui_quickactions")
    if ok_sui and QA and QA.register then
        QA.register({
            id = "screen_mirror",
            label = _("Screen mirror (Mac)"),
            is_in_place = true,
            is_async_in_place = true, -- MirrorView outlives the execute() call
            execute = function()
                self:startMirror()
            end,
        })
    end
end

function ScreenMirror:onScreenMirror()
    self:startMirror()
end

function ScreenMirror:startMirror()
    logger.dbg("screenmirror: starting")
    local view = MirrorView:new({})
    UIManager:show(view)
    view:refresh()
end

-- Fetch the one file mac/send.py is holding. Delivery semantics: the file
-- counts as received only after we read the full body *and* rename it into
-- place; only then does the /ack go out, which is the server's signal to
-- exit. A body read that dies mid-stream (the Kindle radio's signature
-- move) gets one same-server retry — safe, because nothing was acked and
-- send.py re-serves from scratch.
function ScreenMirror:_fetchFrom(host, port)
    local conn = connNew(host, port)
    if not conn:open() then return nil, "connect" end
    local body = getBody(conn, "/status")
    if not body then conn:close() return nil, "no status" end
    local name = sanitizeFetchName(urldecode(
        body:match('"file"%s*:%s*"(.-)"') or ""))
    local size = tonumber(body:match('"size"%s*:%s*(%d+)'))
    -- /status also exists on the mirror server (different JSON): without
    -- file+size this isn't send.py, keep looking.
    if not name or not size then conn:close() return nil, "not send.py" end

    local final_path = SAVE_DIR .. "/" .. name
    local part_path = final_path .. ".part"
    local t0 = gettime()
    local status
    for _ = 1, 2 do
        local out = io.open(part_path, "wb")
        if not out then conn:close() return nil, "cannot write" end
        status = conn:request("GET", "/book", function(chunk)
            return out:write(chunk)
        end)
        out:close()
        if status == 200 then break end
        os.remove(part_path)
        if status == 410 then conn:close() return nil, "already delivered" end
    end
    if status ~= 200 then conn:close() return nil, "download failed" end

    local saved = os.rename(part_path, final_path)
    conn:request("GET", "/ack") -- best effort; lets the server exit
    conn:close()
    if not saved then return nil, "rename failed" end
    plog(string.format("fetch: saved %s (%d B) from %s:%d in %.1fs",
                       name, size, host, port, gettime() - t0))
    return string.format("%s\n%s (%.1f MB)", _("Saved to documents:"),
                         name, size / 1048576.0)
end

function ScreenMirror:_fetchBook()
    if ensure_wifi and ensure_wifi.isWifiOn and not ensure_wifi:isWifiOn() then
        plog("fetch: wifi down — turning it back on")
        pcall(function() ensure_wifi:turnOnWifi() end)
    end
    -- Candidate addresses, most likely first: send.py next to the saved
    -- mirror address, send.py at a freshly discovered Mac, and whatever
    -- port discovery reported last (a send.py started with --port).
    local cands = {}
    local conf_server = readServerConf()
    if conf_server then
        local h = parseServer(conf_server)
        if h then cands[#cands + 1] = { h, FETCH_PORT } end
    end
    local dip, dport = discover(1.0)
    if dip then
        cands[#cands + 1] = { dip, FETCH_PORT }
        cands[#cands + 1] = { dip, dport or DEFAULT_PORT }
    end
    local seen = {}
    for _, cand in ipairs(cands) do
        local key = cand[1] .. ":" .. cand[2]
        if not seen[key] then
            seen[key] = true
            local ok, r1, r2 = pcall(self._fetchFrom, self,
                                      cand[1], cand[2])
            if ok and r1 then return r1 end
            plog("fetch: " .. key .. " — " .. tostring(ok and r2 or r1))
        end
    end
    return nil, _("Mac not found — run send.py there first")
end

function ScreenMirror:fetchFile()
    local msg = InfoMessage:new({ text = _("Fetching from Mac…") })
    UIManager:show(msg)
    UIManager:forceRePaint() -- paint before the blocking socket work
    local saved, err = self:_fetchBook()
    UIManager:close(msg)
    UIManager:show(InfoMessage:new({
        text = saved or (_("Fetch failed") .. "\n" .. (err or "?")),
        timeout = 4,
    }))
    if saved and self.ui and self.ui.onRefresh then
        pcall(function() self.ui:onRefresh() end) -- new book, rescan FM
    end
end

return ScreenMirror
