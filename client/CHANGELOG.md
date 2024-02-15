# CHANGELOG

## v0.1.0

- Initial release of Selium

## v0.2.0

- Added opt-in message batching functionality
- Added opt-in compression for Publisher and Subscriber streams
- Message payload size limit (1MB) is now enforced on wire protocol
- Implemented QUIC Mutual TLS

## v0.3.0

- Added request-reply messaging pattern
- Added connection reestablishment for failed connections
- Features for Selium Cloud
- Bump dependency versions

## v0.3.1

- Add closure so client can gracefully handle replier errors
- Add CA certificate
- Bug fix for Selium Cloud

## v0.4.0

- Add `tracing` lib to improve visibility
- Improve and customise keepalive semantics for each stream type
