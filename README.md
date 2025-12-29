# Selium

[![Crates.io][crates-badge]][crates-url]
[![MPL2 licensed][mpl-badge]][mpl-url]
[![Build Status][build-badge]][build-url]
[![Audit Status][audit-badge]][audit-url]

[crates-badge]: https://img.shields.io/crates/v/selium.svg
[crates-url]: https://crates.io/crates/selium
[mpl-badge]: https://img.shields.io/badge/licence-MPL2-blue.svg
[mpl-url]: https://github.com/seliumlabs/selium/blob/main/LICENCE
[build-badge]: https://github.com/seliumlabs/selium/actions/workflows/ci.yml/badge.svg
[build-url]: https://github.com/seliumlabs/selium/actions/workflows/ci.yml
[audit-badge]: https://github.com/seliumlabs/selium/actions/workflows/audit.yml/badge.svg
[audit-url]: https://github.com/seliumlabs/selium/actions/workflows/audit.yml

Selium is a software framework and runtime for building scalable, connected applications. With Selium, you can compose entire application stacks with **strict capabilities**, **typed I/O**, and **zero infrastructure**.

You get the ergonomics of an application platform, with the safety properties of a capability system:

- **Zero DevOps**: a batteries-included platform that is entirely composable in code.
- **Ergonomic I/O everywhere**: first class composable messaging, even when interacting with network protocols.
- **End-to-end type safety**: seamless type support across process and network boundaries.
- **No ambient authority**: guests only receive the capabilities you grant them at runtime.

**Status:** alpha (APIs and project structure are still evolving).

---

## What happened to the messaging platform?

**The previous project can be found in the _legacy_ branch.**

A messaging platform was never our end-goal. We have taken all of our learning from building that platform and applied it to the network stack for Selium 1.0.

---

## Getting started

The fastest way to orient yourself is by [checking out an example](examples/echo/).

---

## Contributing

We'd love your help! Check out the issue log or join in a discussion to get started.

See `AGENTS.md` for contribution rules, including: stable Rust only, `tracing` logging, and International English.

---

## Licence

MPL v2 (see `LICENCE`)
