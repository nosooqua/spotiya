# Local librespot crates

Source: https://github.com/klockky/librespot/tree/ebacab525375858be6932cbc6c9867d74c957f7a
License: MIT (see LICENSE).

Only core and audio are vendored. Cargo patches these two crates for the pinned
fork; all other librespot crates retain their pinned Git source. The workspace
metadata is expanded in each manifest, and oauth/protocol retain that Git pin.

The optional external decoder and its key service have been removed. Audio keys
use the ordinary AP AES request only. Login5 credential rotation, packet validation,
request cleanup, and fail-closed playback on key errors are preserved.
`AudioKeyMaterial` and `new_material` retain the interface used by the pinned
playback crate, with AES as the only key variant.

Validate from the repository root:

```sh
cargo test --locked -p librespot-core -p librespot-audio --no-default-features --features librespot-core/rustls-tls-webpki-roots,librespot-audio/rustls-tls-webpki-roots --lib
```
