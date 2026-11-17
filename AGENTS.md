# AGENTS.md

Guidance for AI coding agents working in this repository. Read it before
changing behavior, build files, or the browser UI.

## What this is

Internet Radio Proxifier is a self-hosted web radio player. A single static
Rust binary relays internet radio streams through the machine that runs it
(a PC, a server, or an OpenWrt router), optionally through a SOCKS5 or
HTTP(S) proxy **per station**. Browsers only talk to this server; every
asset they need is embedded in the binary, so the player page also works
when the machine itself has no internet access (only the stations need it).

- Rust 2024, axum 0.8, tokio (current-thread runtime), rustls/ring.
- Browser UI is plain HTML/CSS/JS plus bundled htmx and its WS extension.
  There is no Node/npm build step and no CDN dependency.
- Distribution: Linux (static musl, x86_64/aarch64), Windows x64, and
  OpenWrt 25.x `.apk` / 24.10 `.ipk` for aarch64, x86_64, mipsel_24kc and
  mips_24kc.
- Upstream project: <https://github.com/stavultras/internet-radio-proxifier>.
  It started as a rewrite of <https://github.com/pinpox/radio>.

## Commands

```sh
cargo build --release                  # native release binary
cargo test                             # unit tests (parsing, masking, charsets, bitrate); no network needed
cargo run -- -a 127.0.0.1 -p 9876 -c /tmp/radio-settings.ini
curl -sS http://127.0.0.1:9876/ | head
./build.sh                             # all release packages into dist/
./build.sh linux-x86_64 linux-aarch64 windows-x86_64
SDK=/path/to/openwrt-sdk ./build.sh openwrt
```

Notes:

- Rust 1.85+ is required (`edition = "2024"`).
- `cargo fmt` / `cargo clippy` components are not installed in every
  environment here. If available, run them on touched code only; **do not
  reformat the whole tree** — it is intentionally hand-formatted with some
  long lines, and a global `cargo fmt` would create a large unrelated diff.
- `./build.sh` needs rustup targets and cross compilers listed at the top of
  the script. The `openwrt` target additionally needs `SDK=` (for `apk` and
  `fakeroot`); MIPS additionally needs the pinned nightly and downloads
  OpenWrt toolchains on first use.
- Never test with a real settings file from a router; use a temporary one.

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
  files/         procd init script, UCI config, shared install step
  make-apk.sh    OpenWrt 25.x apk builder (SDK host tools only)
  make-ipk.sh    OpenWrt 24.10 ipk builder (OpenWrt's ipkg-build)
  internet-radio-proxifier/Makefile   package for building inside the SDK
build.sh         all release packages -> dist/
docs/            README screenshots (keep in sync with the UI)
.github/workflows/build.yml            build/test/release CI
VERSION          the only place the release version is set
settings.example.ini                   credential-free starter stations
```

## Design and behavior contracts

### Server and routes

`App` in `src/main.rs` is the whole state: stations (`RwLock<Arc<Vec<Station>>>`),
proxy raw string plus parsed form, three feature flags, retries, the settings
file path, websocket sessions, song-title and bitrate caches, an in-flight set
for title fetches, and a save lock.

Routes:

| Route | Purpose |
| --- | --- |
| `GET /` | player page |
| `GET /ws` | player websocket (htmx WS extension) |
| `GET /api/stations` | station picker data, in play order |
| `GET /static/{path}` | embedded assets |
| `GET /settings` | settings page (optional Basic auth) |
| `GET/PUT /settings/api/stations` | read/save settings and stations |
| `POST /settings/api/check` | test one stream, optional proxy |
| `GET /station/{N}` | relayed stream of station `N`, **1-based** |

All responses get `Cache-Control: no-cache` from the `no_cache` middleware.
Embedded files have no mtime, so without it browsers keep old JS after an
upgrade.

### Station numbering and ordering

- Station numbers are **1-based everywhere users see them**; internal indices
  are 0-based. `/station/0` must stay a 404. `App::station_url(i)` adds 1 and
  there is a unit test for it.
- Station numbers are their position in the settings file. Reordering in the
  settings page renumbers them.
- Favorites change the **play order only**, not the numbers:
  `stations::play_order()` returns favorites first, then the rest, each in
  file order. Next/Previous and the picker follow that order.
- The last station is restored in the browser by name/index, not by position
  in `/api/stations` (that list is play-ordered). A bug here once made a star
  look lost after reload — don't regress it.
- `remap_sessions()` keeps listeners on the same stream URL after a reorder;
  listeners whose station was removed move to station 0.

### Settings file (`settings.ini`)

- The INI file is the single source of truth; there is no database.
- Global keys (`password`, `proxy_url`, `autoplay`, `random_start`, `random`,
  `retries`) must appear **before the first `[station]` section**; per-station
  keys are `url`, `use_proxy`, `favorite`.
- The parser accepts `#`/`;` comments, `key = value`, `"""…"""`, backticks,
  and single/double-quoted values. Preserve round-trip behavior; add a case
  to `stations.rs` tests when changing it.
- **The settings-page password can only be set by editing the file.** Never
  add a UI field for it, never render it, and never overwrite it on save.
  Username is `admin`. An empty password means the page is open.
- Saving is atomic (temp file + rename) and mode `0600` on Unix because the
  file holds proxy passwords. Keep both.
- The proxy password is sent to the browser only as `********`; the real
  value is restored from the saved file on save. Proxy parse/save errors must
  never echo the proxy URL (it can contain a password). Existing tests cover
  the mask round-trip.
- `settings.example.ini` must stay credential-free.

### HTTP client and proxies

`src/http.rs` is deliberately hand-written: HTTP/1.1 only, ICY responses,
redirects (up to 10), chunked bodies, SOCKS5 (hostnames resolved by the
proxy for both `socks5://` and `socks5h://`), HTTP proxies (absolute-form for
http, `CONNECT` for https), and HTTPS proxies. It also provides `base64` and
URL parsing. Do not replace it with `reqwest`/`hyper` unless the maintainer
asks: the point is a small, auditable binary with few dependencies.

- `USER_AGENT` is required: some stream hosts reject unknown clients.
- SOCKS default port is 1080; proxy schemes are `socks5`, `socks5h`, `http`,
  `https`.
- Per-station proxying is driven by `use_proxy`; `proxy_for()` returns `None`
  for stations without it.
- The settings page refuses to tick a station's proxy box while the proxy
  field is empty, and the check endpoint reports "proxy is ticked but no
  proxy is set".

### Player behavior (do not regress these)

- **Name before stream.** On a station switch the server sends the station
  info HTML before `player_html`; the player can then show the name
  immediately rather than after the first audio buffer.
- **No layout jump.** `#station-name` and `#station-title` reserve a line
  (`min-height: 1.3em`) so the box does not grow when a title arrives.
- **Status line is real state:** connecting, playing, buffering, retrying,
  unavailable and skipped. Do not print "Connected" before audio plays.
- **Autoplay:** browsers block playback without a gesture. `data-autoplay`
  only requests it; the first `pointerdown`/`keydown`/`touchstart` starts
  playback. Don't treat `NotAllowedError` as a station failure.
- **Retries:** `retries` is 0–10 (default 3). A station that stopped
  reconnects after 3 s × attempt (3, 6, 9, …) before moving on. A station
  that never started is skipped immediately. The counter refills after 5 s
  of good playback and on a station switch. A station that fails while
  Random is on is replaced by a random station.
- **Titles:** fetched only for stations someone is listening to, every 5 s.
  Metadata has no declared charset: decode UTF-8, otherwise Windows-1252
  (Latin-1), which fixed names like `Wölli`.
- **Bitrate:** measured from MP3/AAC frames (`audio.rs`); `~173` means VBR.
  Fall back to `icy-br`/`ice-audio-info` only when no frame chain is found.
  Do not trust the station's advertised value alone.
- **Everything is local:** no external fonts, icons, scripts, analytics or
  images. The player must render with no internet at all. A new asset must
  be committed under `assets/static/` and listed in the `assets!` macro —
  `include_bytes!` only covers listed names.

### Web UI conventions

- Plain JS, no framework, no build step. Keep it dependency-free.
- Settings has two tabs, Stations and Settings, with Undo and Save on both;
  Save writes stations and options together.
- Reordering uses a drag grip (and arrow keys when focused), not up/down
  buttons.
- Validation errors should `reveal()` the tab/field that caused them.
- The maintainer reviews spacing and alignment closely. When touching CSS,
  check the player at desktop and phone widths and keep the Previous/Next
  buttons, `PROXY`/bitrate corner tags, volume row and favorite star aligned
  to the same edges.
- `style.css` has `body > *:not(:last-child) { display: block; … }`, which
  overrides `hidden` and flex on direct body children. Use higher specificity
  (`.tab-panel[hidden]`, `body.settings …`) instead of relying on the
  attribute.
- Hash-only navigation (`/settings` → `/settings#settings`) does not reload
  the page. When testing CSS, call `location.reload()`.

## Coding conventions

- Match the surrounding compact style: 4 spaces in Rust, tabs in HTML/CSS/JS.
- Comments explain **why**, not what; the existing code is sparsely commented
  and that is intentional. Public items get `///` docs where they clarify a
  contract.
- Prefer std: the project rolls its own base64, URL parsing, random number
  (`random_below`, based on `RandomState`), HTTP client and bitrate parser to
  keep the binary around 2 MB. Adding a crate needs a clear size/complexity
  justification.
- Error messages are user-facing and specific, except proxy credentials:
  never include a URL/userinfo in an error or log line.
- Use `AtomicUsize` for IDs/counters, not `AtomicU64`: 32-bit MIPS has no
  64-bit atomics and must build. `next_id` is `usize` for that reason.
- Keep the `tokio` current-thread runtime and `max_blocking_threads(4)`;
  the target is a router with 512 MB RAM. Avoid spawning per-connection
  blocking work.
- Release profile (`opt-level = "z"`, LTO, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`) is deliberate. Changes that grow the
  binary need a reason.
- HTML escaping goes through `esc()`; station names are user input and may
  not contain `[`/`]` or line breaks (`stations::validate`).
- CLI: `-a/--address` and `-p/--port` are required, `-c/--config` is
  optional, `-h` is help (not host), `-V` prints the `VERSION` file.
  `Cargo.toml` keeps `version = "0.0.0"` on purpose. The default config is
  `/etc/internet-radio-proxifier/settings.ini` on Unix and `settings.ini`
  on Windows. A missing file is created from `settings.example.ini`
  (mode `0600` on Unix); an existing file with no `[station]` sections is a
  startup error.
- No backward-compatibility shims. The maintainer explicitly asked to drop
  old names, env vars and config keys when renaming (for example
  `RADIO_*`, `-h/--host`, port 8123). Rename everything and update README,
  UCI, init script, CI and packaging together instead of keeping aliases.
- Commit messages: imperative summary, optionally prefixed with the area
  (`Player:`, `Settings:`, `CI:`, `OpenWrt:`), then a short body explaining
  the change and its reason. Do not commit unless asked.

## Build and release

### Version

- `VERSION` is the only release version; `-V` and packages read it.
- `Cargo.toml` intentionally has `0.0.0` so the file does not need editing.
- The project is at `0.1`, and the maintainer has repeatedly re-released
  `v0.1` instead of bumping. Change `VERSION` only when asked.

### CI

`.github/workflows/build.yml` runs only when:

1. a push to `main` changes `VERSION`,
2. a `v*` tag is pushed, or
3. it is started manually.

Ordinary pushes do not run anything. **Do not push or trigger CI unless the
user asks.** Test locally first — that is an explicit maintainer rule. The
workflow builds every package, runs unit tests, starts every binary (MIPS
and aarch64 under qemu), tests the Windows zip on Windows, and publishes a
GitHub Release when appropriate.

- Ubuntu jobs are pinned to `ubuntu-24.04` on purpose (24.04 has reliable
  qemu-user packages; `ubuntu-latest` repeatedly failed on CI DNS).
- Release assets use names like
  `internet-radio-proxifier-<V>-openwrt-<arch>.apk` and `.ipk`.

Re-releasing the same version:

```sh
gh release delete v0.1 --yes --cleanup-tag
git tag -d v0.1 2>/dev/null
git tag v0.1
git push origin v0.1
gh run watch <run-id> --exit-status
```

The release should contain 17 assets.

### Local OpenWrt packages (maintainer workflow)

When the user asks for local builds, they want **only** the Filogic 820
(`aarch64_cortex-a53`) packages, plus a source tarball — not the full
`build.sh` output. Use standard OpenWrt names, not the GitHub release names:

- `internet-radio-proxifier-<V>-r1.apk`
- `internet-radio-proxifier_<V>-r1_aarch64_cortex-a53.ipk`
- `internet-radio-proxifier-<V>.tar.gz` (`git archive --prefix=…/ <commit>`)

Deliver them in the maintainer's local delivery folder (historically
`~/radio/internet-radio-proxifier-<V>/`) and always include the tarball. Build
the aarch64 binary with:

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

For the tarball, `git stash create` includes tracked changes but **not
untracked files**: `git add` new files first. The tarball is a source backup,
not a vendored build; do not add `vendor/` or a Go SDK.

The aarch64 apk works on all aarch64 OpenWrt architectures
(`aarch64_cortex-a53`, `-a72`, `-a76`, `aarch64_generic`). `build.sh`
also produces x86_64, mipsel_24kc and mips_24kc packages for releases.

### OpenWrt package behavior

- Installs `/usr/bin/internet-radio-proxifier` and a procd service running
  as `nobody` on port 9876 by default.
- UCI file `/etc/config/internet-radio-proxifier` (`enabled`, `address`,
  `port`, `config`) is a conffile. Settings live in
  `/etc/internet-radio-proxifier/settings.ini`, created from the bundled
  example, mode `0600`, owned by the service user, kept across sysupgrade.
- The `.apk` is signed with the SDK key when present; users install it with
  `apk add --allow-untrusted`.
- The IPK builder uses OpenWrt 24.10's `scripts/ipkg-build` with
  `LC_ALL=C` and `PKG_SOURCE_DATE_EPOCH` (commit time) for reproducible
  timestamps.
- The SDK `Makefile` is for building inside the OpenWrt buildroot. Keep its
  install rule in step with `openwrt/make-apk.sh` and
  `openwrt/make-ipk.sh`: all three install
  `openwrt/files/internet-radio-proxifier.init`,
  `openwrt/files/internet-radio-proxifier.config` and the bundled
  `settings.example.ini`.

## Testing checklist

Before reporting work done:

1. `cargo test` passes.
2. `cargo build --release` passes.
3. Start the binary with a temporary settings file and check:
   - `/` renders and contains "Internet Radio",
   - `/settings/api/stations` returns JSON,
   - `/station/1` returns audio (or a station-specific response),
   - `/station/0` is 404.
4. If the UI changed, open it in a browser at desktop and phone widths.
   Check the title line does not shift the layout, the status line is
   accurate, and buttons/tags stay aligned.
5. If packaging changed, build the apk/ipk, inspect their contents
   (`tar`/`unzip`), and install on a test device before calling it done.
6. If `README.md` behavior text or screenshots changed, update both
   (`docs/*.png` are the screenshots used by the README).

## Environment and safety

- The maintainer's test device is an OpenWrt router with a Filogic 820 SoC
  (aarch64_cortex-a53, 512 MB RAM). The default port is **9876**; 8123 was
  dropped because Home Assistant uses it.
- When starting a test server in the background, record `$!` and kill that
  PID. **Never use `pkill -f` with a pattern that also appears in your own
  command line** — it kills the shell running the command (exit 144) and any
  sibling jobs. Run `cd` as a separate statement before `prog &`; otherwise
  `$!` is the wrapper shell and the real server keeps the port.
- Do not put real credentials, proxy URLs containing passwords, router
  configs, or `settings.ini` into the repository, tests, docs or AGENTS.md.
  Use dummy values such as `socks5h://user:secret@192.168.1.10:1080`.
- Do not add the SDK signing key to a router's trusted keys without explicit
  permission.
- Do not log `Authorization` headers, proxy passwords, or the un-masked proxy
  URL. The settings password is read from the file on every request; never
  cache it in a log line.
- Install/test on shared infrastructure only when the user asks, and prefer
  a temporary settings file and a non-default port for local tests.
