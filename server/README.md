# Selium Server

This is the server-side binary for the Selium platform. It takes a very minimal set of
configuration items listed under "Usage" below, however it is recommended that you use
default parameters unless you have a specific reason not to.

## Getting Started

The Selium server binary can be downloaded from
[GitHub](https://github.com/orgs/seliumlabs/packages?repo_name=selium), or compiled from
[crates.io](https://crates.io/crates/selium-server). Once you have installed Selium
Server, its usage is very straightforward.

#### Start a test server

To start a server as quickly as possible **_for testing_**, you can use the `--self-signed`
flag. This will do exactly what you think it will, and generate its own TLS certificate.

>Note that using this option in production will leave clients exposed to
person-in-the-middle attacks!

```bash
$ selium-server --bind-addr=127.0.0.1:7001 --self-signed
```

Running this command will create a new server instance that listens on the loopback
interface, port 7001.

#### Start a production-ready server

For production use, you will likely want to generate a signed certificate keypair. Using
your own certificate authority for this process is perfectly acceptable, provided that
you are able to distribute this certificate to all potential clients in advance. For
instances where this is not possible, a third party CA provider like
[LetsEncrypt](https://letsencrypt.org) can help.

Here is a minimal example to start a production-ready instance of `selium-server`:

```bash
$ selium-server --bind-addr=0.0.0.0:7001 --key /path/to/server.key --cert /path/to/server.crt
```

Running this command will create a new server instance that listens on the public
interface, port 7001.

#### Debugging

`selium-server` can output comprehensive logs to help you debug issues with your Selium
instance. To enable, use the `-v` (verbose) flag:

```bash
$ selium-server ... -v # Display warnings
$ selium-server ... -vv # Display info
$ selium-server ... -vvv # Display debug
$ selium-server ... -vvvv # Display trace
```

## Usage

```
Usage: selium-server [OPTIONS] --bind-addr <BIND_ADDR> <--key <KEY>|--cert <CERT>|--self-signed>

Options:
  -a, --bind-addr <BIND_ADDR>
          Address to bind this server to
  -k, --key <KEY>
          TLS private key
  -c, --cert <CERT>
          TLS certificate
      --self-signed
          Autogenerate server cert (NOTE: This should only be used for testing!)
      --stateless-retry
          Enable stateless retries
      --keylog
          File to log TLS keys to for debugging
      --max-idle-timeout <MAX_IDLE_TIMEOUT>
          Maximum time in ms a client can idle waiting for data - default to 15 seconds [default: 15000]
  -v, --verbose...
          More output per occurrence
  -q, --quiet...
          Less output per occurrence
  -h, --help
          Print help
  -V, --version
          Print version
```
