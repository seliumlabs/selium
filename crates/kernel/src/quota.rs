use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use selium_abi::ResourceClass;

use crate::{Error, Result};

/// Host-held, tenant-scoped quota counter table.
///
/// Quotas are the accountant-authored ceilings the host enforces
/// synchronously at allocation: a grant admits a class of operation, a quota
/// caps its extent. The accounting guest is the sole author — every author
/// (`set`/`clear`) is gated by the bootstrap-provisioned `QuotaWrite`
/// capability in the runtime.
///
/// Usage accounting is independent of authorship: allocations are tracked
/// against the tenant's (tenant, class) counter even before the accountant
/// authors a ceiling (an implicit unlimited entry), so a `set` that arrives
/// after the tenant is already running inherits its live usage — the
/// accountant authors ceilings asynchronously and must see what the tenant
/// actually holds, not what was allocated since its last tick.
#[derive(Clone)]
pub struct QuotaTable {
    inner: Arc<QuotaTableInner>,
}

#[derive(Debug, Clone, Copy)]
struct QuotaState {
    /// Ceiling the accountant authored for this window (`u64::MAX` for an
    /// implicit unlimited entry).
    limit: u64,
    /// Usage allocated against the ceiling.
    used: u64,
}

/// A resource's outstanding quota reservation: the tenant whose ceiling was
/// charged and the amount still held.
#[derive(Debug, Clone)]
struct Reservation {
    tenant: String,
    amount: u64,
}

struct QuotaTableInner {
    quotas: Mutex<HashMap<(String, ResourceClass), QuotaState>>,
    /// Which tenant's counter each resource's reservation was charged to.
    ///
    /// A resource may be minted under a serving tenant that differs from the
    /// allocating process's own tenant (a root principal, or tenant-scoped
    /// delegation). A release must credit the ledger the reservation was
    /// charged against, so the metering domain records the debit here rather
    /// than letting the release path re-derive a tenant. Consumed on release.
    reservations: Mutex<HashMap<(ResourceClass, u64), Reservation>>,
}

impl QuotaTable {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(QuotaTableInner {
                quotas: Mutex::new(HashMap::new()),
                reservations: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Stores (or re-stamps) a quota counter for a tenant and resource class.
    ///
    /// The authored ceiling updates in place and the tracked usage is
    /// preserved: a repeated `set` changes the cap, not the live consumption
    /// observed against it (frees already released their reservation).
    pub fn set(&self, tenant: impl Into<String>, class: ResourceClass, limit: u64) {
        let mut quotas = self.inner.quotas.lock();
        let entry = quotas.entry((tenant.into(), class)).or_default();
        entry.limit = limit;
    }

    /// Removes a quota counter, restoring unrestricted allocation for the
    /// tenant and class.
    pub fn clear(&self, tenant: &str, class: ResourceClass) {
        self.inner
            .quotas
            .lock()
            .remove(&(tenant.to_string(), class));
    }

    /// Returns the currently authored ceiling for a tenant and class, if any.
    pub fn lookup(&self, tenant: &str, class: ResourceClass) -> Option<u64> {
        self.inner
            .quotas
            .lock()
            .get(&(tenant.to_string(), class))
            .map(|state| state.limit)
    }

    /// Returns the usage allocated against the tenant's ceiling since the last
    /// author, zero when the tenant has no entry.
    pub fn used(&self, tenant: &str, class: ResourceClass) -> u64 {
        self.inner
            .quotas
            .lock()
            .get(&(tenant.to_string(), class))
            .map(|state| state.used)
            .unwrap_or(0)
    }

    /// Reserves `amount` against the tenant's ceiling for `class`.
    ///
    /// The platform tenant (empty name) is unrestricted and untracked.
    /// Tenants without an authored ceiling are unrestricted but **tracked**:
    /// the reservation records against an implicit unlimited counter so a
    /// later `set` inherits the live usage (the accountant authors ceilings
    /// asynchronously, after tenants are already running). Otherwise the
    /// reservation fails when it would exceed the ceiling, before any
    /// resource is granted.
    pub fn try_consume(&self, tenant: &str, class: ResourceClass, amount: u64) -> Result<()> {
        if tenant.is_empty() {
            return Ok(());
        }
        let mut quotas = self.inner.quotas.lock();
        let state = quotas
            .entry((tenant.to_string(), class.clone()))
            .or_default();
        let remaining = state.limit.saturating_sub(state.used);
        if amount > remaining {
            return Err(Error::QuotaExceeded {
                tenant: tenant.to_string(),
                class,
            });
        }
        state.used = state.used.saturating_add(amount);
        Ok(())
    }

    /// Reserves `amount` against the tenant's counter **without denying**,
    /// even past the authored ceiling.
    ///
    /// Used by resource *transfers* (a handed-off region entering the
    /// receiver's resource table): the item must land, and the receiving
    /// tenant ends up over its ceiling — subsequent allocations are denied
    /// and metering surfaces the anomaly. See the accountant spec's
    /// poisoned-handoff note.
    pub fn force_consume(&self, tenant: &str, class: ResourceClass, amount: u64) {
        if tenant.is_empty() {
            return;
        }
        let mut quotas = self.inner.quotas.lock();
        let state = quotas.entry((tenant.to_string(), class)).or_default();
        state.used = state.used.saturating_add(amount);
    }

    /// Returns `amount` to the tenant's counter for `class` (a release at a
    /// free/close path). Missing entries and the platform tenant are no-ops.
    pub fn release(&self, tenant: &str, class: ResourceClass, amount: u64) {
        if tenant.is_empty() {
            return;
        }
        if let Some(state) = self
            .inner
            .quotas
            .lock()
            .get_mut(&(tenant.to_string(), class))
        {
            state.used = state.used.saturating_sub(amount);
        }
    }

    /// Records that a resource's `amount` reservation was charged to `tenant`'s
    /// counter for `class`, so the release path credits the same ledger.
    ///
    /// Call after a successful [`try_consume`](Self::try_consume) for the
    /// resource, once its identity is known. The platform tenant holds no
    /// tracked counter, so it records nothing.
    pub fn note_reservation(
        &self,
        class: ResourceClass,
        resource_id: u64,
        tenant: &str,
        amount: u64,
    ) {
        if tenant.is_empty() {
            return;
        }
        self.inner.reservations.lock().insert(
            (class, resource_id),
            Reservation {
                tenant: tenant.to_string(),
                amount,
            },
        );
    }

    /// Releases a resource's reservation, returning `amount` to the counter it
    /// was charged against. Returns whether a reservation was recorded.
    ///
    /// This is the symmetric counterpart of
    /// [`note_reservation`](Self::note_reservation): a cross-tenant mint is
    /// freed under the tenant whose ceiling absorbed it, not under the freeing
    /// process's own tenant.
    pub fn release_reservation(&self, class: ResourceClass, resource_id: u64) -> bool {
        let Some(reservation) = self
            .inner
            .reservations
            .lock()
            .remove(&(class.clone(), resource_id))
        else {
            return false;
        };
        self.release(&reservation.tenant, class, reservation.amount);
        true
    }

    /// Moves a resource's reservation from the tenant it was charged to onto
    /// `to`, returning `amount` to the original counter and force-consuming it
    /// against the receiving tenant.
    ///
    /// This is the ledger half of a cross-tenant handoff: the reservation
    /// follows the resource, so freeing it later credits whoever holds it now.
    /// A reservation that was never recorded (the platform tenant holds no
    /// tracked counter) is a no-op, and the caller keeps the transfer's
    /// accounting.
    pub fn transfer_reservation(
        &self,
        class: ResourceClass,
        resource_id: u64,
        to: &str,
        amount: u64,
    ) -> bool {
        let from = {
            let mut reservations = self.inner.reservations.lock();
            let Some(reservation) = reservations.get_mut(&(class.clone(), resource_id)) else {
                return false;
            };
            if reservation.tenant == to {
                return false;
            }
            std::mem::replace(&mut reservation.tenant, to.to_string())
        };
        self.release(&from, class.clone(), amount);
        self.force_consume(to, class, amount);
        true
    }
}

impl Default for QuotaState {
    fn default() -> Self {
        Self {
            limit: u64::MAX,
            used: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use selium_abi::ResourceClass;

    use super::*;

    #[test]
    fn a_reservation_releases_under_the_tenant_it_was_charged_to() {
        // A cross-tenant mint: the reservation is charged to `acme`'s ceiling
        // while the resource is owned by a root process. The release must
        // credit `acme`, not the freeing process's own (platform) tenant.
        let table = QuotaTable::new();
        table.set("acme", ResourceClass::SharedRegion, 128);
        table
            .try_consume("acme", ResourceClass::SharedRegion, 64)
            .expect("within acme ceiling");
        table.note_reservation(ResourceClass::SharedRegion, 7, "acme", 64);

        assert!(table.release_reservation(ResourceClass::SharedRegion, 7));
        assert_eq!(table.used("acme", ResourceClass::SharedRegion), 0);
        // The reservation is consumed: a second release is a no-op.
        assert!(!table.release_reservation(ResourceClass::SharedRegion, 7));
    }

    #[test]
    fn the_platform_tenant_holds_no_tracked_reservation() {
        let table = QuotaTable::new();
        table.note_reservation(ResourceClass::SharedRegion, 9, "", 4096);
        assert!(
            !table.release_reservation(ResourceClass::SharedRegion, 9),
            "the platform tenant is unrestricted, so there is nothing to credit"
        );
    }

    #[test]
    fn a_transferred_reservation_releases_under_the_receiving_tenant() {
        // Cross-tenant handoff: the reservation follows the resource, so a
        // later free credits whoever holds it now.
        let table = QuotaTable::new();
        table.set("acme", ResourceClass::SharedRegion, 1024);
        table.set("beta", ResourceClass::SharedRegion, 1024);
        table
            .try_consume("acme", ResourceClass::SharedRegion, 64)
            .expect("within acme ceiling");
        table.note_reservation(ResourceClass::SharedRegion, 3, "acme", 64);

        assert!(table.transfer_reservation(ResourceClass::SharedRegion, 3, "beta", 64));
        assert_eq!(table.used("acme", ResourceClass::SharedRegion), 0);
        assert_eq!(table.used("beta", ResourceClass::SharedRegion), 64);

        assert!(table.release_reservation(ResourceClass::SharedRegion, 3));
        assert_eq!(table.used("beta", ResourceClass::SharedRegion), 0);
    }

    // scenario: selium-abi/quota-hostcall-variants/quotaclear-removes-the-counter
    #[test]
    fn set_lookup_and_clear_cover_the_counter_lifecycle() {
        let table = QuotaTable::new();
        assert_eq!(table.lookup("acme", ResourceClass::SharedRegion), None);

        table.set("acme", ResourceClass::SharedRegion, 1024);
        assert_eq!(
            table.lookup("acme", ResourceClass::SharedRegion),
            Some(1024)
        );

        table.clear("acme", ResourceClass::SharedRegion);
        assert_eq!(table.lookup("acme", ResourceClass::SharedRegion), None);
    }

    #[test]
    fn consume_succeeds_within_ceiling_and_fails_beyond_it() {
        let table = QuotaTable::new();
        table.set("acme", ResourceClass::Volume, 100);
        table
            .try_consume("acme", ResourceClass::Volume, 60)
            .expect("within ceiling");
        assert_eq!(table.used("acme", ResourceClass::Volume), 60);
        assert!(matches!(
            table.try_consume("acme", ResourceClass::Volume, 41),
            Err(Error::QuotaExceeded { .. })
        ));
    }

    #[test]
    fn release_returns_usage_to_the_counter() {
        let table = QuotaTable::new();
        table.set("acme", ResourceClass::SharedRegion, 100);
        table
            .try_consume("acme", ResourceClass::SharedRegion, 80)
            .expect("within ceiling");
        table.release("acme", ResourceClass::SharedRegion, 80);
        table
            .try_consume("acme", ResourceClass::SharedRegion, 100)
            .expect("released usage restored");
    }

    #[test]
    fn missing_entry_and_platform_tenant_are_unrestricted() {
        let table = QuotaTable::new();
        // No entry: unrestricted.
        table
            .try_consume("acme", ResourceClass::HostQueue, 1)
            .expect("unrestricted without entry");
        // Platform tenant (empty name): unrestricted even with other entries.
        table.set("beta", ResourceClass::HostQueue, 1);
        table
            .try_consume("", ResourceClass::HostQueue, 10_000)
            .expect("platform tenant unrestricted");
    }

    // scenario: selium-accountant/quota-enforcement-at-allocation/pre-authoring-usage-is-inherited
    #[test]
    fn pre_authoring_usage_is_tracked_and_inherited_by_set() {
        let table = QuotaTable::new();
        // A tenant allocates before the accountant authors a ceiling:
        // unrestricted, but the usage is tracked.
        table
            .try_consume("acme", ResourceClass::SharedRegion, 65_536)
            .expect("unrestricted before authoring");
        assert_eq!(table.used("acme", ResourceClass::SharedRegion), 65_536);

        // The accountant later authors a one-page ceiling: the live usage is
        // inherited, so a further page is denied.
        table.set("acme", ResourceClass::SharedRegion, 65_536);
        assert!(matches!(
            table.try_consume("acme", ResourceClass::SharedRegion, 65_536),
            Err(Error::QuotaExceeded { .. })
        ));

        // Release restores headroom under the authored ceiling.
        table.release("acme", ResourceClass::SharedRegion, 65_536);
        table
            .try_consume("acme", ResourceClass::SharedRegion, 65_536)
            .expect("released usage restored");
    }

    #[test]
    fn restamp_updates_ceiling_and_preserves_usage() {
        let table = QuotaTable::new();
        table.set("acme", ResourceClass::Volume, 100);
        table
            .try_consume("acme", ResourceClass::Volume, 90)
            .expect("within ceiling");
        // Re-authoring changes the ceiling but does not reset live usage.
        table.set("acme", ResourceClass::Volume, 80);
        assert_eq!(table.used("acme", ResourceClass::Volume), 90);
        assert!(matches!(
            table.try_consume("acme", ResourceClass::Volume, 1),
            Err(Error::QuotaExceeded { .. })
        ));
    }

    #[test]
    fn process_class_round_trips_without_new_abi_variants() {
        let table = QuotaTable::new();
        // The Process dimension reuses the same generic counter as every
        // other class: author, reserve, release, and look it up.
        table.set("acme", ResourceClass::Process, 100);
        assert_eq!(table.lookup("acme", ResourceClass::Process), Some(100));
        table
            .try_consume("acme", ResourceClass::Process, 60)
            .expect("within ceiling");
        assert_eq!(table.used("acme", ResourceClass::Process), 60);
        // Over-ceiling reservation is denied.
        assert!(matches!(
            table.try_consume("acme", ResourceClass::Process, 41),
            Err(Error::QuotaExceeded { .. })
        ));
        // Release returns the slot; the counter clears.
        table.release("acme", ResourceClass::Process, 60);
        assert_eq!(table.used("acme", ResourceClass::Process), 0);
        table.clear("acme", ResourceClass::Process);
        assert_eq!(table.lookup("acme", ResourceClass::Process), None);
    }
}
