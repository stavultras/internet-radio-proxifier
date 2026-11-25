# AGENTS.md

Guidance for AI coding agents working in this repository. Read it before
changing behavior, build files, or the browser UI. It was distilled from the
maintainer's development sessions; items marked **maintainer rule** are
explicit requests, not suggestions.

## Ground rules (the ones most often broken)

1. **Test locally first. Do not push, tag, or trigger CI unless asked.**
   Ordinary pushes do not run CI; releases happen only when the maintainer
   says so.
2. **The version stays `0.1`.** Re-release by moving the `v0.1` tag, never by
   bumping `VERSION` on your own.
3. **No backward-compatibility shims.** When renaming, rename everything
   (paths, UCI, init, CI, README, packaging) and drop the old name.
4. **Local builds = Filogic 820 only** (`aarch64_cortex-a53` apk + ipk)
   **plus a source tarball**, with standard OpenWrt names — not the full
   `build.sh` output, not the GitHub release names.
5. **Secrets:** never write real proxy credentials yourself (the maintainer
   enters them), never echo a proxy URL/password in errors, logs, docs or
   commits, and never extract or reuse the credentials stored on the router.
   The settings-page password is set only by hand-editing `settings.ini`.
6. **Install on the router only when asked.** Kill background test servers
   by PID, never `pkill -f` with a pattern that matches your own command.
7. The maintainer reviews **pixel alignment** closely and iterates on small
   UI fixes. Check the player at desktop and phone widths before calling UI
   work done.

## What this is

Internet Radio Proxifier is a self-hosted web radio player. A single static
Rust binary (~2 MB, a few MB of RAM) relays internet radio streams through
the machine that runs it (a PC, a server, or an OpenWrt router), optionally
through a SOCKS5 or HTTP(S) proxy **per station**. Browsers only talk to this
server; every asset they need is embedded in the binary, so the player page
also works when the machine itself has no internet access (only the stations
need it).

- Rust 2024, axum 0.8, tokio (current-thread runtime), rustls/ring.
- Browser UI: plain HTML/CSS/JS plus bundled htmx 1.9.12 and its WS
  extension. No Node/npm build step, no CDN, no web fonts (the CSS names
  `Berkeley Mono, monospace` and falls back to the system monospace font).
- Installable as a PWA; Media Session gives lock-screen/headset controls.
- Distribution: Linux (static musl, x86_64/aarch64), Windows x64, and
  OpenWrt 25.x `.apk` / 24.10 `.ipk` for aarch64 (cortex-a53/a72/a76,
  generic), x86_64, mipsel_24kc and mips_24kc.
- Upstream: <https://github.com/stavultras/internet-radio-proxifier>.
  It started as a modified copy of <https://github.com/pinpox/radio> (Go),
  then was rewritten in Rust for size (12.8 → 1.8 MB binary, ~24 → ~1.6 MB
  RSS); the Go version was removed entirely and must not come back.

## Commands

`cargo` is not on the default PATH in this environment:
`export PATH="$HOME/.cargo/bin:$PATH"` first.

```sh
cargo build --release                  # native release binary
cargo test --locked                    # 19 unit tests, all offline
cargo run -- -a 127.0.0.1 -p 19876 -c /tmp/radio-settings.ini
curl -sS http://127.0.0.1:19876/ | head
./build.sh                             # all release packages into dist/
./build.sh linux-x86_64 linux-aarch64 windows-x86_64
SDK=/path/to/openwrt-sdk ./build.sh openwrt
```

Notes:

- Rust 1.85+ is required (`edition = "2024"`).
- The rustfmt/clippy **components are not installed** here (the rustup
  proxies exist but fail). If you install them, run them on touched code
  only; **do not reformat the whole tree** — it is intentionally
  hand-formatted with some long lines, and a global `cargo fmt` would create
  a large unrelated diff.
- `./build.sh` needs rustup targets and cross compilers listed at the top of
  the script. The `openwrt` target additionally needs `SDK=` (for `apk` and
  `fakeroot`); MIPS additionally needs the pinned nightly
  (`nightly-2026-11-09`, keep in step with CI) and downloads OpenWrt
  toolchains into `~/.cache/openwrt-toolchains` on first use.
- Never test with a real settings file from a router; use a temporary one
  and a non-default port.

## Repository layout

```
src/
  main.rs        app state, routes, websockets, settings API, relay, CLI
  http.rs        hand-written HTTP/1.1 client + SOCKS5/HTTP(S) proxies
  stations.rs    INI settings format: parse, validate, render, atomic save
  audio.rs       real bitrate from MP3/AAC frame headers
assets/
  templates/     index.html (player), settings.html (settings page)
  static/        CSS, JS, htmx, icons, manifest, service worker
openwrt/
  files/         procd init, UCI config, setup.sh (shared install step)
  make-apk.sh    OpenWrt 25.x apk builder (SDK host tools only)
  make-ipk.sh    OpenWrt 24.10 ipk builder (OpenWrt's ipkg-build)
  internet-radio-proxifier/Makefile   package for building inside the SDK
build.sh         all release packages -> dist/ (git-ignored)
docs/            README screenshots (keep in sync with the UI)
.github/workflows/build.yml            build/test/release CI
VERSION          the only place the release version is set
settings.example.ini                   credential-free starter stations
```

## Server architecture

`App` in `src/main.rs` is the whole state: stations
(`RwLock<Arc<Vec<Station>>>`), proxy raw string plus parsed form, three
feature flags (`autoplay`, `random_start`, `random`), `retries`, the settings
file path, websocket sessions, song-title and bitrate caches (both keyed by
**stream URL**), an in-flight set for title fetches, and a save lock.

### Routes

| Route | Purpose |
| --- | --- |
| `GET /` | player page (`{{url}}`/`{{autoplay}}` placeholders filled in) |
| `GET /ws` | player websocket (htmx WS extension) |
| `GET /api/stations` | station picker data, **in play order** (favorites first) |
| `POST /api/stations/remove` | remove one station by name from the player picker; saves and remaps listeners |
| `GET /static/{path}` | embedded assets |
| `GET /sw.js`, `GET /manifest.webmanifest` | same assets at root, for PWA scope |
| `GET /settings` | settings page (optional Basic auth) |
| `GET/PUT /settings/api/stations` | read/save settings; stations in **file order** |
| `POST /settings/api/check` | test one stream, optional proxy |
| `GET /station/{N}` | relayed stream of station `N`, **1-based** |

All responses get `Cache-Control: no-cache` from the `no_cache` middleware.
Embedded files have no mtime, so without it browsers keep old JS after an
upgrade.

### Websocket protocol

Client messages are JSON `{action, index}`:

| action | effect |
| --- | --- |
| `next` / `previous` | step through `play_order()`; with `random` on, pick any station but the current one |
| `select` | jump to `index` (0-based, range-checked) |
| `favorite` | toggle the favorite of the session's **current** station and save immediately |

The server pushes HTML fragments (`#station-tags`, `#station-name`,
`#station-title`, `#station-fav`, `#audio-source`); the htmx WS extension
swaps them **by id, replacing the elements**. Two consequences:

- **Never cache DOM references** to those ids in JS — re-query after every
  swap (see `updateMediaSessionMetadata` in `script.js`).
- On a station switch the server sends the station info **before**
  `player_html`, so the name shows immediately (see below).

A new session starts at station 0 and gets its info right away. The server
pings every 30 s and drops a session after 75 s of silence, so vanished
clients stop being polled for titles. `TCP_NODELAY` is set on accepted
sockets (`tap_io`) and outbound connections: without it, small WS updates
waited ~43 ms for ACKs; with it the name arrives in ~1 ms. Keep both.

### Station numbering, ordering, favorites

- Station numbers are **1-based everywhere users see them** (player `#N`,
  picker, settings rows, `/station/N`); internal indices are 0-based.
  `/station/0` must stay a 404. `App::station_url(i)` adds 1; there is a unit
  test (`station_urls_count_from_one`).
- Numbers are positions in the settings file. Reordering on the settings
  page renumbers them; the settings rows show numbers via a **CSS counter**
  (`settings.css`), not JS.
- Favorites change the **play order only**, not the numbers:
  `stations::play_order()` returns favorites first, then the rest, each in
  file order. Next/Previous and the picker follow that order.
- **The player star saves on click.** `favorite` → `toggle_favorite()`
  writes the file immediately (under `save_lock`, re-reading the password)
  and broadcasts the new star. The maintainer explicitly asked for this
  after a star was lost on refresh — do not make favorites depend on the
  settings page's Save button.
- **The picker removes stations by name.** `POST /api/stations/remove`
  (`{name}` JSON) calls `stations::remove()`, saves immediately like the
  star, and `remap_sessions()` moves listeners. The last station is
  refused server-side and its × is disabled in the picker. It works without
  the settings password (the player is the open page); the JSON body keeps
  a cross-site form from triggering it.
- The last station is restored in the browser **by name**, not by position
  in `/api/stations` (that list is play-ordered): `loadStationNames()` places
  each name at its own `index`. A bug here once made a star look lost after
  reload — don't regress it.
- `remap_sessions()` matches listeners to the new list **by stream URL**
  after a save/reorder and re-sends info + source when the index changed;
  listeners whose station was removed move to the next station that is
  still there, or to the last one when the removed station was last.

### Settings file (`settings.ini`)

- The INI file is the single source of truth; there is no database.
- **The station name is the section header** (`[ROCK FM]`), not a key.
  Per-station keys: `url`, `use_proxy`, `favorite`. Global keys (`password`,
  `proxy_url`, `autoplay`, `random_start`, `random`, `retries`) must appear
  **before the first section**; there is no `[DEFAULT]` and no `:` separator.
- Values: `"""…"""`, backticks and single/double quotes are literal;
  otherwise the first `#`/`;` starts a comment. Booleans accept
  `1/t/true/y/yes/on` (case-insensitive). `retries` parses as u32, clamped to
  `MAX_RETRIES` (10); an invalid value keeps the default (3).
- `validate()`: at least one station; names trimmed, non-empty, no
  `[`/`]`/line breaks, **unique case-insensitively**; URLs must be http(s).
- **Saving rewrites the whole file** from the parsed state (`render()`):
  hand-written comments and formatting are lost. The maintainer asked about
  this and accepted it — do not "fix" it with a comment-preserving parser.
  `render()` always writes the four option keys; `password`/`proxy_url` only
  when non-empty; values are quoted by `ini_quote()` as needed.
- **The settings-page password can only be set by editing the file.** Never
  add a UI field for it, never render it, never overwrite it on save (both
  `settings_put` and `toggle_favorite` re-read it from disk). Username is
  `admin`; `check_auth` reads the file on **every request**, so a hand edit
  takes effect without a restart. An empty password means the page is open.
- Saving is atomic (temp file `.settings-<pid>-<nanos>.ini` in the same
  directory, `create_new`, `sync_all`, rename) and mode `0600` on Unix
  because the file holds proxy passwords. Keep both.
- The proxy password is sent to the browser only as `********`
  (`mask_proxy`); `unmask_proxy` restores the saved value when the mask comes
  back. Proxy parse/save errors must never echo the proxy URL. Tests cover
  the mask round-trip (`proxy_mask`, `ini_round_trip`).
- `settings.example.ini` must stay credential-free. A missing file is
  created from it at startup (parent dirs too, mode `0600`); startup fails
  if the parsed station list is empty ("No stations found").

### HTTP client and proxies

`src/http.rs` is deliberately hand-written: HTTP/1.1 only, ICY responses
(`ICY 200 OK`), redirects (301/302/303/307/308, up to 10, http(s) only),
chunked/content-length/EOF bodies, SOCKS5, HTTP and HTTPS proxies. It also
provides `base64` and URL parsing (userinfo is percent-decoded; a stream URL
with `user:pass@` gets Basic auth). Do not replace it with `reqwest`/`hyper`
unless the maintainer asks: the point is a small, auditable binary with few
dependencies.

- `USER_AGENT` is required: some stream hosts (e.g. SomaFM) reject unknown
  clients.
- SOCKS default port is 1080; schemes are `socks5`, `socks5h`, `http`,
  `https`. **Both** SOCKS schemes send the hostname to the proxy (DNS on the
  proxy); username/password auth (method 2) is used when a user is present.
- Plain HTTP proxy: absolute-form request + `Proxy-Authorization`. HTTPS
  target: `CONNECT` (reply read byte-by-byte so no tunnel bytes are
  consumed). HTTPS proxy: TLS to the proxy first.
- TLS roots: `SSL_CERT_FILE`, then `/etc/ssl/certs/ca-certificates.crt`,
  `/etc/ssl/cert.pem`, `/etc/pki/tls/certs/ca-bundle.crt`; Windows falls back
  to bundled `webpki-roots`. No roots → a warning, https stations fail; the
  OpenWrt package depends on `ca-bundle`.
- `UnexpectedEof` on a body read is treated as EOF (stream servers close TLS
  without `close_notify`). Every request sends `Connection: close`.
- Per-station proxying is driven by `use_proxy`; `proxy_for()` returns `None`
  for stations without it. The settings page refuses to tick a station's
  proxy box while the proxy field is empty, and the check endpoint reports
  "proxy is ticked but no proxy is set".

### Titles and bitrate

- `title_loop` fetches every 5 s, **only for stations someone is listening
  to** (`listened()`), deduplicated per stream URL (`inflight`). Fetching all
  stations once caused a ~5 s name delay; never reintroduce it.
- Each fetch has a 10 s timeout; on error/timeout the **last cached title is
  kept**. Caches are keyed by stream URL and survive station switches, so
  returning to a station shows its last title immediately.
- ICY metadata has no declared charset: decode UTF-8, otherwise
  Windows-1252 (`decode_title`, CP1252 table for 0x80–0x9F) — this fixed
  names like `Wölli`. `clean_title()` turns `Title~Artist~…` (≥4 parts) into
  "Artist - Title".
- Bitrate is **measured from MP3/AAC frames** (`audio.rs`: chained frame
  headers, ≥4 frames, longest chain wins; AAC ADTS rate = frame length × 8 ×
  sample rate / 1024). `~173` means VBR. Fall back to `icy-br` /
  `ice-audio-info` only when no frame chain is found; stations lie about
  their bitrate (one claimed 256, measured ~158–173).
- Streams without `icy-metaint` have no title; 32 KB (`MEASURE_BYTES`) is
  read to measure the bitrate.

### Stream relay and check

- `GET /station/N`: 10 s connect timeout; any failure (DNS, connect, non-2xx,
  timeout) → **502** with a log line, so the player moves on quickly.
  Content-Type falls back to `audio/mpeg` when missing, empty or
  `application/octet-stream`. Audio is relayed in 16 KiB chunks through a
  4-slot channel; the upstream read stops as soon as the listener goes away
  (`tx.closed()`), even if the station stalls.
- `POST /settings/api/check`: 15 s deadline, reads 64 KB (~4 s of 128 kbps
  audio — catches streams that burst and stall), requires status 200, and
  reports `ok/status/contentType/bitrate/icyName/error`. Too slow →
  "too slow to play: X KB/s". The measured bitrate overrides the icy one.

## Player UI contracts (do not regress)

- **Name before stream.** On a switch the server sends station info before
  the new `<source>`; the name shows immediately, not after the first audio
  buffer.
- **The source says which station it is.** `#audio-source` carries
  `data-station` (the stream URL) as well as `src` and `data-index`. A
  removal can keep the number and the `/station/N` URL while the station
  changes, so the player must compare `data-station` too; otherwise the
  removed stream keeps playing under the next station's name.
- **No layout jump.** `#station-name` and `#station-title` reserve a line
  (`min-height: 1.3em`).
- **Status line is real state:** connecting, playing, buffering, retrying,
  unavailable, skipped, paused, "No connection to the radio server". Never
  print "Connected" before audio plays (the maintainer tested on a router
  with no internet and was misled by exactly that). The flags
  `stationStarted`, `failedStation`, `gaveUp`, `retrying` exist so pause /
  buffering events don't overwrite red error messages — keep them.
- **Autoplay:** browsers block playback without a gesture. `data-autoplay`
  only requests it; `NotAllowedError` starts `startOnFirstGesture()`
  (pointerdown/keydown/touchstart) and is the **only** thing that disables
  auto-skip until the first click. Don't treat it as a station failure.
- **Retries:** `retries` is 0–10 (default 3). A station that **started and
  then stopped** reconnects after 3 s × attempt (3, 6, 9, …) before being
  skipped; one that never started is skipped immediately. The counter
  refills after 5 s of good playback and on a source change. With `random`
  on, a failed station is replaced by a random one (no memory of failures —
  a random pick can return to a previously failed station; that is intended).
- **Auto-skip:** a 1 s watchdog skips a station stalled for 12 s
  (`SKIP_STALL_MS`), errored or ended. It gives up after **one full round**
  of failing stations (`stationNames.length`) with "No station can be played
  right now…". Manual Next/Previous and picker selects reset the counter;
  pausing disables auto-skip.
- **Picker:** title shows the count — "(448)", or "(12 of 448)" while
  filtering. The filter matches names or an **exact station number** ("12"
  finds #12, not #112; a leading `#` is stripped); Enter plays the exact
  number match first. Rows show number, favorite star, name, proxy tag; the
  current station gets `aria-current` and is scrolled into view. Each row
  also has a **×** remove button: it confirms, removes through the API, and
  reloads the list; the last remaining station's × is disabled. After a
  removal, keep the picker and page scroll position and focus the row that
  took the removed one's place — never focus the first row (browsers scroll
  the focused element into view, which jumps the list to the top).
- **Random button** picks client-side, never the current station, needs ≥2
  stations. `random_start` (server-side) only applies with autoplay, on the
  first connect.
- **Persistence:** localStorage keys `internet-radio-proxifier-volume` and
  `internet-radio-proxifier-last-station` (stored **by name** on every source
  change, restored on every `wsOpen`). Volume is re-applied on
  loadstart/loadedmetadata/playing (upstream issue #3: it reset on station
  change).
- **PWA / Media Session:** `sw.js` is a deliberate no-op pass-through — it
  exists only so Chrome treats the site as installable; live streams have
  nothing to cache, don't add caching. `icon-192.png`/`icon-512.png` are used
  by the manifest and Media Session artwork — keep them even though the
  favicon is now `favicon.png`/`favicon.svg` (black on light themes,
  aquamarine on dark; the Apple icon is inverted).
- **Everything is local:** no external fonts, icons, scripts, analytics or
  images. A new asset must be committed under `assets/static/` **and listed
  in the `assets!` macro** — `include_bytes!` only covers listed names.

## Settings page contracts

- Two tabs, Stations and Settings, with Undo and Save on both; Save writes
  stations and options together and returns the new config (the page
  re-renders from it). Listeners get the new list right away
  (`remap_sessions`). The open tab lives in the URL hash (`#settings`).
- Save/Undo are disabled unless dirty (`isDirty()` compares JSON);
  `beforeunload` warns about unsaved changes. Undo restores the last saved
  state.
- Stations tab tools: **Add**, per-row **test**, **Test all**, **Test all
  without proxy** (report-only, changes nothing), **Proxy the failing**
  (tests all directly, tests the failing ones via proxy, ticks the ones that
  only work through it and **unticks** stations that work directly; ends
  with "Review and Save"). Tests run 6 at a time with "Testing X of N…"
  progress; results show ✓/✗, `[via proxy]`/`[without proxy]`, and
  content-type · bitrate · icy-name. Failed bulk tests report station
  numbers ("#3, #17").
- The global "for all stations: proxy" checkbox ticks/unticks every row and
  shows an indeterminate state when only some are ticked. Without a proxy,
  unticked boxes are disabled (a ticked one can still be unticked, so
  clearing the proxy never locks the UI).
- Reordering uses a drag grip (pointer capture, swap past a neighbour's
  middle, auto-scroll near window edges) and arrow keys when the grip is
  focused — not up/down buttons.
- Validation errors `reveal()` the tab/field that caused them (proxy format,
  empty names, non-http(s) URLs, proxy ticked but unset). `random_start` is
  disabled unless Autoplay is on; the retries input clamps to 0–10.
- Fetches are built from `location.origin` so the page also works when its
  URL contains `user:pass@`.

## Web UI / CSS conventions

- Plain JS, no framework, no build step. Keep it dependency-free.
- `style.css` has `body > *:not(:last-child) { display: block; … }`, which
  overrides `hidden` and flex on direct body children. Use higher
  specificity (`.tab-panel[hidden]`, `body.settings …`) instead of relying
  on the attribute.
- Hash-only navigation (`/settings` → `/settings#settings`) does not reload
  the page. When testing CSS, call `location.reload()`.
- Alignment anchors the maintainer checks (desktop): corner tags
  `#station-tags` sit 12.5 px from top/left/right, level with the
  Previous/Next buttons **including their 3D shadow**; the button row has
  `padding-right: 5px` to compensate half the shadow; the favorite star sits
  at right 8.3 px / bottom 8 px so its glyph lines up with the volume "100%"
  above it (the odd offsets compensate the glyph's own bearing — see the
  comment in `style.css`). Keep these edges aligned when touching CSS.
- Phone width (≤700 px): buttons stack vertically, the volume row is
  `visibility: hidden` (keeps its space). The needs-gesture pulse respects
  `prefers-reduced-motion`.
- Settings spacing: 16 px under the tabs, status line margins 13/8 px
  (`settings.css`); the invisible spacer buttons in `.list-head` exist so
  the header lines up with the rows' proxy boxes — don't remove them.

## Coding conventions

- Match the surrounding compact style: 4 spaces in Rust, tabs in HTML/CSS/JS.
- Comments explain **why**, not what; the existing code is sparsely commented
  and that is intentional. Public items get `///` docs where they clarify a
  contract.
- Prefer std: the project rolls its own base64, URL parsing, random number
  (`random_below`, based on `RandomState` + clock), HTTP client and bitrate
  parser to keep the binary around 2 MB. Adding a crate needs a clear
  size/complexity justification.
- Error messages are user-facing and specific, except proxy credentials:
  never include a URL/userinfo in an error or log line.
- Use `AtomicUsize` for IDs/counters, not `AtomicU64`: 32-bit MIPS has no
  64-bit atomics and must build.
- Keep the `tokio` current-thread runtime and `max_blocking_threads(4)`;
  the target is a router with 512 MB RAM. Avoid spawning per-connection
  blocking work.
- Release profile (`opt-level = "z"`, LTO, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`) is deliberate; the binary is already
  stripped (extra `strip` saves ~248 bytes; UPX and nightly build-std were
  considered and rejected). Changes that grow the binary need a reason.
- HTML escaping goes through `esc()`; station names are user input.
- CLI: `-a/--address` and `-p/--port` are required (port 1–65535),
  `-c/--config` optional, `-h` is help (not host), `-V` prints the `VERSION`
  file; `--flag=value` and `[::1]` bracket forms are accepted. `::` falls
  back to `0.0.0.0` when IPv6 is unavailable. `Cargo.toml` keeps
  `version = "0.0.0"` on purpose. Default config:
  `/etc/internet-radio-proxifier/settings.ini` on Unix, `settings.ini` on
  Windows.
- Commit messages: imperative summary, optionally prefixed with the area
  (`Player:`, `Settings:`, `CI:`, `OpenWrt:`), then a short body explaining
  the change and its reason. **Do not commit unless asked.**

## Build and release

### Version

- `VERSION` is the only release version; `-V` and all packages read it.
- The project is at `0.1` and the maintainer has repeatedly re-released
  `v0.1` instead of bumping (the Go era used 0.2/0.4; the GitHub repo
  restarted at 0.1). Change `VERSION` only when asked.

### CI

`.github/workflows/build.yml` runs when:

1. a push to `main` changes `VERSION` — the only push trigger; markdown and
   all other files are already excluded (`paths` and `paths-ignore` cannot
   be combined on one event, so push keeps `paths: [VERSION]`),
2. a `v*` tag is pushed,
3. a pull request changes anything but markdown
   (`pull_request: paths-ignore: ['**.md']`), or
4. it is started manually (`gh workflow run build.yml`).

The workflow builds every package, runs unit tests, starts every binary
(MIPS and aarch64 under qemu), tests the Windows zip on a Windows runner
(including an https station through the relay), and publishes a GitHub
Release when the tag matches `v<VERSION>` (or a VERSION change on main has
no tag yet). **Pull requests and manual runs never release** — the version
job only sets `release=true` for a matching tag or a VERSION change on
main; otherwise the packages stay workflow artifacts.

- Ubuntu jobs are pinned to `ubuntu-24.04` **by the maintainer's choice**
  after repeated runner DNS failures ("Keep the pin"). The qemu package is
  `qemu-user` on newer Ubuntu and `qemu-user-static` on older; the workflow
  tries both. Don't unpin.
- Release assets: `internet-radio-proxifier-<V>-linux-<arch>.tar.gz`,
  `...-windows-x86_64.zip`, `...-openwrt-<arch>.apk` and `.ipk`.

Re-releasing the same version (**maintainer rule**: this is the normal flow):

```sh
gh release delete v0.1 --yes --cleanup-tag
git tag -d v0.1 2>/dev/null
git tag v0.1
git push origin v0.1
gh run watch <run-id> --exit-status
```

The release should contain **17 assets** (2 Linux + 1 Windows + 7 OpenWrt
architectures × apk/ipk).

### ImageBuilder naming — settled, don't re-fix

The apk v3 index has no filename field, so the ImageBuilder only finds
`internet-radio-proxifier-<V>-r1.apk`, not the release name with the
architecture in it. The maintainer chose a **README note telling users to
rename** the file; extra archives and renaming the release assets were
explicitly rejected ("archives are excess"). Leave it as documented.

### Local OpenWrt packages (maintainer workflow)

When the user asks for local builds, they want **only** the Filogic 820
(`aarch64_cortex-a53`) packages plus a source tarball — not the full
`build.sh` output. Use standard OpenWrt names, not the GitHub release names
(the maintainer corrected this once):

- `internet-radio-proxifier-<V>-r1.apk`
- `internet-radio-proxifier_<V>-r1_aarch64_cortex-a53.ipk`
- `internet-radio-proxifier-<V>.tar.gz` (`git archive --prefix=…/ <commit>`)

Deliver them in the maintainer's local delivery folder (historically
`~/radio/internet-radio-proxifier-<V>/`, tarball next to it) and always
include the tarball. Build the aarch64 binary with:

```sh
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc \
       CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc \
       AR_aarch64_unknown_linux_musl=aarch64-linux-gnu-ar
cargo build --release --locked --target aarch64-unknown-linux-musl

SDK=/path/to/openwrt-sdk-25.12.5-… \
  openwrt/make-apk.sh target/aarch64-unknown-linux-musl/release/internet-radio-proxifier \
  aarch64_cortex-a53 internet-radio-proxifier-<V>-r1.apk

IPKG_BUILD=$(ls ~/.cache/openwrt-toolchains/ipkg-build-* | head -1) \
  openwrt/make-ipk.sh target/aarch64-unknown-linux-musl/release/internet-radio-proxifier \
  aarch64_cortex-a53 internet-radio-proxifier_<V>-r1_aarch64_cortex-a53.ipk
```

- For the tarball, `git stash create` includes tracked changes but **not
  untracked files**: `git add` new files first. The tarball is a source
  backup, not a vendored build; do not add `vendor/` (the maintainer removed
  it: "i'm not going to rebuild it anyway").
- The aarch64 apk works on all aarch64 OpenWrt architectures
  (`aarch64_cortex-a53`, `-a72`, `-a76`, `aarch64_generic`).
- If you ever build **x86_64 musl manually**, set
  `CC_x86_64_unknown_linux_musl=gcc` (build.sh does). Without it the cc-rs
  build fails and a **stale binary gets packaged** — this once shipped an
  old binary that didn't know `--port`.

### OpenWrt package behavior

- Installs `/usr/bin/internet-radio-proxifier`, a procd init (START=95,
  user `nobody`, `respawn 3600 5 0`, reload trigger on UCI change) and
  `/usr/share/internet-radio-proxifier/settings.example.ini`.
- UCI `/etc/config/internet-radio-proxifier`: `enabled` (1), `address`
  (`::`), `port` (9876), `config`
  (`/etc/internet-radio-proxifier/settings.ini`). It is a **conffile**:
  apk/opkg replace it only if it was never edited; an edited file is kept
  and the new one saved as `.apk-new` / `-opkg`. The maintainer expects
  exactly this — no migration code.
- `settings.ini` is created at install time by the shared
  `openwrt/files/setup.sh` (and by the init on first start if missing):
  dir `0700`, file `0600`, owned `nobody:nogroup` (fallback 65534). It is
  not shipped in the package, so `lib/upgrade/keep.d/` keeps `/etc/…/`
  across sysupgrade.
- **Upgrade restart:** procd ignores "start" for an already-running service,
  so the apk `post-upgrade` forces a restart; the ipk `postinst-pkg` uses
  `pidof internet-radio-proxifier` because 24.10's opkg does not set
  `PKG_UPGRADE`. Keep both mechanisms.
- The `.apk` is signed with the SDK `private-key.pem` when present
  (`SIGN_KEY` overrides); users install with `apk add --allow-untrusted`.
  Never add the SDK key to a router's trusted keys without permission.
- The ipk builder uses OpenWrt 24.10's `scripts/ipkg-build` with
  `TAR="tar --owner=0 --group=0"`, `LC_ALL=C` (a Russian locale garbled the
  date once) and `PKG_SOURCE_DATE_EPOCH` = last commit time, for
  reproducible timestamps.
- The SDK `Makefile` builds inside the OpenWrt buildroot (symlink it into
  `package/`, needs the rust feed). Keep its install rule in step with
  `make-apk.sh` / `make-ipk.sh`: all three install
  `openwrt/files/internet-radio-proxifier.init`,
  `openwrt/files/internet-radio-proxifier.config` and the bundled
  `settings.example.ini`. (A stale `radio.config` path here was a real bug.)

## Testing checklist

Before reporting work done:

1. `cargo test --locked` passes (19 tests, offline).
2. `cargo build --release --locked` passes.
3. Start the binary with a **temporary** settings file on a non-default
   port (record `$!`, kill that PID) and check:
   - `/` renders and contains "Internet Radio",
   - `/settings/api/stations` returns JSON with `"stations"`,
   - `/station/1` streams audio (200, audio content-type) or 502 when the
     upstream is unreachable — both are correct behavior,
   - `/station/0` and an out-of-range number are 404,
   - a PUT round-trip keeps the password and restores a masked proxy.
4. If the UI changed, open it in a browser at desktop and phone widths.
   Check the title line does not shift the layout, the status line is
   accurate, and buttons/tags/star stay aligned to the anchors above.
5. **Proxied stations cannot be tested directly from this PC**: the
   maintainer's proxy requires credentials that only the router has, and
   the PC's direct internet path is filtered for some hosts (connections
   freeze after ~20–33 KB — the reason per-station proxies exist). Test them
   through the router's own `POST /settings/api/check` (pass the **masked**
   proxy string from `GET /settings/api/stations`; the server un-masks it)
   or its `/station/N` relay. Never read the router's stored credentials.
6. If packaging changed, build the apk/ipk, inspect their contents
   (`tar`/`unzip`), and install on a test device **only when asked**.
7. If `README.md` behavior text or screenshots changed, update both.
   `docs/*.png` (player, stations, settings-stations, settings-options) were
   taken from a demo server with the example stations plus public streams
   and the dummy proxy `socks5h://user:secret@192.168.1.10:1080` shown
   masked; regenerate them the same way, never with real stations'
   credentials.

## Environment and safety

- Devices: the main test router is `192.168.200.254` (TR3000, Filogic 820,
  aarch64_cortex-a53, 512 MB RAM, 128 MB NAND) — install there when asked.
  The x86_64 OpenWrt box `192.168.200.252` is the first stop for tests
  (the maintainer asked for new builds there before `.254`). An offline test
  router (`192.168.203.254`) was reached via a `utest` Wi-Fi link that has
  since been dropped; the maintainer enters Wi-Fi passphrases themselves.
- The default port is **9876** ("easy to remember"); 8123 was dropped
  because Home Assistant uses it, 8765 because motionEye/AnkiConnect do.
- When starting a test server in the background, record `$!` and kill that
  PID. **Never use `pkill -f` with a pattern that also appears in your own
  command line** — it kills the shell running the command (exit 144) and any
  sibling jobs. Run `cd` as a separate statement before `prog &`; otherwise
  `$!` is the wrapper shell and the real server keeps the port.
- Do not put real credentials, proxy URLs containing passwords, router
  configs, or `settings.ini` into the repository, tests, docs or AGENTS.md.
  Use dummy values such as `socks5h://user:secret@192.168.1.10:1080`.
- Do not log `Authorization` headers, proxy passwords, or the un-masked
  proxy URL.
- Install/test on shared infrastructure only when the user asks; prefer a
  temporary settings file and a non-default port for local tests.
