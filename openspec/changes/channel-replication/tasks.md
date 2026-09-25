## 1. Channel Config

- [ ] 1.1 Add replication_factor to channel config
- [ ] 1.2 Add replica placement configuration

## 2. Replication Implementation

- [ ] 2.1 Implement write-master routing
- [ ] 2.2 Implement read-slave distribution
- [ ] 2.3 Implement replica synchronization

## 3. Election

- [ ] 3.1 Implement Raft-based election
- [ ] 3.2 Handle election timeout
- [ ] 3.3 Wire writer wakers to election complete

## 4. Backpressure

- [ ] 4.1 Implement write buffering during election
- [ ] 4.2 Implement re-queue on election complete

## 5. Testing

- [ ] 5.1 Test master failure and election
- [ ] 5.2 Test write backpressure
- [ ] 5.3 Test read-slave eventual consistency
