# Vendored third-party crates

Copies of upstream crates that MythBrowse builds with local fixes, wired in through
`[patch.crates-io]` in the root `Cargo.toml`. They are excluded from the workspace, so
`cargo fmt --all` and clippy never touch them. Everything here keeps its own license
(Servo crates: MPL-2.0); modified files must stay MPL-2.0 and their source must be
available to anyone we ship binaries to.

## servo-script 0.1.4

Source: the crates.io package `servo-script` 0.1.4 (sha256
`43d7b41f129a969c0bdbd6a4daf2a33513c6d1487a99474304aa31cf040cfe74`), unpacked as published,
minus `.cargo_vcs_info.json` and the package's own `Cargo.lock`, plus:

| Patch | Why |
| --- | --- |
| `patches/servo-script-0001-userscripts-can-run-script.patch` | Servo ran user scripts in documents that may not run script (`<iframe sandbox>` without `allow-scripts`, not fully active documents). `evaluate_js_on_global` asserts `can_run_script()`, so the panic took down the script thread and the page (amazon.com, zoom.us). User scripts are now skipped there, as the HTML spec skips any script. Still unfixed in Servo `main` on 2026-10-09. |
| `patches/servo-script-0002-evaluate-javascript-can-run-script.patch` | Same assertion through the embedder API: `WebView::evaluate_javascript` (the compat probe, local rules, Myth's page tools) on a document that may not run script (a top-level page served with `Content-Security-Policy: sandbox` without `allow-scripts`, or one that is no longer fully active) asserted in `evaluate_js_on_global` and killed the script thread. The evaluation now fails with `EvaluationFailure` instead. |
| `patches/servo-script-0003-error-stream-only-when-readable.patch` | `Response::error_stream` errored the fetch body stream even when the page had already cancelled it (or it had closed): a decompression error at EOF then hit `ReadableStream::error`'s `assert!(self.is_readable())` and killed the script thread (seznam.cz; reproduced locally with a truncated gzip body + `res.body.cancel()`). It now returns early when the stream is not readable. |
| `patches/servo-script-0004-iframe-redirect-new-event-loop.patch` | The constellation picks a pipeline's event loop (script thread) from the URL its navigation *starts* at; redirects are then followed inside that pipeline. An `<iframe>` redirected back to its initiator's site (OAuth/SSO: parent `outlook.live.com`, frame `login.microsoftonline.com` -> `login.live.com` -> `outlook.live.com/...oauthRedirect.html`) was created in the other site's event loop, so its same-origin parent got a cross-origin WindowProxy (`SecurityError`) and BroadcastChannel messages between them were lost (MSAL.js silent sign-in timed out). `handle_navigation_redirect` now hands such a redirect (iframe, GET, not origin-sandboxed, target same-site as the `LoadOrigin::Script` initiator, created for another site) back to the constellation as a replacing `LoadUrl`, so the parent starts a fresh pipeline in the right event loop. At most one hop per chain (the new load is same-site with its initiator). Needs servo-constellation 0001. An upstream issue draft is in preparation. |
| `patches/servo-script-0005-copyfromchannel-shared-length.patch` | `AudioBuffer.copyFromChannel` on a buffer whose data had not been handed to JS yet (new `AudioBuffer`, rendered `OfflineAudioContext` result) pre-filled the copy with zeros and then `extend_from_slice`d the samples: twice the destination length, so `Float32Array::update` asserted `data.len() <= buffer.len()` and the script thread died (shopee.co.id audio fingerprint). It now copies into the buffer. Reproduced on stock servoshell 0.5.0 with a 9-line page. Same code in Servo `main` on 2026-10-12. The compat rule `polyfill.dynamics-compressor` also guards it in JS (calls `getChannelData()` first) for unpatched engines. An upstream issue draft is in preparation. |
| `patches/servo-script-0006-media-no-backend-no-load-delay.patch` | MythBrowse builds Servo without `media-gstreamer`, so servo-media runs its dummy backend: `can_play_type` is "no" for everything and the dummy player never reports metadata or an error. A media element with a source (`<audio preload src=...mp3>`) therefore kept its *delaying-the-load-event flag* forever and the document never fired `load`: google.com search results (a pronunciation `<audio preload>` from ssl.gstatic.com) stayed `readyState == "interactive"` and the tab spinner ran forever (v0.4.1 user report "Google keeps loading after the verification"). Reproduced on stock servoshell 0.5.0 with a 1-line page (WebDriver navigation never returns). `create_media_player` now fails the resource as unsupported when the backend can play nothing (MEDIA_ERR_SRC_NOT_SUPPORTED, `error` event, load no longer delayed), which is what a browser without codecs reports. Builds with a real backend are unaffected. |
| `patches/servo-script-0007-webcrypto-no-ml-dsa.patch` | Security (v0.4.2): servo-script 0.1.4 depends on `ml-dsa` 0.0.4 for the draft WebCrypto ML-DSA-44/65/87 algorithms (<https://wicg.github.io/webcrypto-modern-algos/>), reachable by any page through `crypto.subtle` (secure contexts). That version has three advisories, all fixed only in `ml-dsa` >= 0.1.0-rc.5: GHSA-5x2r-hc65-25f9 / CVE-2026-24850 (verification accepts signatures with repeated hint indices), GHSA-hcp2-x6j4-29j7 / CVE-2026-22705 / RUSTSEC-2025-0144 (timing side channel in signing), GHSA-h37v-hp6w-2pp8 (UseHint off by two). No servo 0.1.x release moves to `ml-dsa` 0.1 (Servo did so in 0.5.0 together with the rest of its RustCrypto stack), and MythBrowse itself never uses ML-DSA (its own signed data, the compat rules, uses Ed25519). The patch removes the `ml-dsa` dependency and the ML-DSA operations (`dom/subtlecrypto/ml_dsa_operation.rs`, the ML-DSA key handles and their normalization arms), so `generateKey` / `importKey` / `exportKey` / `sign` / `verify` with an ML-DSA name reject with `NotSupportedError`, as in browsers that do not implement the draft. The algorithm names stay in `CryptoAlgorithm`. Every other WebCrypto algorithm, ML-KEM included, is unchanged. Drop this patch when moving to a Servo release on `ml-dsa` >= 0.1. MythBrowse-specific. |
| `patches/servo-script-0008-webcrypto-rsa-blinding.patch` | Security hardening (v0.4.2): WebCrypto RSA private-key operations use `rsa` 0.9.10, which has RUSTSEC-2023-0071 / CVE-2023-49092 ("Marvin": non-constant-time arithmetic can leak the private key through timing) and no fixed release (0.10 release candidates are affected too). Servo 0.1.4 ran RSA-OAEP `decrypt` (`RsaPrivateKey::decrypt`) and RSASSA-PKCS1-v1_5 `sign` (`Signer::try_sign`) *without* blinding (the crate passes no RNG, `DummyRng`), while RSA-PSS already signed with blinding. The patch makes both use blinding with `OsRng` (`decrypt_blinded`, `RandomizedSigner::try_sign_with_rng`). Outputs are unchanged (PKCS#1 v1.5 signatures stay deterministic; blinding only randomizes the intermediate value), so it is compatible with every caller. It narrows, but does not close, the timing channel; the advisory stays open until `rsa` ships constant-time arithmetic. MythBrowse-specific; drop when the upstream crate is fixed or Servo blinds these calls itself. |

## servo-net 0.1.4

Source: the crates.io package `servo-net` 0.1.4 (sha256
`c213fbac002f4b986d24b14c45075014f14e86f632b981db0ca097164765c147`), unpacked as published,
minus `.cargo_vcs_info.json` and the package's own `Cargo.lock`, plus:

| Patch | Why |
| --- | --- |
| `patches/servo-net-0001-proxy-tunnel-default-port.patch` | With an HTTP proxy configured (Servo copies `http_proxy`/`https_proxy` from the environment into its prefs), every request is tunnelled with hyper-util's `Tunnel`, which sends `CONNECT host:443` whenever the URL has no explicit port. Plain `http://` pages therefore went to port 443: "400 The plain HTTP request was sent to HTTPS port" (example.com, trueconf.net) or `client error (Connect/SendRequest)` (shiabank.com, macromedia.com) in the top-1000 run. The tunnel now gets the scheme's default port (80 for http/ws). Unit-tested (`default_port_tests`). Not yet reported upstream (servo/servo `components/net/connector.rs`, also in `main` on 2026-10-12). |
| `patches/servo-net-0002-cookie-management-hooks.patch` | Settings > Cookies (v0.4.2). Servo 0.1.4's `SiteDataManager` cannot list every cookie of a site (sub-domain host-only cookies, other paths), delete one cookie, or keep a site from storing cookies, and adding `CoreResourceMsg` variants would mean patching servo-net-traits and servo as well. New module `mythbrowse_cookies.rs`: the embedder reaches it through the two messages `SiteDataManager` already sends, with URLs in schemes no page can produce: `GetCookiesForUrl(mythbrowse-cookie-jar://<site>/)` returns every unexpired cookie of the registrable domain with its **value redacted** to `mbmeta.<size>.<host-only>` (`CookieStorage::site_cookies_redacted`); `SetCookieForUrl(mythbrowse-cookie-delete://...)` removes the cookie with exactly that name/domain/path (`delete_cookie_exact`); `SetCookieForUrl(mythbrowse-cookie-block://<site>/, block=1|0)` puts a site in a process-wide blocked set checked in `CookieStorage::push` and `cookies_data_for_url` (stores and sends nothing). Also: cookie values are no longer written to the log (`COOKIES SENT` info line, `SENT COOKIE` debug line). Third-party blocking is *not* here: the set/read paths do not know the top-level site. Unit tests: `cookie_storage::mythbrowse_tests`, `mythbrowse_cookies::tests`. MythBrowse-specific, not for upstream as is. |
| `patches/servo-net-0003-sealed-cookie-jar.patch` | Settings > Cookies "Stay signed in" (v0.4.2 prep, off by default). With `Opts::config_dir` set, stock Servo writes `cookie_jar.json` and `auth_cache.json` (HTTP auth user names and passwords) as plain JSON. When the embedder installs a `JarCodec` (`mythbrowse_cookies::set_jar_codec`) before the resource threads start, the jar is read from / written to `cookie_jar.sealed` through the codec instead (MythBrowse: AES-256-GCM, key in the OS key store), written atomically (temp file + rename, 0600 on Unix), and `auth_cache.json` is neither read nor written (HTTP auth stays in memory). `hsts_list.json` stays plain (host names only). Adds the `serde_json` dependency (already in Cargo.lock). Unit test: `mythbrowse_cookies::tests::sealed_jar_round_trip` (the crate's lib tests are off, `test = false`; exercised through the shell). MythBrowse-specific. |

## stylo 0.15.0

Source: the crates.io package `stylo` 0.15.0 (sha256
`6c032879feb9c437f8eb370b1ddc56c13531267766bd6f3ada99c406d36d60a6`), unpacked as published,
minus `.cargo_vcs_info.json` and the package's own `Cargo.lock`, plus:

| Patch | Why |
| --- | --- |
| `patches/stylo-0001-animation-base-style-keep-finished.patch` | Two CSS-animation bugs that froze or replayed animations whenever a stylesheet changed at run time (CSS-in-JS: Griffel/Fluent UI, emotion, styled-components; login.live.com's card stayed at opacity 0). (1) `maybe_start_animations` computed keyframes that leave a property out from `new_style`, which already contains the running animation's value, so the implicit 0%/100% keyframe became the current animated value and the element stayed there; keyframes are now computed from the style without animation/transition rules (`remove_animation_rules`, cascaded once per element and only when such rules exist). (2) `process_animations_for_style` dropped `Finished` animations, so the next rule change started them again from 0%; they now stay until cancelled (name leaves `animation-name`, or `display: none`). A third bug (stale Animations-level rule after a style-attribute-only restyle) is not patched. The JS site rule `site.css-animation-implicit-end` stays as the fallback (it is harmless with the patch: it only adds `animation-name:none` after the animation has ended). An upstream issue draft is in preparation. |

## servo-constellation 0.1.4

Source: the crates.io package `servo-constellation` 0.1.4 (sha256
`bb8e2c3ab634b7c7c86c3b9de6d0f97b188113bdf35f1cc25849d66e59e95130`), unpacked as published,
minus `.cargo_vcs_info.json` and the package's own `Cargo.lock`, plus:

| Patch | Why |
| --- | --- |
| `patches/servo-constellation-0001-discard-superseded-pending-iframe-pipeline.patch` | Companion of servo-script 0004: when a pipeline that is still a *pending* change of an iframe (no document yet) asks to navigate the frame, `load_url` has the parent start a new pipeline and now also discards the superseded pending one (`handle_abort_load_url_msg`), instead of leaving it pending forever. Active documents navigating themselves are unaffected (`source_id == ctx.pipeline_id`). |

## servo-layout 0.1.4

Source: the crates.io package `servo-layout` 0.1.4 (sha256
`f48e406d04bc5e8da746f44e457a875fd25d01776b72536210b9559b6e615e0d`), unpacked as published,
minus `.cargo_vcs_info.json` and the package's own `Cargo.lock`, plus:

| Patch | Why |
| --- | --- |
| `patches/servo-layout-0001-scrollable-overflow-no-panic.patch` | `BoxFragment::scrollable_overflow()` (and `FragmentTree::scrollable_overflow()`, `PositioningFragment::scrollable_overflow_for_parent()`) `expect`ed that the overflow pass had already visited the fragment. A fragment it never reached (one created after the last overflow calculation, or not reachable from the fragment tree's roots) panicked with `Should only call scrollable_overflow() after calculating overflow` (`fragment_tree/box_fragment.rs:266`) on the script thread, killing the page: the v0.4 Passwords page in the Windows feature shots (a private CI run, f02) stayed at "Looking..." with an empty import dropdown. The readers now fall back to the fragment's padding box / own rect / the initial containing block and log one warning per process. Upstream later made overflow lazy and recalculated on demand (servo/servo#44854), which removes the `expect`s. The UI also stopped giving the hidden native `<select>` behind the glass dropdown a box (`.dd-native` is `display: none`, design/tokens.css), the likely trigger. Regression note: docs/REGRESSIONS.md. |

## surfman 0.11.0

Source: the crates.io package `surfman` 0.11.0 (sha256
`04d3973284ef0dc7362cdaee41c2356c4c39a5a5ebd8490c5163ebf21a7adc02`), unpacked as published,
minus `.cargo_vcs_info.json`, the package's own `Cargo.lock` and `android-example/`, plus:

| Patch | Why |
| --- | --- |
| `patches/surfman-0001-skip-software-dxgi-adapter.patch` | Windows (ANGLE): `Connection::create_adapter()` (what Servo's `WindowRenderingContext` uses) is `create_hardware_adapter()` = the first DXGI adapter whose vendor is not Intel. Since Windows 8 DXGI always enumerates the "Microsoft Basic Render Driver" (vendor 0x1414, device 0x8c, `DXGI_ADAPTER_FLAG_SOFTWARE`), so on a PC whose only GPU is Intel (most laptops) the first non-Intel adapter *is* that software adapter: WebRender and the shell's glass compositor were then rasterized by WARP on the CPU. Adapters are now chosen as: hardware matching the vendor preference, else any hardware adapter, else the first one (a PC without a GPU driver still runs, on WARP). The chosen adapter is logged (`surfman: D3D11 adapter ...`). Not yet reported upstream (servo/surfman `src/platform/windows/angle/device.rs`). Windows-target `cargo check` only in the sandbox; needs a real Windows run. |

To refresh for a new Servo release: unpack the new `servo-script` / `servo-net` / `servo-constellation` / `servo-layout` / `stylo` / `surfman` crate here, re-apply every
patch with `git apply -p1 --directory=third_party/<crate> patches/<file>`, update the
version in this file, and run `cargo metadata` so `Cargo.lock` follows. Drop a patch (and the
whole vendored copy once none are left) when the fix lands upstream.
