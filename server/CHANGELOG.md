# CHANGELOG

## v0.1.0

- Initial release of Selium

## v0.2.0

- Added support for message batching
- Message payload size limit (1MB) is now enforced on wire protocol
- Implemented QUIC Mutual TLS

## v0.3.0

- Added request-reply messaging pattern
- Implement protocol graceful shutdown
- Decoupled binary into library components for testing
- Features for Selium Cloud
- Bump dependency versions

## v0.3.1

- Remove openssl dependency
- Replace faulty Cloud certs
- Bug fixes for Selium Cloud

## v0.4.0

- Improve error reporting to client
- Fix race condition when replier stream rejoins topic, which could result in the replier being erroneously rejected

## v0.5.0

- Integrated Selium Log into pubsub broker
