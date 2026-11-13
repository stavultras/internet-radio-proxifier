# Internet Radio Proxifier

A small self-hosted web radio player. It streams internet radio stations
through the machine it runs on (a PC, a server or an OpenWrt router), so
browsers only talk to that machine, and it can route individual stations
through a SOCKS5 or HTTP proxy (for example stations that are blocked in
your region).

The project was initially based on [pinpox/radio](https://github.com/pinpox/radio)
and has since been rewritten in Rust.

![Player](docs/player.png)

## Features

- Streams every station through the radio's own machine; per station
  through a SOCKS5 or HTTP proxy
- Player: previous / next / random station, a station picker with a filter,
  volume, the current song title and a status line that says what the
  player is doing
- Keeps playing through short drops: a station that stops is reconnected a
  few times before the player moves on; stations that are down are skipped
- Settings page to manage stations and options from the browser, protected
  by a password if you set one
- Works without internet access to anything but the stations themselves:
  everything the browser needs is built into the binary
- Installable as an app (PWA); lock screen and headset controls (Media
  Session)
- One static binary of about 2 MB using a few MB of RAM; packages for
  Linux, Windows and OpenWrt (25.x and 24.10)

## Player

`http://ADDRESS:PORT/`

- **Previous / Next station**, **Random station**, **Pause**
- **All stations**: a list of all stations with a filter; the title shows how
  many there are ("Stations (12 of 448)" while filtering), and stations that
  go through the proxy are marked. Typing a number in the filter finds that
  station (Enter plays it)
- Every station has a number, shown next to its name in the player, in the
  station list and on the settings page, so a station with a good song is easy
  to find again
- **Favorites**: the star in the bottom right corner of the player marks the
  station as a favorite (saved in the settings file, so it is the same on
  every device). Favorites come first in **All stations** and in Next /
  Previous, then the other stations, each in the order of the settings file.
  The numbers do not change
- The top corners of the player show **PROXY** when the station plays through
  the proxy, and the bitrate. The bitrate is measured from the audio itself
  (MP3 and AAC frames), not taken from what the station claims; `~173 kbps`
  means a variable bitrate (average). Other formats show the station's
  `icy-br` if it sends one
- The status line shows what is going on: connecting, playing, buffering,
  a station that dropped and is reconnecting, a station that is unavailable
  and skipped
- Each browser remembers its last station and volume
- With *Autoplay* the radio starts as soon as the page opens (browsers may
  still want one click first)
- Song titles are only fetched for stations someone is listening to; titles
  in UTF-8 and in Latin-1 / Windows-1252 are both shown correctly

![Station picker](docs/stations.png)

## Settings page

`http://ADDRESS:PORT/settings` (user name `admin` and the `password` from
the settings file, if one is set). Changes on both tabs are saved together
with **Save**, or dropped with **Undo changes**; listeners get them right
away.

**Stations** tab:

- Add, rename and remove stations; drag the grip on the left to reorder
  them (or focus it and use the arrow keys); the numbers follow the new order
- **Test** checks that a station plays and shows its format and bitrate;
  **Test all** checks every station and lists the numbers of the ones that
  fail. **Test all without proxy** does the same with the proxy off for every
  station, to see which ones really need it; it changes nothing
- **&#x2605;**: favorite, the same as the star in the player
- **proxy**: this station goes through the proxy. It can only be ticked
  when a proxy is set on the Settings tab. **for all stations: proxy** above the list
  ticks or unticks it for every station at once
- **Proxy the failing**: tests every station directly and ticks *proxy*
  for the ones that only work through the proxy

![Settings: stations](docs/settings-stations.png)

**Settings** tab:

- **Autoplay**: start playing when the radio page opens
  - **Start with a random station** instead of the one played last in this
    browser (needs Autoplay)
- **Random**: Previous and Next jump to a random station
- **Retry attempts**: how often a station that stopped playing (for
  example while the internet or the proxy reconnects) is reconnected
  before the player skips it; 0 skips right away
- **Proxy**: `socks5://`, `socks5h://` (DNS on the proxy), `http://` or
  `https://`. The saved password is shown as `********`

![Settings: options](docs/settings-options.png)

The settings page password itself can only be set in the settings file.

## Downloads

Packages for every version are on the
[Releases](https://github.com/stavultras/internet-radio-proxifier/releases) page:

| Package | For |
| --- | --- |
| `...-linux-x86_64.tar.gz` | Linux on x86_64 PCs and servers (static binary) |
| `...-linux-aarch64.tar.gz` | Linux on 64-bit Arm (Raspberry Pi, servers) |
| `...-windows-x86_64.zip` | Windows 10/11 x64 |
| `...-openwrt-aarch64_cortex-a53.apk` (and `-a72`, `-a76`, `aarch64_generic`) | OpenWrt on 64-bit Arm routers (MediaTek Filogic, Qualcomm IPQ, ...) |
| `...-openwrt-x86_64.apk` | OpenWrt on x86_64 |
| `...-openwrt-mipsel_24kc.apk` | OpenWrt on MediaTek MT7621 / MT76x8 (ramips) |
| `...-openwrt-mips_24kc.apk` | OpenWrt on Atheros / Qualcomm QCA (ath79) |

Every OpenWrt package comes in two formats: `.apk` for OpenWrt 25.x and
`.ipk` (same names) for OpenWrt 24.10.

### Linux

```sh
tar xzf internet-radio-proxifier-*-linux-x86_64.tar.gz
cd internet-radio-proxifier-*/
./internet-radio-proxifier -a :: -p 9876 -c settings.ini
```

### Windows

Unpack the zip and run `start.cmd`. It starts the radio on port 9876 with
`settings.ini` next to `internet-radio-proxifier.exe` and opens the player in the browser.

### OpenWrt

Find your router's package architecture, copy the matching package to the
router and install it.

OpenWrt 25.x (apk; architecture: `cat /etc/apk/arch`):

```sh
apk add --allow-untrusted /tmp/internet-radio-proxifier-*-openwrt-aarch64_cortex-a53.apk
```

OpenWrt 24.10 (opkg; architecture: `opkg print-architecture`, the last line):

```sh
opkg install /tmp/internet-radio-proxifier-*-openwrt-aarch64_cortex-a53.ipk
```

#### ImageBuilder

To build the package into a firmware image, put it into the ImageBuilder's
`packages/` folder and add `internet-radio-proxifier` to `PACKAGES`.

OpenWrt 25.x: the apk must have its standard file name,
`internet-radio-proxifier-<version>-r1.apk`. The release file has the
architecture in its name, which the ImageBuilder does not find ("package
mentioned in index not found"), so rename it:

```sh
mv internet-radio-proxifier-0.1-openwrt-aarch64_cortex-a53.apk packages/internet-radio-proxifier-0.1-r1.apk
```

OpenWrt 24.10: the ipk works under its release name; the standard one would
be `internet-radio-proxifier_<version>-r1_<arch>.ipk`.

The package installs `/usr/bin/internet-radio-proxifier` and a procd service that runs it as
user `nobody` on port 9876:

- `/etc/config/internet-radio-proxifier`: `enabled`, `address` (default `::`, all
  interfaces), `port` (default `9876`), `config` (default
  `/etc/internet-radio-proxifier/settings.ini`)
- `/etc/internet-radio-proxifier/settings.ini`: created with example stations on first start,
  kept across sysupgrade

```sh
/etc/init.d/internet-radio-proxifier restart
logread -e internet-radio-proxifier
```

Then open `http://<router>:9876/`.

## Usage

```
internet-radio-proxifier -a ADDRESS -p PORT [-c SETTINGS_FILE]

  -a, --address ADDRESS  address to listen on: :: (all interfaces, IPv6 and
                         IPv4), 0.0.0.0 (all IPv4 interfaces) or one address
                         or host name, e.g. 192.168.1.1 or localhost
  -p, --port PORT        port to listen on, e.g. 9876
  -c, --config FILE      settings file (default: /etc/internet-radio-proxifier/settings.ini, on
                         Windows settings.ini); created with example stations
                         if it does not exist
  -h, --help             show this help
  -V, --version          show the version
```

The player is at `http://ADDRESS:PORT/`, the settings page at
`http://ADDRESS:PORT/settings`.

## Settings file

Everything on the settings page is stored in one INI file, which can also
be edited by hand (restart the service afterwards):

```ini
# Settings page password (user name: admin). It can only be set by editing
# this file; without it the settings page is open. Put the value in
# backticks if it contains # or ; (password = `pa#ss`).
password  = secret
proxy_url = socks5h://user:password@192.168.1.10:1080
autoplay     = true
random_start = false
random       = false
retries      = 3

[Station name]
url = https://stream.example.com/live.mp3

[Another station]
url       = https://stream.example.org/radio.aac
use_proxy = true
favorite  = true
```

- `password`: settings page password; without it the page is open
- `proxy_url`: `socks5://`, `socks5h://` (DNS on the proxy), `http://` or
  `https://`; without a port, 1080 is used for SOCKS
- `autoplay`: start playing as soon as the player page opens (default true)
- `random_start`: with `autoplay`, start with a random station instead of
  the one played last in this browser
- `random`: Next and Previous jump to a random station instead of the
  neighbouring one
- `retries`: how often the player reconnects to a station that stopped
  playing before it skips to the next one, waiting 3, 6, 9, ... seconds;
  0 to 10, default 3. A station that never started is skipped right away
- `use_proxy` (per station): this station goes through the proxy
- `favorite` (per station): listed and played first; set by the star in the
  player too
- The settings page rewrites the file when you save, keeping the password.
  On Linux the file is written with mode 0600 because it holds passwords.

HTTPS stations need CA certificates on Linux: `/etc/ssl/certs/ca-certificates.crt`,
`/etc/ssl/cert.pem` or `/etc/pki/tls/certs/ca-bundle.crt` (or set
`SSL_CERT_FILE`); on OpenWrt the package pulls in `ca-bundle`. The Windows
build has the CA certificates built in.

## Building

```sh
cargo build --release      # for this machine
cargo test
```

All release packages are built by [build.sh](build.sh) into `dist/`:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu
rustup toolchain install nightly-2026-11-09 --profile minimal --component rust-src
sudo apt install gcc-aarch64-linux-gnu gcc-mingw-w64-x86-64 zip zstd curl
SDK=/path/to/openwrt-sdk-25.12.5-x86-64_gcc-14.3.0_musl.Linux-x86_64 ./build.sh
```

- Linux and OpenWrt binaries are static musl builds.
- MIPS has no prebuilt Rust standard library, so it is built with a pinned
  nightly (`-Z build-std`) and OpenWrt's own toolchain, which `build.sh`
  downloads into `~/.cache/openwrt-toolchains` on first use.
- The OpenWrt `.apk` packages are put together by
  [openwrt/make-apk.sh](openwrt/make-apk.sh) with the SDK's `apk` and
  `fakeroot`; only the SDK's host tools are used. Packages are signed with
  the SDK's `private-key.pem` when it exists, so install them with
  `--allow-untrusted`.
- The `.ipk` packages for OpenWrt 24.10 are made by
  [openwrt/make-ipk.sh](openwrt/make-ipk.sh) with OpenWrt 24.10's own
  `scripts/ipkg-build`, which `build.sh` downloads on first use.
- To build inside the OpenWrt SDK instead (with OpenWrt's Rust), link
  [openwrt/internet-radio-proxifier](openwrt/internet-radio-proxifier) into the SDK's `package/` folder; see the
  comment at the top of its Makefile.

## Releases

Test locally with `build.sh` first; GitHub Actions does not run on ordinary
pushes. To release, change [VERSION](VERSION) and push to `main`:
[.github/workflows/build.yml](.github/workflows/build.yml) then builds and
tests everything (unit tests, every binary started under qemu, the Windows
package on a Windows runner) and, when there is no tag `v<VERSION>` yet,
publishes the packages as GitHub Release `v<VERSION>`. To publish a version
again, delete its release and tag on GitHub and push the tag `v<VERSION>`
(`git tag v0.1 && git push origin v0.1`). A test run without a
release can be started by hand from the Actions tab (`gh workflow run
build.yml`).

## License

LGPL-2.1-only, see [LICENSE](LICENSE).
