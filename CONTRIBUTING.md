# Contributing

Thanks for helping improve `mihomo-top`. The project is experimental, so clear reports of incorrect attribution, controller compatibility issues, and documentation gaps are especially valuable.

## Before opening a change

Open an issue or describe the intended change first when it alters the command line, JSON schema, controller trust boundary, or attribution semantics. These are user-facing contracts and need deliberate review.

Keep pull requests focused. Explain the user-visible behavior, update the English and Chinese documentation for interface changes, and add or adjust meaningful tests when behavior needs protection. Run the relevant Rust checks before submitting:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

The live controller lab is Linux-only and is not part of `cargo test` unless you opt in:

```sh
scripts/test-env.sh
```

## Reporting bugs

State the `mihomo-top` version, Linux distribution/kernel context, Rust version if built from source, mihomo version, the command used (with secrets removed), expected behavior, and observed behavior. For attribution reports, say whether the traffic was TCP or UDP and whether it belonged to the same user.

Do not publish controller secrets, private destination names, personally identifying process arguments, live IP addresses, or complete connection logs. Use documentation addresses and redacted values in examples.

## Design commitments

Contributions must preserve the v0.1 boundaries:

- The controller endpoint remains local numeric loopback only.
- Secret values never appear in output or diagnostics.
- Local attribution stays optional and best-effort; ambiguous visible TCP ownership is reported as `UNKNOWN` rather than asserted as resolved.
- UDP local process attribution is unsupported unless its semantics and limitations are designed and documented.
- The monitor remains usable without root and does not claim complete host traffic coverage.

By contributing, you agree that your contribution may be distributed under the [MIT License](LICENSE).
