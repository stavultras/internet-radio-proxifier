# Working on Internet Radio Proxifier

## Maintainer rules

- Test locally first. Commit, push, tag, trigger CI, or release only when asked. Use an imperative commit summary and a short body explaining why.
- Keep version `0.1`. `VERSION` drives the CLI and packages. Leave Cargo's placeholder at `0.0.0`. An authorized re-release moves the existing `v0.1` tag.
- Rename across source, paths, UCI, init, packaging, CI and README together. Drop the old name and avoid compatibility shims.
- Use dummy credentials in tests, examples and screenshots. The maintainer enters real credentials. Never extract or reuse router credentials, commit a live config, or expose proxy URLs, passwords or authorization headers in logs or errors.
- Test on shared devices or install packages only when asked. For device tests, start with the x86_64 OpenWrt box at `192.168.200.252` before the Filogic 820 at `192.168.200.254`. Test proxied stations via the router relay or its check API using the masked proxy value.

## Local development

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --locked
cargo build --release --locked
target/release/internet-radio-proxifier -a 127.0.0.1 -p 19876 -c /tmp/radio-settings.ini
```

Use temporary settings on a non-default port (the deployed default is 9876). If backgrounding the binary, record its PID with `$!` and stop that PID. Avoid `pkill -f`.

- Smoke-test the player, settings JSON and a settings PUT that preserves the password and masked proxy. Check that `/station/1` serves audio or returns 502 for an unreachable upstream, and that zero and out-of-range station numbers return 404.
- Check UI changes in a browser at desktop and phone widths (≤700 px). Reload after CSS edits. Keep README behavior and screenshots in `docs/` in sync using demo data.
- Match existing formatting (four spaces in Rust, tabs in web assets). Limit formatting to touched code.
- Target small routers and 32-bit MIPS. Keep the Rust binary, size-optimized release profile, hand-written HTTP client, Tokio current-thread runtime and four blocking threads. Prefer std, justify new dependencies and use `AtomicUsize` for counters.

## Behavior to preserve

- **Stations.** User-visible numbers follow 1-based file order. Internal indices are 0-based. The picker and navigation put favorites first. The player star and confirmed removal by name save immediately. Protect the last station on both client and server. Preserve picker scroll and focus the replacement row after removal.
- **Listener state.** Remap listeners across edits, including when two stations share a URL. On removal, select the next surviving station or the last one. Restore the browser's saved station by name on reconnect using each picker's explicit station index. Keep the saved name on a temporary list failure and reapply volume on source changes.
- **Playback.** Send station info before the audio source. htmx replaces elements by ID, so re-query them after swaps. Compare the stream identity as well as the numbered source URL. Report actual playback state. A blocked autoplay waits for a gesture. Retry a stream that previously played, skip one that never started, and stop after one failed round. Pausing disables auto-skip.
- **Settings.** The settings password is editable only by hand in `settings.ini`. Read it on every auth check and again before saving. Mask the proxy password and restore it on save. Whole-file rewrites are intentional. Serialize saves, use atomic replacement with Unix mode `0600`, and finish durable writes after client disconnects.
- **Settings UI.** Save and Undo cover stations and options together. Reveal the field and tab on validation errors. Direct-only bulk tests are report-only. “Proxy the failing” clears direct successes, selects proxy-only successes and leaves changes for review and Save. Keep pointer and keyboard reordering.
- **Streaming.** Preserve ICY support, proxy-side DNS for both SOCKS schemes, per-station proxy selection, and the configured User-Agent on relay, metadata and check requests. Share MP3/AAC upstreams by URL and proxy mode. Keep container streams per listener, bound queues, stop unused upstreams and retain TCP_NODELAY on accepted and outbound sockets.
- **Metadata.** Prefer titles from active relays. Poll only streams with playing listeners, deduplicate by URL and poll hidden tabs less often. Preserve cached titles across failures. Measure MP3/AAC bitrate from frames before falling back to headers.
- **Browser assets.** Keep HTML/CSS/JS dependency-free apart from bundled htmx. Embed all assets locally through `assets!`, escape user content with `esc()`, and keep the service worker pass-through. Preserve PWA and Media Session icons.
- **Layout.** Reserve a line for station name and title. Align corner tags with the button shadows and the star with the volume percentage. Preserve mobile volume-row space and settings header spacers.

## Packaging and release

Local package requests produce **only three files** in `dist/` for Filogic 820 (`aarch64_cortex-a53`). Substitute the contents of `VERSION` for `<V>`.

- `internet-radio-proxifier-<V>-r1.apk`
- `internet-radio-proxifier_<V>-r1_aarch64_cortex-a53.ipk`
- `internet-radio-proxifier-<V>.tar.gz`

Build `aarch64-unknown-linux-musl` with `cargo build --release --locked --target aarch64-unknown-linux-musl` and the linker, CC and AR settings in `build.sh`. Package the fresh binary with `openwrt/make-apk.sh` (`SDK`) and `openwrt/make-ipk.sh` (`IPKG_BUILD`). Archive the matching source with a versioned top-level directory. Include intended new files (`git stash create` omits untracked files) and exclude `vendor/`. For manual x86_64 musl builds set `CC_x86_64_unknown_linux_musl=gcc`.

- Keep the SDK Makefile and both package scripts in sync. Preserve UCI conffile handling, sysupgrade retention, service-owned private settings and the distinct apk/ipk upgrade restart hooks. Inspect both packages after packaging changes. Installing an SDK signing key on a router requires permission.
- Keep CI on `ubuntu-24.04` and the MIPS nightly in sync with `build.sh`. PR and manual runs are build-only. For a requested re-release, replace the release and tag, watch CI to completion and verify 17 assets. Keep release filenames and the README's ImageBuilder rename instruction.
