//! Selium accounting system guest — the resource-driven revenue control loop.
//!
//! The crate ships two entrypoints over one module, bootstrapped as two
//! system-guest descriptors:
//!
//! - **`bookkeeper`** (one per host): maintains the live local-process
//!   inventory from lifecycle activity events, samples each process's metering
//!   observation once per second, differences cumulative counters
//!   (cpu/bandwidth) and samples gauges (memory/storage), reduces those into
//!   per-tenant buckets, and publishes the buckets to a shared-memory topic.
//! - **`accountant`** (single logical instance): merges bookkeeper buckets,
//!   rolls them into per-minute billing windows appended to a durable usage
//!   ledger (replayed on restart), evaluates the paid plan vs the opt-in
//!   overage budget, drives the paid/in-overage/at-budget/delinquent account
//!   state machine, and authors enforcement state — `QuotaSet` counters and a
//!   published narrowing set consumed by the bridge-server at conferral.
//!
//! The operator/billing control surface (`AccountantControl`) is served over a
//! root-namespace RPC listener: `SetPlan`/`SetOverage` author the ceilings and
//! `MarkDelinquent`/`MarkRestored` drive the rare billing-state transitions.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context as _;
use rkyv::{Archive, Deserialize, Serialize};
use selium_abi::{
    ActivityKind, Capability, CapabilityGrant, MeteringObservation, ProcessId, ResourceClass,
    ResourceKind, ResourceSelector,
};
use selium_guest::{
    ActivityLog, Context, DurableLog, Instant, Metering, ResourceListener, ResourceTarget, Serve,
    Timer, entrypoint, info, mark_ready, process_tenant, quota_set, spawn, time, warn,
};
use selium_service::{AccountantControl, AccountantControlResponse, MeteringBucket, TenantPlan};
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{
    LiveTable,
    framed::{FramedRead, FramedWrite},
    pubsub::{Publisher, Subscriber},
};

/// The accountant's published narrowing live table: `tenant -> rkyv-encoded
/// Vec<Capability>` of capabilities to subtract at conferral.
pub type NarrowingLiveTable = LiveTable<String, Vec<u8>, ShmTransport>;

/// Accountant entrypoint export name.
pub const ACCOUNTANT_ENTRYPOINT: &str = "accountant";
/// Bookkeeper entrypoint export name.
pub const BOOKKEEPER_ENTRYPOINT: &str = "bookkeeper";
/// Serving route path for the bookkeeper's bucket topic (`sel:///accounting-buckets`).
pub const BUCKET_TOPIC_PATH: &str = "accounting-buckets";
/// Serving route path for the accountant's control surface (`sel:///accountant`).
pub const CONTROL_PATH: &str = "accountant";
/// Default per-tenant process-count ceiling, authored unless an operator
/// raised it via the `SetProcessQuota` control.
const DEFAULT_PROCESS_QUOTA: u64 = 100;
/// Durable log name for the usage ledger (per-minute windows + policy records).
pub const LEDGER_LOG: &str = "selium.accountant.ledger";
/// Serving route path for the accountant's narrowing live table (`sel:///accounting-narrowing`).
pub const NARROWING_TABLE_PATH: &str = "accounting-narrowing";
/// Ring capacity for the bucket topic and narrowing table.
const TOPIC_CAPACITY: u64 = 64 * 1024;
/// Seconds in one billing window.
const WINDOW_SECS: u64 = 60;

/// Per-dimension usage in one sampling interval or billing window.
///
/// Counter dimensions (`cpu_instructions`, `bandwidth_bytes`) accumulate; gauge
/// dimensions (`memory_bytes`, `storage_bytes`) carry a point-in-time reading
/// (windowed reduction takes the peak).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct Usage {
    /// CPU executed instructions.
    pub cpu_instructions: u64,
    /// Memory bytes.
    pub memory_bytes: u64,
    /// Storage bytes.
    pub storage_bytes: u64,
    /// Bandwidth bytes.
    pub bandwidth_bytes: u64,
}

/// A record appended to the durable usage ledger.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum LedgerRecord {
    /// A billing window's per-tenant usage, with its billable overage.
    Window {
        /// Tenant the window aggregates.
        tenant: String,
        /// Window start second (aligned to a minute boundary).
        window_start_s: u64,
        /// Windowed usage per dimension.
        usage: Usage,
        /// Billable overage (usage above the paid plan) per dimension.
        overage: Usage,
    },
    /// The operator authored the tenant's paid plan.
    SetPlan {
        /// Tenant whose plan is authored.
        tenant: String,
        /// Per-dimension soft ceilings.
        plan: Usage,
    },
    /// The operator authored the tenant's opt-in overage budget.
    SetOverage {
        /// Tenant whose overage budget is authored.
        tenant: String,
        /// Per-dimension hard-ceiling additions over the plan.
        overage: Usage,
    },
    /// The tenant was marked delinquent.
    Delinquent {
        /// Suspended tenant.
        tenant: String,
    },
    /// The tenant was restored to good standing.
    Restored {
        /// Restored tenant.
        tenant: String,
    },
    /// The operator authored the tenant's per-tenant process-count ceiling.
    SetProcessQuota {
        /// Tenant whose process ceiling is authored.
        tenant: String,
        /// Process-count ceiling.
        processes: u64,
    },
}

/// The account states mapped by the revenue control loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    /// Usage is within the paid plan.
    Paid,
    /// Usage exceeds the plan but stays within the hard ceiling.
    InOverage,
    /// Usage reached the hard ceiling (preemptively capped by the chokepoints).
    AtBudget,
    /// The tenant is billing-suspended: grants narrowed to nothing, quotas zero.
    Delinquent,
}

/// One tenant's live account state.
#[derive(Debug, Clone)]
pub struct Account {
    /// Paid plan (soft ceilings).
    plan: Usage,
    /// Opt-in overage budget (hard ceiling = plan + overage).
    overage: Usage,
    /// The operator-authored process-count ceiling (default 100).
    process_quota: u64,
    /// Billing suspension flag.
    delinquent: bool,
    /// The last rolled window's usage.
    last_usage: Usage,
}

/// Enforcement state authored from an account: quota values and the narrowing
/// set the bridge-server subtracts from baseline conferral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enforcement {
    /// Per-minute CPU instruction ceiling (the runtime translates it into a
    /// per-process engine execution budget).
    pub cpu_ceiling: u64,
    /// Shared-memory ceiling.
    pub memory_quota: u64,
    /// Storage ceiling (authored for both durable-log and blob-store classes).
    pub storage_quota: u64,
    /// Per-tenant process-count ceiling.
    pub process_quota: u64,
    /// Capabilities to subtract from a tenant's baseline grants at conferral.
    pub narrowing: Vec<Capability>,
}

/// The bookkeeper's retained per-process cumulative counters, kept to
/// difference incremental consumption between ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CumulativeCounters {
    /// Last observed cumulative CPU instructions.
    pub cpu_instructions: u64,
    /// Last observed cumulative bandwidth bytes.
    pub bandwidth_bytes: u64,
}

/// One sample under reduction: a live process, its tenant, and its current
/// metering observation.
pub struct ProcessSample {
    /// Sampled process.
    pub process_id: ProcessId,
    /// The process's tenant, when known.
    pub tenant: Option<String>,
    /// The current metering observation.
    pub observation: MeteringObservation,
}

/// A per-minute billing window accumulating merged bookkeeper buckets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollingWindow {
    /// Window start second (minute-aligned).
    pub start_s: u64,
    /// Per-tenant windowed usage.
    pub usage: HashMap<String, Usage>,
}

/// The replayed usage projection: per-tenant windowed history, plus the policy
/// records that rebuild account state on restart.
#[derive(Debug, Clone, Default)]
pub struct AccountBook {
    /// Per-tenant live account projection.
    pub accounts: HashMap<String, Account>,
    /// Per-tenant windowed usage history (window start second → usage/overage).
    pub history: HashMap<String, Vec<(u64, Usage, Usage)>>,
}

/// Shared accounting state handed between the rolling loop and RPC handlers.
struct Shared {
    accounts: AccountBook,
    ledger: DurableLog,
    buckets: Subscriber<MeteringBucket, ShmTransport>,
    narrowing: Arc<LiveTable<String, Vec<u8>, ShmTransport>>,
    window: Option<RollingWindow>,
}

impl Usage {
    /// Adds counter dimensions (`cpu`, `bandwidth`).
    fn add_counters(&mut self, other: Usage) {
        self.cpu_instructions = self.cpu_instructions.saturating_add(other.cpu_instructions);
        self.bandwidth_bytes = self.bandwidth_bytes.saturating_add(other.bandwidth_bytes);
    }

    /// Merges gauge dimensions (`memory`, `storage`) taking the peak reading.
    fn merge_gauges(&mut self, other: Usage) {
        self.memory_bytes = self.memory_bytes.max(other.memory_bytes);
        self.storage_bytes = self.storage_bytes.max(other.storage_bytes);
    }

    /// Returns the per-dimension excess of `self` over `ceiling`.
    pub fn over(&self, ceiling: Usage) -> Usage {
        Usage {
            cpu_instructions: self
                .cpu_instructions
                .saturating_sub(ceiling.cpu_instructions),
            memory_bytes: self.memory_bytes.saturating_sub(ceiling.memory_bytes),
            storage_bytes: self.storage_bytes.saturating_sub(ceiling.storage_bytes),
            bandwidth_bytes: self.bandwidth_bytes.saturating_sub(ceiling.bandwidth_bytes),
        }
    }

    /// Returns whether any dimension exceeds `ceiling`.
    pub fn exceeds(&self, ceiling: Usage) -> bool {
        self.cpu_instructions > ceiling.cpu_instructions
            || self.memory_bytes > ceiling.memory_bytes
            || self.storage_bytes > ceiling.storage_bytes
            || self.bandwidth_bytes > ceiling.bandwidth_bytes
    }
}

impl From<&MeteringBucket> for Usage {
    fn from(bucket: &MeteringBucket) -> Self {
        Self {
            cpu_instructions: bucket.cpu_instructions,
            memory_bytes: bucket.memory_bytes,
            storage_bytes: bucket.storage_bytes,
            bandwidth_bytes: bucket.bandwidth_bytes,
        }
    }
}

impl From<&TenantPlan> for Usage {
    fn from(plan: &TenantPlan) -> Self {
        Self {
            cpu_instructions: plan.cpu_instructions,
            memory_bytes: plan.memory_bytes,
            storage_bytes: plan.storage_bytes,
            bandwidth_bytes: plan.bandwidth_bytes,
        }
    }
}

impl Account {
    /// The account state derived from policy and the last windowed usage.
    pub fn state(&self) -> AccountState {
        if self.delinquent {
            AccountState::Delinquent
        } else if self.last_usage.exceeds(self.hard_ceiling()) {
            AccountState::AtBudget
        } else if self.last_usage.exceeds(self.plan) {
            AccountState::InOverage
        } else {
            AccountState::Paid
        }
    }

    /// The hard ceiling: plan plus the opt-in overage budget.
    pub fn hard_ceiling(&self) -> Usage {
        Usage {
            cpu_instructions: self
                .plan
                .cpu_instructions
                .saturating_add(self.overage.cpu_instructions),
            memory_bytes: self
                .plan
                .memory_bytes
                .saturating_add(self.overage.memory_bytes),
            storage_bytes: self
                .plan
                .storage_bytes
                .saturating_add(self.overage.storage_bytes),
            bandwidth_bytes: self
                .plan
                .bandwidth_bytes
                .saturating_add(self.overage.bandwidth_bytes),
        }
    }

    /// Applies one ledger record in replay order.
    pub fn apply_record(&mut self, record: &LedgerRecord) {
        match record {
            LedgerRecord::Window { usage, overage, .. } => {
                self.last_usage = *usage;
                let _ = overage; // billable overage is carried in the ledger, not the projection
            }
            LedgerRecord::SetPlan { plan, .. } => self.plan = *plan,
            LedgerRecord::SetOverage { overage, .. } => self.overage = *overage,
            LedgerRecord::SetProcessQuota { processes, .. } => self.process_quota = *processes,
            LedgerRecord::Delinquent { .. } => self.delinquent = true,
            LedgerRecord::Restored { .. } => self.delinquent = false,
        }
    }
}

impl Default for Account {
    fn default() -> Self {
        Self {
            plan: Usage::default(),
            overage: Usage::default(),
            process_quota: DEFAULT_PROCESS_QUOTA,
            delinquent: false,
            last_usage: Usage::default(),
        }
    }
}

impl RollingWindow {
    /// Merges a bookkeeper bucket into the window: counters accumulate and
    /// gauges take the peak reading across the window's samples.
    pub fn merge(&mut self, bucket: &MeteringBucket) {
        let entry = self.usage.entry(bucket.tenant.clone()).or_default();
        entry.add_counters(Usage {
            cpu_instructions: bucket.cpu_instructions,
            memory_bytes: 0,
            storage_bytes: 0,
            bandwidth_bytes: bucket.bandwidth_bytes,
        });
        entry.merge_gauges(Usage {
            cpu_instructions: 0,
            memory_bytes: bucket.memory_bytes,
            storage_bytes: bucket.storage_bytes,
            bandwidth_bytes: 0,
        });
    }

    /// Rolls the window into ledger records, returning one record per tenant.
    pub fn into_records(self, accounts: &HashMap<String, Account>) -> Vec<LedgerRecord> {
        let mut records = Vec::new();
        for (tenant, usage) in self.usage {
            let plan = accounts
                .get(&tenant)
                .map(|account| account.plan)
                .unwrap_or_default();
            let overage = usage.over(plan);
            records.push(LedgerRecord::Window {
                tenant,
                window_start_s: self.start_s,
                usage,
                overage,
            });
        }
        records
    }
}

impl AccountBook {
    /// Applies one ledger record in replay order.
    pub fn apply_record(&mut self, record: LedgerRecord) {
        match &record {
            LedgerRecord::Window {
                tenant,
                window_start_s,
                usage,
                overage,
            } => {
                self.accounts
                    .entry(tenant.clone())
                    .or_default()
                    .apply_record(&record);
                self.history.entry(tenant.clone()).or_default().push((
                    *window_start_s,
                    *usage,
                    *overage,
                ));
            }
            LedgerRecord::SetPlan { tenant, .. }
            | LedgerRecord::SetOverage { tenant, .. }
            | LedgerRecord::SetProcessQuota { tenant, .. }
            | LedgerRecord::Delinquent { tenant }
            | LedgerRecord::Restored { tenant } => {
                self.accounts
                    .entry(tenant.clone())
                    .or_default()
                    .apply_record(&record);
            }
        }
    }

    /// Re-applies replayed record payloads in log order. Undecodable records
    /// are skipped with a warning (a torn write must not lose the projection).
    pub fn rebuild_payloads(&mut self, payloads: impl IntoIterator<Item = Vec<u8>>) {
        for payload in payloads {
            match selium_abi::decode_rkyv::<LedgerRecord>(&payload) {
                Ok(record) => self.apply_record(record),
                Err(error) => warn!("accountant: skipping undecodable ledger record: {error}"),
            }
        }
    }

    /// Replays the durable ledger into the projection.
    pub fn rebuild(&mut self, ledger: &DurableLog) -> selium_guest::Result<()> {
        let payloads = ledger
            .replay(None, u32::MAX)?
            .into_iter()
            .map(|record| record.payload);
        self.rebuild_payloads(payloads);
        Ok(())
    }
}

impl Shared {
    /// Merges one bucket into the current window, starting a new window at the
    /// bucket's minute boundary when absent.
    fn merge_bucket(&mut self, bucket: MeteringBucket) {
        let window_start_s = bucket_second(&bucket);
        self.window = Some(match self.window.take() {
            Some(window) if window.start_s == window_start_s => window,
            _ => RollingWindow {
                start_s: window_start_s,
                usage: HashMap::new(),
            },
        });
        if let Some(window) = self.window.as_mut() {
            window.merge(&bucket);
        }
    }

    /// Rolls the current window into the ledger (appending one record per
    /// tenant) and returns the rolled records.
    fn roll_window(&mut self, window: RollingWindow) -> Vec<LedgerRecord> {
        let records = window.into_records(&self.accounts.accounts);
        for record in &records {
            self.append_record(record);
            self.accounts.apply_record(record.clone());
        }
        records
    }

    /// Appends a ledger record, updated the projection, and re-authors the
    /// tenant's enforcement state.
    fn apply_policy(&mut self, tenant: &str, record: LedgerRecord) {
        self.append_record(&record);
        self.accounts.apply_record(record);
        self.author_enforcement(tenant);
    }

    /// Appends an encoded ledger record, tolerating encode/append failures with
    /// a warning (usage history is advisory; enforcement is the preemptive path).
    fn append_record(&mut self, record: &LedgerRecord) {
        let timestamp_ms = time::now()
            .map(|nanos| nanos / 1_000_000)
            .unwrap_or_default();
        match selium_abi::encode_rkyv(record) {
            Ok(payload) => {
                if let Err(error) = self.ledger.append(timestamp_ms, Vec::new(), payload) {
                    warn!("accountant: ledger append failed: {error}");
                }
            }
            Err(error) => warn!("accountant: ledger record encode failed: {error}"),
        }
    }

    /// Authors the tenant's enforcement state (quotas + narrowing) from its
    /// current account projection.
    fn author_enforcement(&mut self, tenant: &str) {
        let account = self
            .accounts
            .accounts
            .get(tenant)
            .cloned()
            .unwrap_or_default();
        let enforcement = enforcement_for(
            account.plan,
            account.overage,
            account.process_quota,
            account.delinquent,
        );

        for class in [ResourceClass::SharedRegion] {
            if let Err(error) = quota_set(tenant, class.clone(), enforcement.memory_quota) {
                warn!(tenant, class = ?class, "accountant: quota set failed: {error}");
            }
        }
        for class in [ResourceClass::DurableLog, ResourceClass::BlobStore] {
            if let Err(error) = quota_set(tenant, class.clone(), enforcement.storage_quota) {
                warn!(tenant, class = ?class, "accountant: quota set failed: {error}");
            }
        }
        if let Err(error) = quota_set(tenant, ResourceClass::Process, enforcement.process_quota) {
            warn!(tenant, "accountant: process quota set failed: {error}");
        }
        // The CPU ceiling is a quota dimension (not an allocation gate): the
        // runtime translates it into per-process engine execution budgets on
        // its minute refresh. Zeroed for a delinquent tenant alongside its
        // other quotas.
        if let Err(error) = quota_set(tenant, ResourceClass::Cpu, enforcement.cpu_ceiling) {
            warn!(tenant, "accountant: cpu ceiling set failed: {error}");
        }

        match selium_abi::encode_rkyv(&enforcement.narrowing) {
            Ok(bytes) => {
                if let Err(error) = self.narrowing.set(tenant.to_string(), bytes) {
                    warn!(tenant, "accountant: narrowing publish failed: {error}");
                }
            }
            Err(error) => warn!("accountant: narrowing encode failed: {error}"),
        }
    }
}

/// The grant set assigned to the accountant: sole quota authorship, storage for
/// the usage ledger, shared memory for the narrowing table, host queues for the
/// control listener, and root-namespace route registration.
pub fn accountant_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(Capability::QuotaWrite, Vec::new()),
        CapabilityGrant::new(
            Capability::Storage,
            vec![ResourceSelector::ResourceClass(ResourceClass::DurableLog)],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        ),
        CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
    ]
}

/// Every capability variant: a delinquent tenant's narrowing set, so conferral
/// reduces the baseline grants to nothing.
pub fn all_capabilities() -> Vec<Capability> {
    vec![
        Capability::ProcessLifecycle,
        Capability::SharedMemory,
        Capability::Signal,
        Capability::Network,
        Capability::Storage,
        Capability::SessionLifecycle,
        Capability::ActivityRead,
        Capability::MeteringRead,
        Capability::GuestLogRead,
        Capability::GuestLogWrite,
        Capability::HostQueue,
        Capability::DelegateGrants,
        Capability::SystemRegistration,
        Capability::MintCertificate,
        Capability::QuotaWrite,
    ]
}

/// The grant set assigned to the bookkeeper: metering and activity readings,
/// shared memory for the bucket topic, and root-namespace route registration.
pub fn bookkeeper_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(
            Capability::MeteringRead,
            vec![ResourceSelector::ResourceClass(
                ResourceClass::MeteringStream,
            )],
        ),
        CapabilityGrant::new(
            Capability::ActivityRead,
            vec![ResourceSelector::ResourceClass(ResourceClass::ActivityLog)],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
        CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
    ]
}

/// Computes the enforcement state for an account: non-delinquent tenants are
/// capped at the hard ceiling (the opt-in overage budget, zero means the plan
/// is the hard cap) and at the operator-authored (or default) process count; a
/// delinquent tenant is zeroed and narrowed to nothing.
///
/// The CPU ceiling is authored as `plan + overage` instructions: the plan and
/// overage carry instruction ceilings, so the operator-facing (human-unit)
/// conversion happens once at the pricing-boundary rate card, upstream of the
/// accountant — the metering/enforcement pipeline never carries a pseudo-time
/// quantity.
pub fn enforcement_for(
    plan: Usage,
    overage: Usage,
    process_quota: u64,
    delinquent: bool,
) -> Enforcement {
    let hard = Usage {
        cpu_instructions: plan
            .cpu_instructions
            .saturating_add(overage.cpu_instructions),
        memory_bytes: plan.memory_bytes.saturating_add(overage.memory_bytes),
        storage_bytes: plan.storage_bytes.saturating_add(overage.storage_bytes),
        bandwidth_bytes: plan.bandwidth_bytes.saturating_add(overage.bandwidth_bytes),
    };
    if delinquent {
        Enforcement {
            cpu_ceiling: 0,
            memory_quota: 0,
            storage_quota: 0,
            process_quota: 0,
            narrowing: all_capabilities(),
        }
    } else {
        Enforcement {
            cpu_ceiling: hard.cpu_instructions,
            memory_quota: hard.memory_bytes,
            storage_quota: hard.storage_bytes,
            process_quota,
            narrowing: Vec::new(),
        }
    }
}

/// Reduces samples into per-tenant buckets: cumulative counters are differenced
/// against the retained last-known value (loss-tolerant) and gauges are sampled
/// directly. Processes without a known tenant are dropped.
pub fn reduce_samples(
    samples: &[ProcessSample],
    last_counters: &mut HashMap<ProcessId, CumulativeCounters>,
) -> HashMap<String, Usage> {
    let mut buckets: HashMap<String, Usage> = HashMap::new();
    for sample in samples {
        let Some(tenant) = &sample.tenant else {
            let _ = sample.process_id;
            continue;
        };
        let last = last_counters
            .get(&sample.process_id)
            .copied()
            .unwrap_or_default();
        let cpu_delta = sample
            .observation
            .cpu_instructions
            .saturating_sub(last.cpu_instructions);
        let bandwidth_delta = sample
            .observation
            .bandwidth_bytes
            .saturating_sub(last.bandwidth_bytes);
        last_counters.insert(
            sample.process_id,
            CumulativeCounters {
                cpu_instructions: sample.observation.cpu_instructions,
                bandwidth_bytes: sample.observation.bandwidth_bytes,
            },
        );

        let bucket = buckets.entry(tenant.clone()).or_default();
        bucket.add_counters(Usage {
            cpu_instructions: cpu_delta,
            memory_bytes: 0,
            storage_bytes: 0,
            bandwidth_bytes: bandwidth_delta,
        });
        bucket.merge_gauges(Usage {
            cpu_instructions: 0,
            memory_bytes: sample.observation.memory_bytes,
            storage_bytes: sample.observation.storage_bytes,
            bandwidth_bytes: 0,
        });
    }
    buckets
}

/// Accountant entrypoint: ledger replay, window rolling, ceiling evaluation,
/// enforcement authoring, and the operator/billing control surface.
#[entrypoint]
async fn accountant(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("accountant: started");

    let ledger = DurableLog::open(LEDGER_LOG).with_context(|| "accountant: ledger open failed")?;
    let accounts = AccountBook::default();
    let mut accounts = accounts;
    accounts
        .rebuild(&ledger)
        .with_context(|| "accountant: ledger replay failed")?;

    // Attach the bookkeeper's bucket topic.
    let topic = ctx
        .lookup(&format!("sel:///{BUCKET_TOPIC_PATH}"))
        .await
        .with_context(|| "accountant: bucket topic resolve failed")?
        .ok_or_else(|| anyhow::anyhow!("accountant: bucket topic route not found"))?;
    let channel = Channel::attach(topic.resource_id)
        .map_err(|e| anyhow::anyhow!("accountant: bucket topic attach failed: {e}"))?;
    let transport = ShmTransport::new(&channel, &channel)
        .map_err(|e| anyhow::anyhow!("accountant: bucket transport failed: {e}"))?;
    let buckets = Subscriber::new(FramedRead::new(transport), None);

    // The narrowing live table consumed by the bridge-server.
    let (narrowing_region, narrowing) = create_live_table(TOPIC_CAPACITY)
        .map_err(|e| anyhow::anyhow!("accountant: narrowing table create failed: {e}"))?;
    let narrowing = Arc::new(narrowing);

    let shared = Arc::new(Mutex::new(Shared {
        accounts,
        ledger,
        buckets,
        narrowing,
        window: None,
    }));

    // Re-author enforcement for every replayed tenant (covers a restart where
    // a tenant's policy records predate this boot).
    {
        let mut shared = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tenants: Vec<String> = shared.accounts.accounts.keys().cloned().collect();
        for tenant in tenants {
            shared.author_enforcement(&tenant);
        }
    }

    // Serve the narrowing table under its root route.
    let narrowing_target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: narrowing_region,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![NARROWING_TABLE_PATH.to_string()],
        target: narrowing_target,
        default: false,
    })
    .await
    .with_context(|| "accountant: serve narrowing table failed")?;

    // The operator/billing control listener.
    let listener =
        ResourceListener::create().with_context(|| "accountant: create listener failed")?;
    let control_target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface: None,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![CONTROL_PATH.to_string()],
        target: control_target,
        default: false,
    })
    .await
    .with_context(|| "accountant: serve control surface failed")?;

    mark_ready();

    // The rolling loop runs alongside the control surface.
    spawn(roll_loop(shared.clone()));

    // Serve the control surface.
    loop {
        let incoming = match listener.recv().await {
            Ok(incoming) => incoming,
            Err(error) => {
                warn!("accountant: accept failed: {error}");
                continue;
            }
        };
        let connection = match selium_shm::rpc::accept::<AccountantControl, AccountantControlResponse>(
            incoming.into(),
        ) {
            Ok(connection) => connection,
            Err(error) => {
                warn!("accountant: rpc accept failed: {error}");
                continue;
            }
        };
        spawn(handle_control(connection, shared.clone()));
    }
}

/// Applies one operator/billing control to the shared state.
fn apply_control(
    shared: &Arc<Mutex<Shared>>,
    control: AccountantControl,
) -> AccountantControlResponse {
    let (tenant, record) = match control {
        AccountantControl::SetPlan { tenant, plan } => (
            tenant.clone(),
            LedgerRecord::SetPlan {
                tenant,
                plan: Usage::from(&plan),
            },
        ),
        AccountantControl::SetOverage { tenant, overage } => (
            tenant.clone(),
            LedgerRecord::SetOverage {
                tenant,
                overage: Usage::from(&overage),
            },
        ),
        AccountantControl::MarkDelinquent { tenant } => {
            (tenant.clone(), LedgerRecord::Delinquent { tenant })
        }
        AccountantControl::MarkRestored { tenant } => {
            (tenant.clone(), LedgerRecord::Restored { tenant })
        }
        AccountantControl::SetProcessQuota { tenant, processes } => (
            tenant.clone(),
            LedgerRecord::SetProcessQuota { tenant, processes },
        ),
    };

    {
        let mut shared = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shared.apply_policy(&tenant, record);
    }

    AccountantControlResponse::Updated { tenant }
}

/// Bookkeeper entrypoint: inventory, sampling, reduction, and publication on a
/// one-second cadence.
#[entrypoint(no_poll)]
async fn bookkeeper(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("bookkeeper: started");

    // The bucket topic this host's bookkeeper publishes to.
    let channel = Channel::create_with_backpressure(
        TOPIC_CAPACITY,
        ChannelBackpressure::Drop,
        ResourceKind::PubSubTopic,
    )
    .with_context(|| "bookkeeper: bucket topic create failed")?;
    let region_id = channel.region_id();
    let transport = ShmTransport::new(&channel, &channel)
        .map_err(|e| anyhow::anyhow!("bookkeeper: bucket transport failed: {e}"))?;
    let mut publisher = Publisher::new(FramedWrite::new(transport));

    let target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: region_id,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![BUCKET_TOPIC_PATH.to_string()],
        target,
        default: false,
    })
    .await
    .with_context(|| "bookkeeper: serve bucket topic failed")?;

    // Live-process inventory, rebuilt from lifecycle activity events.
    let mut inventory: HashMap<ProcessId, Option<String>> = HashMap::new();
    let mut last_counters: HashMap<ProcessId, CumulativeCounters> = HashMap::new();
    let mut activity_cursor = 0_usize;

    mark_ready();

    let mut next = Instant::now().map_err(|e| anyhow::anyhow!("bookkeeper: clock failed: {e}"))?;
    loop {
        next = next
            .checked_add(Duration::from_secs(1))
            .expect("bookkeeper tick overflow");
        sync_inventory(&mut inventory, &mut activity_cursor);

        // Sample every live process (resolving still-unknown tenants, e.g. a
        // process started before its ProcessStarted event was observed).
        let pids: Vec<ProcessId> = inventory.keys().copied().collect();
        let mut samples = Vec::new();
        for pid in pids {
            let tenant = match inventory.get(&pid).cloned().flatten() {
                Some(tenant) => Some(tenant),
                None => match process_tenant(pid) {
                    Ok(Some(tenant)) => {
                        inventory.insert(pid, Some(tenant.clone()));
                        Some(tenant)
                    }
                    _ => None,
                },
            };
            match Metering::read(pid) {
                Ok(Some(observation)) => samples.push(ProcessSample {
                    process_id: pid,
                    tenant,
                    observation,
                }),
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        process_id = pid,
                        "bookkeeper: metering read failed: {error}"
                    );
                }
            }
        }

        let buckets = reduce_samples(&samples, &mut last_counters);
        for (tenant, usage) in &buckets {
            let bucket = MeteringBucket {
                tenant: tenant.clone(),
                cpu_instructions: usage.cpu_instructions,
                memory_bytes: usage.memory_bytes,
                storage_bytes: usage.storage_bytes,
                bandwidth_bytes: usage.bandwidth_bytes,
                // Publish-time stamp: the accountant buckets windows by this
                // stamp, not by its receive-time clock.
                published_unix_s: time::now().map(|nanos| nanos / 1_000_000_000).unwrap_or(0),
            };
            if let Err(error) = publisher.publish(&bucket) {
                warn!(tenant, "bookkeeper: bucket publish failed: {error}");
            }
        }

        Timer::new(next).await;
    }
}

/// The minute-aligned window start second for a bucket, from the bookkeeper's
/// publish-time stamp carried on the bucket — not the accountant's
/// receive-time clock, so a lagging consumer attributes boundary-straddling
/// buckets to the window the bookkeeper sampled them in.
fn bucket_second(bucket: &MeteringBucket) -> u64 {
    bucket.published_unix_s - (bucket.published_unix_s % WINDOW_SECS)
}

/// Builds one live table over its own `LiveTable` ring, returning the ring id
/// and the table.
fn create_live_table(capacity: u64) -> selium_wire::Result<(u64, NarrowingLiveTable)> {
    let channel = Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Drop,
        ResourceKind::LiveTable,
    )?;
    let region_id = channel.region_id();
    let write = ShmTransport::new(&channel, &channel)?;
    let read = ShmTransport::new(&channel, &channel)?;
    let table = LiveTable::new(
        Publisher::new(FramedWrite::new(write)),
        Subscriber::new(FramedRead::new(read), None),
    )?;
    Ok((region_id, table))
}

/// Serves one operator/billing control session. The operator tier is derived
/// from the caller's process tenant: only a root/system principal may author
/// plans, budgets, and billing-state transitions.
async fn handle_control(
    mut connection: selium_shm::rpc::RpcConnection<AccountantControl, AccountantControlResponse>,
    shared: Arc<Mutex<Shared>>,
) {
    let operator = match process_tenant(connection.client_process_id()) {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(error) => {
            warn!(
                client = connection.client_process_id(),
                "accountant: could not resolve caller tenant: {error}"
            );
            false
        }
    };

    loop {
        match connection.recv().await {
            Ok(request) => {
                let response = match request.payload() {
                    Ok(_) if !operator => AccountantControlResponse::Error {
                        context: "operator tier required".to_string(),
                    },
                    Ok(payload) => apply_control(&shared, payload),
                    Err(error) => AccountantControlResponse::Error {
                        context: format!("{error}"),
                    },
                };
                if let Err(error) = request.reply(response).await {
                    warn!("accountant: reply failed: {error}");
                    break;
                }
            }
            Err(selium_shm::rpc::RpcError::ConnectionClosed) => break,
            Err(error) => {
                warn!("accountant: recv failed: {error}");
                break;
            }
        }
    }
}

/// Rolls the current window into the ledger when its minute has elapsed,
/// returning the rolled records (empty when the minute is still open).
fn roll_due(shared: &mut Shared) -> Vec<LedgerRecord> {
    match shared.window.take() {
        Some(window) if window_complete(&window) => shared.roll_window(window),
        window => {
            shared.window = window;
            Vec::new()
        }
    }
}

/// The rolling loop: drains bookkeeper buckets into the current minute window
/// and rolls a completed window into the ledger, re-evaluating enforcement.
async fn roll_loop(shared: Arc<Mutex<Shared>>) {
    loop {
        // Drain available buckets into the current window, rolling when the
        // minute has elapsed. The read is scoped so its `RefMut` drops before
        // the merge re-borrows: a `match` scrutinee's temporary lives for the
        // whole statement, so reading inline would double-borrow the `RefCell`
        // and panic the guest (a wasm trap kills the reactor permanently).
        loop {
            let read = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .buckets
                .read_with_tag();
            match read {
                Ok((bucket, _tag)) => {
                    let mut shared = shared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    // Roll the elapsed window BEFORE merging: the merge
                    // starts the next minute's window, and rolling first
                    // guarantees the completed window reaches the ledger
                    // instead of being dropped by the boundary replacement.
                    for record in roll_due(&mut shared) {
                        if let LedgerRecord::Window { tenant, .. } = &record {
                            shared.author_enforcement(tenant);
                        }
                    }
                    shared.merge_bucket(bucket);
                }
                Err(selium_wire::error::Error::BufferEmpty) => break,
                Err(error) => {
                    warn!("accountant: bucket read failed: {error}");
                    return;
                }
            }
        }

        // A minute ticks even without traffic: roll an elapsed window.
        {
            let mut shared = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let records = roll_due(&mut shared);
            for record in records {
                if let LedgerRecord::Window { tenant, .. } = &record {
                    shared.author_enforcement(tenant);
                }
            }
        }

        let next = Instant::now()
            .ok()
            .and_then(|now| now.checked_add(Duration::from_secs(1)))
            .unwrap_or_else(|| Instant::now().expect("accountant clock"));
        Timer::new(next).await;
    }
}

/// Syncs the process inventory from lifecycle activity events since `cursor`.
fn sync_inventory(inventory: &mut HashMap<ProcessId, Option<String>>, cursor: &mut usize) {
    for event in ActivityLog::read_from(*cursor).unwrap_or_default() {
        *cursor = cursor.saturating_add(1);
        let Some(process_id) = event.process_id else {
            continue;
        };
        match event.kind {
            ActivityKind::ProcessStarted => {
                inventory.entry(process_id).or_insert(None);
            }
            ActivityKind::ProcessExited | ActivityKind::ProcessStopped => {
                inventory.remove(&process_id);
            }
            _ => {}
        }
    }
}

/// Whether a window's minute has elapsed.
fn window_complete(window: &RollingWindow) -> bool {
    time::now()
        .map(|nanos| nanos / 1_000_000_000)
        .map(|now_s| now_s >= window.start_s + WINDOW_SECS)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(cpu: u64, memory: u64, storage: u64, bandwidth: u64) -> Usage {
        Usage {
            cpu_instructions: cpu,
            memory_bytes: memory,
            storage_bytes: storage,
            bandwidth_bytes: bandwidth,
        }
    }

    fn plan() -> Usage {
        usage(1_000, 10_000, 5_000, 2_000)
    }

    fn overage() -> Usage {
        usage(500, 5_000, 2_500, 1_000)
    }

    #[test]
    fn enforcements_map_across_the_state_machine() {
        // Paid/active tenants are capped at the hard ceiling and not narrowed.
        // The CPU ceiling is the plan plus the opt-in overage, in instructions.
        let active = enforcement_for(plan(), overage(), DEFAULT_PROCESS_QUOTA, false);
        assert_eq!(active.cpu_ceiling, 1_500);
        assert_eq!(active.memory_quota, 15_000);
        assert_eq!(active.storage_quota, 7_500);
        assert_eq!(active.process_quota, 100);
        assert!(active.narrowing.is_empty());

        // No opt-in overage means the plan is the hard cap.
        let no_overage = enforcement_for(plan(), Usage::default(), DEFAULT_PROCESS_QUOTA, false);
        assert_eq!(no_overage.cpu_ceiling, 1_000);
        assert_eq!(no_overage.memory_quota, 10_000);
        assert_eq!(no_overage.storage_quota, 5_000);

        // An operator-raised process ceiling threads through unchanged.
        let raised = enforcement_for(plan(), overage(), 250, false);
        assert_eq!(raised.process_quota, 250);

        // Delinquency zeroes quotas (including the CPU ceiling and the process
        // ceiling) and narrows to nothing.
        let delinquent = enforcement_for(plan(), overage(), 500, true);
        assert_eq!(delinquent.cpu_ceiling, 0);
        assert_eq!(delinquent.memory_quota, 0);
        assert_eq!(delinquent.storage_quota, 0);
        assert_eq!(delinquent.process_quota, 0);
        assert_eq!(delinquent.narrowing, all_capabilities());
        assert!(delinquent.narrowing.len() >= 10);
    }

    #[test]
    fn authored_cpu_ceiling_is_plan_plus_overage() {
        // Task 5.1: the authored per-minute CPU instruction ceiling equals the
        // tenant's plan plus its opt-in overage budget.
        let plan = usage(7_000, 0, 0, 0);
        let overage = usage(3_000, 0, 0, 0);
        let enforcement = enforcement_for(plan, overage, DEFAULT_PROCESS_QUOTA, false);
        assert_eq!(
            enforcement.cpu_ceiling,
            plan.cpu_instructions + overage.cpu_instructions
        );
    }

    #[test]
    fn account_defaults_to_the_process_quota_ceiling() {
        // No operator control: the account defaults to a 100-process ceiling.
        let account = Account::default();
        assert_eq!(account.process_quota, DEFAULT_PROCESS_QUOTA);

        // An operator control raises the ceiling, replayed through the ledger.
        let mut account = Account::default();
        account.apply_record(&LedgerRecord::SetProcessQuota {
            tenant: "acme".to_string(),
            processes: 250,
        });
        assert_eq!(account.process_quota, 250);
    }

    #[test]
    fn ledger_records_rkyv_round_trip() {
        let record = LedgerRecord::Window {
            tenant: "acme".to_string(),
            window_start_s: 600,
            usage: usage(100, 200, 300, 400),
            overage: usage(0, 50, 0, 0),
        };
        let encoded = selium_abi::encode_rkyv(&record).expect("encode");
        let decoded: LedgerRecord = selium_abi::decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, record);

        let process_quota = LedgerRecord::SetProcessQuota {
            tenant: "acme".to_string(),
            processes: 250,
        };
        let encoded = selium_abi::encode_rkyv(&process_quota).expect("encode");
        let decoded: LedgerRecord = selium_abi::decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, process_quota);
    }

    #[test]
    fn account_book_replays_policy_and_windows() {
        let mut book = AccountBook::default();
        book.apply_record(LedgerRecord::SetPlan {
            tenant: "acme".to_string(),
            plan: plan(),
        });
        book.apply_record(LedgerRecord::SetOverage {
            tenant: "acme".to_string(),
            overage: overage(),
        });
        book.apply_record(LedgerRecord::Window {
            tenant: "acme".to_string(),
            window_start_s: 60,
            usage: usage(1_200, 12_000, 6_000, 3_000),
            overage: usage(200, 2_000, 1_000, 1_000),
        });

        let account = &book.accounts["acme"];
        assert_eq!(account.state(), AccountState::InOverage);
        assert_eq!(book.history["acme"].len(), 1);
        assert_eq!(book.history["acme"][0].1.memory_bytes, 12_000);

        // Delinquency suspends; restoration returns to the usage-derived state.
        book.apply_record(LedgerRecord::Delinquent {
            tenant: "acme".to_string(),
        });
        assert_eq!(book.accounts["acme"].state(), AccountState::Delinquent);
        book.apply_record(LedgerRecord::Restored {
            tenant: "acme".to_string(),
        });
        assert_eq!(book.accounts["acme"].state(), AccountState::InOverage);
    }

    #[test]
    fn state_transitions_follow_usage_and_ceilings() {
        let mut account = Account {
            plan: plan(),
            overage: overage(),
            delinquent: false,
            last_usage: Usage::default(),
            ..Account::default()
        };
        assert_eq!(account.state(), AccountState::Paid);

        account.last_usage = usage(1_000, 12_000, 2_000, 100);
        assert_eq!(account.state(), AccountState::InOverage);

        account.last_usage = usage(0, 20_000, 0, 0);
        assert_eq!(account.state(), AccountState::AtBudget);

        account.delinquent = true;
        assert_eq!(account.state(), AccountState::Delinquent);
    }

    #[test]
    fn reduce_samples_differences_counters_and_samples_gauges() {
        let mut last_counters = HashMap::new();
        let samples = vec![
            ProcessSample {
                process_id: 1,
                tenant: Some("acme".to_string()),
                observation: MeteringObservation {
                    cpu_instructions: 100,
                    memory_bytes: 1_000,
                    storage_bytes: 500,
                    bandwidth_bytes: 50,
                },
            },
            ProcessSample {
                process_id: 2,
                tenant: Some("acme".to_string()),
                observation: MeteringObservation {
                    cpu_instructions: 300,
                    memory_bytes: 2_000,
                    storage_bytes: 700,
                    bandwidth_bytes: 10,
                },
            },
            // Unknown-tenant samples are dropped.
            ProcessSample {
                process_id: 3,
                tenant: None,
                observation: MeteringObservation {
                    cpu_instructions: 9_000,
                    memory_bytes: 9_000,
                    storage_bytes: 9_000,
                    bandwidth_bytes: 9_000,
                },
            },
        ];

        let first = reduce_samples(&samples, &mut last_counters);
        assert_eq!(first["acme"], usage(400, 2_000, 700, 60));

        // A second tick differences only the incremental counters and samples
        // the gauges directly.
        let second_samples = vec![ProcessSample {
            process_id: 1,
            tenant: Some("acme".to_string()),
            observation: MeteringObservation {
                cpu_instructions: 150,
                memory_bytes: 900,
                storage_bytes: 600,
                bandwidth_bytes: 75,
            },
        }];
        let second = reduce_samples(&second_samples, &mut last_counters);
        assert_eq!(second["acme"], usage(50, 900, 600, 25));
    }

    #[test]
    fn rolling_window_accumulates_counters_and_peaks_gauges() {
        let mut window = RollingWindow {
            start_s: 60,
            usage: HashMap::new(),
        };
        window.merge(&MeteringBucket {
            tenant: "acme".to_string(),
            cpu_instructions: 100,
            memory_bytes: 1_000,
            storage_bytes: 500,
            bandwidth_bytes: 50,
            published_unix_s: 0,
        });
        window.merge(&MeteringBucket {
            tenant: "acme".to_string(),
            cpu_instructions: 50,
            memory_bytes: 800,
            storage_bytes: 900,
            bandwidth_bytes: 25,
            published_unix_s: 0,
        });

        assert_eq!(
            window.usage["acme"],
            usage(150, 1_000, 900, 75),
            "counters accumulate; gauges take the peak"
        );
    }

    #[test]
    fn window_rolls_to_per_tenant_ledger_records_with_overage() {
        let accounts = {
            let mut accounts = HashMap::new();
            accounts.insert(
                "acme".to_string(),
                Account {
                    plan: plan(),
                    overage: overage(),
                    ..Account::default()
                },
            );
            accounts.insert(
                "beta".to_string(),
                Account {
                    plan: Usage::default(),
                    overage: Usage::default(),
                    ..Account::default()
                },
            );
            accounts
        };

        let mut window = RollingWindow {
            start_s: 120,
            usage: HashMap::new(),
        };
        window.merge(&MeteringBucket {
            tenant: "acme".to_string(),
            cpu_instructions: 1_200,
            memory_bytes: 12_000,
            storage_bytes: 6_000,
            bandwidth_bytes: 3_000,
            published_unix_s: 0,
        });
        window.merge(&MeteringBucket {
            tenant: "beta".to_string(),
            cpu_instructions: 10,
            memory_bytes: 20,
            storage_bytes: 30,
            bandwidth_bytes: 40,
            published_unix_s: 0,
        });

        let mut records = window.into_records(&accounts);
        records.sort_by(|a, b| match (a, b) {
            (LedgerRecord::Window { tenant: a, .. }, LedgerRecord::Window { tenant: b, .. }) => {
                a.cmp(b)
            }
            _ => std::cmp::Ordering::Equal,
        });

        match &records[0] {
            LedgerRecord::Window {
                tenant,
                window_start_s,
                usage: windowed_usage,
                overage: windowed_overage,
            } => {
                assert_eq!(tenant, "acme");
                assert_eq!(*window_start_s, 120);
                assert_eq!(*windowed_usage, usage(1_200, 12_000, 6_000, 3_000));
                // Above the plan in every dimension.
                assert_eq!(*windowed_overage, usage(200, 2_000, 1_000, 1_000));
            }
            other => panic!("expected window record, got {other:?}"),
        }
        // An unplanned tenant bills full usage as overage against the zero plan.
        match &records[1] {
            LedgerRecord::Window {
                tenant,
                usage: windowed_usage,
                overage: windowed_overage,
                ..
            } => {
                assert_eq!(tenant, "beta");
                assert_eq!(*windowed_overage, *windowed_usage);
            }
            other => panic!("expected window record, got {other:?}"),
        }
    }
}
