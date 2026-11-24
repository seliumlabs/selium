# Spec Delta

## Purpose

Define reproducible, consumer-tested publication of Selium's host runtime, CLI, libraries, and required WASM system guests without accidental partial or incompatible releases.

## ADDED Requirements

### Requirement: Tagged Release Publishes A Complete Usable Artifact
A platform release SHALL include the runtime, `sel` CLI, and matching system-guest WASM modules, with code-defined bootstrap material and documentation identifying its state and trust requirements.

#### Scenario: User runs the packaged platform
- **WHEN** a user downloads a tagged platform artifact on a supported target and invokes its documented smoke path
- **THEN** the runtime SHALL bootstrap the packaged system guests and the CLI SHALL reach the control plane without paths into the source checkout

#### Scenario: Component-only artifact is labelled
- **WHEN** a release contains only an embedder runtime or another subset
- **THEN** its name and documentation SHALL identify it as a component rather than a turnkey platform

### Requirement: Publication Requires The Full Gate And Artifact Smoke Tests
A tag SHALL publish only after the required Linux gate passes and each advertised binary target successfully runs a packaged smoke test.

#### Scenario: Target artifact cannot start
- **WHEN** a macOS, Windows, Linux, or ARM artifact fails its startup or guest-bootstrap smoke test
- **THEN** the release SHALL fail without presenting that target as supported

### Requirement: Crate Publication Is Ordered And Atomic In Intent
Workspace crates SHALL carry publishable versioned dependencies and an explicit dependency-ordered publication plan; failure SHALL stop subsequent dependent publication and SHALL be reported as an incomplete release.

#### Scenario: Dry run from packaged sources
- **WHEN** release automation runs `cargo publish --dry-run` for every publishable crate
- **THEN** packaged manifests SHALL resolve without repository-only path assumptions or unpublished dependency versions

#### Scenario: Dependency publication fails
- **WHEN** any crate fails publication
- **THEN** automation SHALL NOT continue publishing crates that depend on it and the release SHALL remain visibly incomplete

### Requirement: Downstream Guest Build Uses Supported Dependency Sources
An isolated consumer SHALL be able to build a guest and instantiate it against the matching host without inheriting repository-local patches.

#### Scenario: Clean consumer instantiates guest
- **WHEN** a fresh project consumes the published host and guest crates with no workspace patch table
- **THEN** it SHALL build a no-WASI guest using supported dependency sources and complete the ABI negotiation and golden-path smoke test

### Requirement: Host And Guest Packages Share A Compatibility Contract
Release tagging SHALL validate workspace, package, guest-module, and ABI versions together and SHALL publish the supported compatibility window or lockstep requirement.

#### Scenario: Mixed unsupported versions
- **WHEN** a user combines host and guest artifacts outside the published compatibility contract
- **THEN** startup SHALL fail with a version mismatch rather than silent request misinterpretation
