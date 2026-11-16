# Contributing to clipr

Thanks for helping. clipr is a Windows game recorder and clipper written in Rust, with a Svelte UI.

## Building

You need:

- Rust (stable), installed with rustup, plus the **Visual Studio Build Tools** "Desktop development with C++" workload.
- Node.js 22.

Then:

```sh
cd ui && npm ci && npm run build && cd ..
cargo run -p clipr
```

To make the downloads (portable zip and installer) in `dist\`:

```sh
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

The installer needs [Inno Setup 6](https://jrsoftware.org/isdl.php); without it you only get the zip. CI builds
both on every push (the `clipr-windows` artifact).

## Checks

CI runs these on every push and pull request, on `windows-latest`. Run them locally before opening a PR:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cd ui && npm run check   # svelte-check
```

## Ground rules

- **Never inject into games.** clipr captures through Windows Graphics Capture and never hooks or modifies a game process. Please keep it that way; it is what makes clipr safe with anti-cheat.
- **Every feature is optional and explained.** Each integration, upload, cue or update check must be off by default or easy to disable, and the UI should say what it does.

## License

clipr is licensed under GPL-3.0 (see `LICENSE`). By contributing, you agree that your contributions are licensed under the same terms.
