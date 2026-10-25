# PDF

PDF is OneOS's PDF editor: read, organise, combine, split, annotate, fill, sign and protect PDFs, running in the browser
as WebAssembly inside OneOS.

It is based on **PdfCraft** by the ArtCraft team (MIT OR Apache-2.0). This repository is OneOS's fork: it keeps the upstream crate
names and file layout so upstream changes merge cleanly, and carries the OneOS host bridge and the OneOS identity.

## What is different from upstream

- **Host bridge.** The web build (`apps/pdfcraft-web`) talks to OneOS over `postMessage` to the one parent origin the frame
  page declares: open from bytes, report unsaved changes, and save as a new version of the file with a stated base version. See
  `apps/pdfcraft-web/src/main.rs` (`mod host`).
- **Size build.** `cargo build --profile web-release --target wasm32-unknown-unknown -p pdfcraft-web --locked`, then
  `wasm-bindgen --target web` and `wasm-opt -Oz`. The OneOS build script (`workspace/scripts/build-craft-libs.mjs`) does all of it.
- **Languages.** The web build ships the French, German, Spanish, Italian and Brazilian Portuguese catalogs besides English, to stay
  under the 25 MiB a static asset may be.
- **Identity.** The product is called PDF; the app id is `one.wkspace.pdf`; the upstream community links and brand marks are removed.

## Building the desktop app

```
cargo run --release -p pdfcraft
```

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The upstream copyright notice is kept in
[LICENSE-MIT](LICENSE-MIT) and [NOTICE](NOTICE); third-party material is listed in [ATTRIBUTION.md](ATTRIBUTION.md).
