## REMOVED Requirements

### Requirement: Well-Known Connector Channel Provisioning
Removed: system guests now create their own resources and register their own routes via `serve`, so the runtime no longer mints listener queues, injects shared ids as entrypoint arguments, or registers well-known URIs on a guest's behalf; route revocation on exit is owner-keyed in discovery.
