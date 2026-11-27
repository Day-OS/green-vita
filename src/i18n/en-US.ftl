screen-signing-in = SIGNING IN
screen-token-setup = TOKEN SETUP
screen-mode-select = CHOOSE MODE
screen-cloud-titles = CLOUD TITLES
screen-consoles = CONSOLES
screen-connecting = CONNECTING
screen-streaming = STREAMING
screen-paused = QUICK MENU
screen-settings = SETTINGS
screen-error = ERROR

menu-settings = Settings
menu-logout = Log out of Microsoft

settings-cloud = Cloud
settings-home = Home
settings-locale = Locale
settings-color-profile = Color profile: { $mode }
settings-game = Game settings
settings-swap-shoulders-triggers = Swap L1/L2 and R1/R2
settings-swap-shoulders-global = Swap L1/L2 and R1/R2 (all games)
settings-front-touch-auxiliary-buttons = Use front touch for L2/R2/L3/R3 instead of mouse
settings-rear-touch-enabled = Enable rear touch controls
settings-cloud-host = Cloud host: { $host }
settings-home-host = Home host: { $host }
settings-region-bypass-off = Bypass region restriction: Off
settings-region-bypass-ip = Bypass region restriction: { $ip }
settings-region-bypass-enter-ip = Enter bypass IP...
settings-region-bypass-warning = Use this at your own risk: Xbox is told you are in another country, which carries the same risks as using a VPN for xCloud, including breaking Microsoft's terms of use.
settings-region-bypass-pending = Changed: signs in again when you leave Settings (not during a stream).
settings-stream-debug-info = Show streaming debug info
settings-latency-test = Latency test (blinking square + stopwatch)
settings-video-bitrate-cap = Bitrate cap: { $kbps } kbps
settings-video-bitrate-nocap = Bitrate cap: none (unlimited)
settings-video-h264-profile = H264 profile: { $profile }
settings-periodic-keyframe = Periodic keyframe (clear artifacts)
settings-manual-twcc = Manual TWCC feedback
settings-direct-present = Direct present (low latency)
settings-direct-present-sub = Decodes straight into the display buffer. RGBA8888 only (RGB565 falls back to SDL). Turn off to show the debug HUD; adds one frame of latency.
settings-debug-modes = Debug view: Off / FPS+Bitrate / Full (top-right overlay; forces the UI pass).
settings-latency-test = Latency test: blinking square + stopwatch; forces the UI pass (~1 ms).
settings-help = Settings explained
settings-bitrate = Video bitrate cap: 2500 / 5000 / unlimited. Higher = cleaner on a good link, more loss on a weak one.
settings-latency-profile = Latency profile: Fast = lowest floor; Smooth = deeper bank that absorbs bursts (adds latency).
settings-damage-policy = Damage policy: Low latency decodes through damage; Balanced/Quality request a keyframe sooner (fewer artefacts, more IDRs).
settings-repair-window = Repair window: Off = no repair hold, no freeze; Short = ~1 frame; Balanced/Patient hold the queue and can freeze for 300-500 ms.
settings-instream-controls = In-stream controls
settings-ps-button-capture = Capture PS button
settings-ps-button-capture-sub = Tap = Xbox guide · Hold = Vita menu (experimental)
action-back = Back

mode-select-cloud-subtitle = Stream Xbox Cloud Gaming titles
mode-select-home-subtitle = Stream your local Xbox console — Remote Play

paused-resume = Return to game
paused-xbox-button = Xbox button
paused-exit-game = Exit game
paused-xbox-guide = Xbox guide
paused-exit-stream = Exit stream
paused-quit-app = Quit app (LiveArea)
paused-next-stream = next stream
paused-hint-view = View hold = menu
paused-hint-ps = PS tap = guide · PS hold = Vita menu

streaming-hold-back = Hold Back to open the quick menu
streaming-ps-hold-hint = Hold for PS Home
connecting-cancel = Press Circle to cancel
token-starting-sign-in = Starting Microsoft sign-in...
token-at-uri = at { $uri }

title-loading = Loading titles...
title-select-details = Select a title to see details
title-play = ▶ PLAY
title-no-description = No description available
title-search = Search games...
title-search-empty = No games found
screen-language-select = LANGUAGE
language-select-prompt = Choose the application language before signing in.
language-select-continue = Continue to sign in
token-source-warning = Safety notice: Only download GreenVita from GitHub (Day-OS/green-vita) or VitaDB. Be cautious with builds from any other source.
signing-in-starting = Starting GreenVita...
signing-in-requesting-code = Requesting Xbox sign-in...
signing-in-loading-credentials = Signing in to Xbox...
title-requesting = Requesting game list...
title-none-returned = No games were returned
title-token-hint = Check the xCloud authentication and server address
console-requesting = Requesting console list...
console-none-returned = No consoles were returned
console-remote-play-hint = Check that Remote Play is enabled on your Xbox
console-token-hint = Check the Xbox Home authentication and server address
console-account-hint = The console must use the same Microsoft account
connecting-target = Target: { $value }
connecting-session-preparing = Session: preparing
connecting-starting = Starting stream for { $value }
connecting-session = Session: { $value }
connecting-status = Status: { $value }
stream-state-starting = Starting
stream-state-provisioning = Provisioning your session
stream-state-queued-seconds = Queued — estimated wait { $first }s
stream-state-queued-minutes = Queued — estimated wait { $first }m { $seconds }s
stream-state-queued = Queued — waiting for a free server
stream-state-handshake = Finishing sign-in handshake
stream-state-ready = Ready
stream-state-failed = Failed
error-sign-in-request = Sign-in request failed
error-title-request = Game list request failed
error-console-request = Console request failed
error-start-stream = Failed to start stream
error-webrtc-negotiation = WebRTC negotiation failed
error-stream-state = Failed to check stream state
error-webrtc-session = WebRTC session failed
error-stream-ended = Stream session ended
error-technical-details = Technical details: { $error }

error-no-wifi = No Wi-Fi connection detected
error-no-wifi-details = Connect the PS Vita to a Wi-Fi network, then retry.

net-cap-advice = Link check: { $mbps } Mbps, { $rtt } ms RTT — suggested cap: { $kbps } kbps
net-cap-apply = Apply cap
net-cap-dismiss = Dismiss


error-stream-ended-by-console = Stream ended by the console
error-class-auth = Sign-in expired or was rejected — please sign in again
error-class-ratelimit = Too many requests — try again in a few minutes
error-class-server = The Xbox service is having problems — try again later
error-class-timeout = The request timed out — check the Wi-Fi connection
error-class-dns = Could not resolve the server address — check the Wi-Fi connection
error-class-network = Network connection failed — check the Wi-Fi connection

paused-reconnect = Reconnect stream
stalled-banner = Stream stalled while idle — the console stopped sending video. Resume may recover it; otherwise reconnect.
