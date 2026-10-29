# GLib 0.18 security backport

This directory contains the crates.io `glib` 0.18.5 release, under its original
MIT license (see LICENSE and COPYRIGHT). Registry cache metadata is omitted.
The original crate archive's SHA-256 is
`233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.

The only source change is in `src/variant_iter.rs`: make the output pointer
mutable and pass `&mut p` to `g_variant_get_child`. This backports the fix for
RUSTSEC-2024-0429 / GHSA-wrw7-89jp-8q8g:
https://github.com/gtk-rs/gtk-rs-core/pull/1343
https://rustsec.org/advisories/RUSTSEC-2024-0429.html

GTK3 and WebKitGTK in Tauri 2 require glib 0.18, so upgrading glib alone to 0.20
would introduce incompatible types. Cargo's root patch makes all consumers use
this fixed copy. No registry or git source exception is needed.

The original version is retained for dependency compatibility and provenance.
OSV therefore still identifies the old version; its advisory exception refers
to this source backport, not acceptance of the vulnerability.

Validate every affected iterator method with Quill's regression test. The
root Cargo profile optimizes GLib even in debug builds so the test covers the
compiler behavior that exposed the bug:

```sh
cd src-tauri
cargo test --test glib_variant_iter
```

Remove this directory, the Cargo patch, and the OSV exception together once
Tauri's Linux dependency tree uses a fixed upstream GLib release.
