## Purpose

Defines the `sel` command-line client: a thin native wrapper over `selium-client` that drives the control plane's typed control surface over QUIC through the bridge, expressing operator intent (deploy, scale, stop, status, resolve, upload) without re-encoding it as text.

## Requirements

### Requirement: Bridge-Derived Connection

The `sel` CLI SHALL establish one QUIC connection to a caller-supplied connector address per invocation. It SHALL derive the connection's server name (`bridge.<tenant>`) and the control route (`sel://<tenant>/control`) from a caller-supplied tenant, trust a caller-supplied server root certificate, and present a caller-supplied client identity for mutual TLS.

#### Scenario: Tenant drives server name and control route

- **WHEN** the CLI is invoked with `--tenant acme` and a connector address
- **THEN** it SHALL connect with server name `bridge.acme` and open the control route `sel://acme/control`

#### Scenario: Client identity is presented

- **WHEN** `--client-cert` and `--client-key` are supplied
- **THEN** the connection SHALL present that identity for mutual TLS so the connector derives the client's tenant and fingerprint

### Requirement: Ephemeral Single-Request Lifecycle

Each invocation SHALL open one RPC channel to the control route, issue exactly one control request, await its reply, print the result, and exit. The CLI SHALL NOT hold a persistent connection, batch requests, or enter an interactive shell.

#### Scenario: One invocation performs one request

- **WHEN** any control verb is invoked
- **THEN** the CLI SHALL complete exactly one request/reply on one channel to the control route and exit

### Requirement: Deploy Expresses Desired State

The `deploy` subcommand SHALL translate a workload identifier, a replica count, and a module reference into a deployment request, and SHALL report the accepted outcome on success.

#### Scenario: Deploy is accepted

- **WHEN** `sel deploy api --replicas 3 --module api/v1` succeeds
- **THEN** the CLI SHALL report workload `api` accepted with 3 replicas and module reference `api/v1`

### Requirement: Scale and Stop Maintain Desired State

The `scale` subcommand SHALL translate a workload identifier and a new replica count into a scale request; the `stop` subcommand SHALL translate a workload identifier into a stop request. Both SHALL report their accepted outcome on success.

#### Scenario: Scale changes the replica count

- **WHEN** `sel scale api --replicas 5` succeeds
- **THEN** the CLI SHALL report workload `api` scaled to 5 replicas

#### Scenario: Stop removes a workload

- **WHEN** `sel stop api` succeeds
- **THEN** the CLI SHALL report workload `api` stopped

### Requirement: Status Reads the Recorded Deployment

The `status` subcommand SHALL read the last accepted desired state for a workload and SHALL distinguish a recorded deployment from a workload with none.

#### Scenario: Status returns the recorded deployment

- **WHEN** `sel status api` succeeds for a workload with recorded desired state
- **THEN** the CLI SHALL print the workload's recorded deployment (replica count and module reference)

#### Scenario: Status reports an unknown workload

- **WHEN** `sel status api` is invoked for a workload with no recorded deployment
- **THEN** the CLI SHALL report the workload as not found and exit non-zero

### Requirement: Resolve Queries Discovery

The `resolve` subcommand SHALL translate a URI into a resolve request and SHALL print the resolved target, or a not-found result when the URI resolves to nothing.

#### Scenario: Resolve returns a target

- **WHEN** `sel resolve sel://acme/lobby` succeeds
- **THEN** the CLI SHALL print the resolved target for that URI

### Requirement: Upload Stores Module Bytes

The `upload` subcommand SHALL read module bytes from a supplied file and send an upload request carrying a manifest name; it SHALL report the stored manifest on success and fail loudly when the file cannot be read.

#### Scenario: Upload stores a module

- **WHEN** `sel upload --manifest api/v1 --file ./api.wasm` succeeds
- **THEN** the CLI SHALL report the module stored under manifest `api/v1`

#### Scenario: Upload fails on an unreadable file

- **WHEN** the file named by `--file` cannot be read
- **THEN** the CLI SHALL print an error and exit non-zero without sending an upload request

### Requirement: Typed Failure Mapping

An accepted request whose delegated interaction was not applied SHALL be treated as a failed operation: the CLI SHALL print the delegation context and exit non-zero. A typed control error SHALL likewise print its failed step and context and exit non-zero.

#### Scenario: Deferred delegation surfaces as failure

- **WHEN** a deploy, scale, or stop request returns an accepted outcome whose delegated status is not applied
- **THEN** the CLI SHALL print the delegation context and exit non-zero

#### Scenario: Typed error carries step and context

- **WHEN** the control plane replies with a typed error (for example a failed discovery or storage step)
- **THEN** the CLI SHALL print the failed step and its context and exit non-zero