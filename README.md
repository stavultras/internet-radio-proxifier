# Internet Radio Proxifier

A small self-hosted web radio player. It streams internet radio stations
through the machine it runs on (a PC, a server or an OpenWrt router), so
browsers only talk to that machine, and it can route individual stations
through a SOCKS5 or HTTP proxy (for example stations that are blocked in
your region).

The project was initially based on [pinpox/radio](https://github.com/pinpox/radio)
and has since been rewritten in Rust.

## Features

- Player page: next / previous / random station, a station picker, volume
  with percentage, the current song title
- Remembers the last station per browser; optional autoplay; optional
  random mode for Next / Previous
- Skips stations that stop working
- Song titles are fetched only for stations someone is listening to
- Settings page: add, edit, test and drag to reorder stations, set the
  proxy, choose per station whether it goes through the proxy, autoplay,
  random
- Installable as a PWA; Media Session support (lock screen, headset buttons)
- One static binary of about 2 MB using a few MB of RAM; no runtime
  dependencies besides CA certificates for https stations

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
./internet-radio-proxifier -h :8123 -c settings.ini
```

### Windows

Unpack the zip and run `start.cmd`. It starts the radio on port 8123 with
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

The package installs `/usr/bin/internet-radio-proxifier` and a procd service that runs it as
user `nobody` on port 8123:

- `/etc/config/internet-radio-proxifier`: `enabled`, `address` (default `:8123`), `config`
  (default `/etc/internet-radio-proxifier/settings.ini`)
- `/etc/internet-radio-proxifier/settings.ini`: created with example stations on first start,
  kept across sysupgrade

```sh
/etc/init.d/internet-radio-proxifier restart
logread -e internet-radio-proxifier
```

Then open `http://<router>:8123/`.

## Usage

```
internet-radio-proxifier -h ADDRESS [-c SETTINGS_FILE]

  -h, --host ADDRESS     listen address, e.g. :8123 (all interfaces) or 192.168.1.1:8123
  -c, --config FILE      settings file (default: /etc/internet-radio-proxifier/settings.ini, on
                         Windows settings.ini); created with example stations
                         if it does not exist
      --help             show this help
  -V, --version          show the version
```

The player is at `http://ADDRESS/`, the settings page at `http://ADDRESS/settings`.

## Settings file

```ini
# Settings page password (user name: admin). It can only be set by editing
# this file; without it the settings page is open. Put the value in
# backticks if it contains # or ; (password = `pa#ss`).
password  = secret
proxy_url = socks5h://user:password@192.168.1.10:1080
autoplay     = true
random_start = false
random       = false

[Station name]
url = https://stream.example.com/live.mp3

[Another station]
url       = https://stream.example.org/radio.aac
use_proxy = true
```

- `proxy_url`: `socks5://`, `socks5h://` (DNS on the proxy), `http://` or
  `https://`; without a port, 1080 is used for SOCKS
- `use_proxy`: this station goes through the proxy
- `random_start`: with `autoplay`, start with a random station instead of
  the one played last in this browser
- `random`: Next and Previous jump to a random station instead of the
  neighbouring one
- `autoplay`: start playing as soon as the player page opens (browsers may
  still require one click first)
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
publishes the packages as GitHub Release `v<VERSION>`. A test run without a
release can be started by hand from the Actions tab (`gh workflow run
build.yml`).

## License

LGPL-2.1-only, see [LICENSE](LICENSE).
