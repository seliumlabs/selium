//! Switchboard solver and planning helpers.

use std::collections::{BTreeSet, HashMap};

use selium_switchboard_protocol::{Cardinality, EndpointDirections, EndpointId, SchemaId};
use thiserror::Error;

/// Errors produced while planning switchboard wiring.
#[derive(Debug, Error)]
pub enum SwitchboardError {
    /// An invalid endpoint identifier was referenced.
    #[error("invalid endpoint")]
    InvalidEndpoint,
    /// The requested directions are incompatible.
    #[error("directions cannot be connected")]
    DirectionMismatch,
    /// The solver could not find a valid wiring.
    #[error("graph cannot be solved")]
    Unsolveable,
}

/// Pair of endpoints the solver should consider when planning flows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    /// Producer endpoint identifier.
    pub from: EndpointId,
    /// Consumer endpoint identifier.
    pub to: EndpointId,
}

/// Canonical key describing a channel's schema and attachments.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelKey {
    schema: SchemaId,
    producers: Vec<EndpointId>,
    consumers: Vec<EndpointId>,
}

/// A channel that should exist once the solution is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSpec {
    key: ChannelKey,
}

/// A flow for a single intent mapped to a channel in the solution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowRoute {
    /// Producer endpoint for the flow.
    pub producer: EndpointId,
    /// Consumer endpoint for the flow.
    pub consumer: EndpointId,
    /// Index into the solution channel list.
    pub channel: usize,
}

/// The set of flows required for a given intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntentRoute {
    /// Producer endpoint in the intent.
    pub from: EndpointId,
    /// Consumer endpoint in the intent.
    pub to: EndpointId,
    /// Flow list for the intent.
    pub flows: Vec<FlowRoute>,
}

/// The solver output, describing the channels and per-intent routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Solution {
    /// Channels required for the solution.
    pub channels: Vec<ChannelSpec>,
    /// Routes for each intent.
    pub routes: Vec<IntentRoute>,
}

/// Solver interface for producing wiring plans.
pub trait Solver {
    /// Build a wiring solution for the supplied endpoints and intents.
    fn solve(
        &self,
        endpoints: &HashMap<EndpointId, EndpointDirections>,
        intents: &[Intent],
    ) -> Result<Solution, SwitchboardError>;
}

/// Default `Solver` implementation used by `SwitchboardCore`.
#[derive(Default)]
pub struct DefaultSolver;

/// In-memory state tracker for endpoints and intents.
pub struct SwitchboardCore<S: Solver = DefaultSolver> {
    endpoints: HashMap<EndpointId, EndpointDirections>,
    intents: Vec<Intent>,
    solver: S,
    next_id: EndpointId,
}

#[derive(Clone)]
struct FlowSpec {
    producer: EndpointId,
    consumer: EndpointId,
    schema: SchemaId,
}

#[derive(Clone)]
struct ChannelGroup {
    schema: SchemaId,
    producers: BTreeSet<EndpointId>,
    consumers: BTreeSet<EndpointId>,
}

impl ChannelKey {
    /// Create a new channel key from producers and consumers.
    pub fn new(
        schema: SchemaId,
        producers: impl Iterator<Item = EndpointId>,
        consumers: impl Iterator<Item = EndpointId>,
    ) -> Self {
        let mut producers: Vec<EndpointId> = producers.collect();
        producers.sort();
        producers.dedup();
        let mut consumers: Vec<EndpointId> = consumers.collect();
        consumers.sort();
        consumers.dedup();
        Self {
            schema,
            producers,
            consumers,
        }
    }

    /// Check whether this key matches the supplied endpoints and schema.
    pub fn contains(&self, schema: SchemaId, producer: EndpointId, consumer: EndpointId) -> bool {
        self.schema == schema
            && self.producers.binary_search(&producer).is_ok()
            && self.consumers.binary_search(&consumer).is_ok()
    }

    /// Check whether the key is compatible with another key.
    pub fn is_compatible(&self, desired: &ChannelKey) -> bool {
        if self.schema != desired.schema {
            return false;
        }
        (is_subset(&self.producers, desired.producers())
            || is_subset(desired.producers(), &self.producers))
            && (is_subset(&self.consumers, desired.consumers())
                || is_subset(desired.consumers(), &self.consumers))
    }

    /// Return the schema identifier for the key.
    pub fn schema(&self) -> SchemaId {
        self.schema
    }

    /// Return producers on this channel.
    pub fn producers(&self) -> &[EndpointId] {
        &self.producers
    }

    /// Return consumers on this channel.
    pub fn consumers(&self) -> &[EndpointId] {
        &self.consumers
    }
}

impl ChannelSpec {
    /// Create a new channel specification.
    pub fn new(
        schema: SchemaId,
        producers: impl Iterator<Item = EndpointId>,
        consumers: impl Iterator<Item = EndpointId>,
    ) -> Self {
        Self {
            key: ChannelKey::new(schema, producers, consumers),
        }
    }

    /// Access the channel key.
    pub fn key(&self) -> &ChannelKey {
        &self.key
    }
}

impl DefaultSolver {
    fn make_flow(
        &self,
        producer_id: EndpointId,
        producer: &EndpointDirections,
        consumer_id: EndpointId,
        consumer: &EndpointDirections,
    ) -> Result<FlowSpec, SwitchboardError> {
        let output = producer.output();
        let input = consumer.input();
        if output.cardinality() == Cardinality::Zero || input.cardinality() == Cardinality::Zero {
            return Err(SwitchboardError::DirectionMismatch);
        }
        if output.schema_id() != input.schema_id() {
            return Err(SwitchboardError::DirectionMismatch);
        }
        Ok(FlowSpec {
            producer: producer_id,
            consumer: consumer_id,
            schema: output.schema_id(),
        })
    }
}

impl<S> SwitchboardCore<S>
where
    S: Solver,
{
    /// Create a new core with the supplied solver.
    pub fn new(solver: S) -> Self {
        Self {
            endpoints: HashMap::new(),
            intents: Vec::new(),
            solver,
            next_id: 1,
        }
    }

    /// Register a new endpoint and return its identifier.
    pub fn add_endpoint(&mut self, directions: EndpointDirections) -> EndpointId {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.endpoints.insert(id, directions);
        id
    }

    /// Remove an endpoint and any intents referencing it.
    pub fn remove_endpoint(&mut self, endpoint_id: EndpointId) {
        self.endpoints.remove(&endpoint_id);
        self.intents
            .retain(|intent| intent.from != endpoint_id && intent.to != endpoint_id);
    }

    /// Access the registered endpoints.
    pub fn endpoints(&self) -> &HashMap<EndpointId, EndpointDirections> {
        &self.endpoints
    }

    /// Access the registered intents.
    pub fn intents(&self) -> &[Intent] {
        &self.intents
    }

    /// Add a new intent between endpoints.
    pub fn add_intent(&mut self, from: EndpointId, to: EndpointId) -> Result<(), SwitchboardError> {
        if !self.endpoints.contains_key(&from) || !self.endpoints.contains_key(&to) {
            return Err(SwitchboardError::InvalidEndpoint);
        }
        if self
            .intents
            .iter()
            .any(|intent| intent.from == from && intent.to == to)
        {
            return Ok(());
        }
        self.intents.push(Intent { from, to });
        Ok(())
    }

    /// Remove a previously registered intent.
    pub fn remove_intent(&mut self, from: EndpointId, to: EndpointId) {
        self.intents
            .retain(|intent| intent.from != from || intent.to != to);
    }

    /// Solve the current wiring plan.
    pub fn solve(&self) -> Result<Solution, SwitchboardError> {
        self.solver.solve(&self.endpoints, &self.intents)
    }
}

impl Default for SwitchboardCore {
    fn default() -> Self {
        Self::new(DefaultSolver)
    }
}

impl Solver for DefaultSolver {
    fn solve(
        &self,
        endpoints: &HashMap<EndpointId, EndpointDirections>,
        intents: &[Intent],
    ) -> Result<Solution, SwitchboardError> {
        let mut flows: Vec<FlowSpec> = Vec::new();
        let mut flows_by_intent: Vec<Vec<FlowSpec>> = Vec::with_capacity(intents.len());
        for intent in intents {
            let from_directions = endpoints
                .get(&intent.from)
                .ok_or(SwitchboardError::InvalidEndpoint)?;
            let to_directions = endpoints
                .get(&intent.to)
                .ok_or(SwitchboardError::InvalidEndpoint)?;

            let flow = self.make_flow(intent.from, from_directions, intent.to, to_directions)?;
            flows_by_intent.push(vec![flow.clone()]);
            flows.push(flow);
        }

        let mut consumer_map: HashMap<(EndpointId, SchemaId), BTreeSet<EndpointId>> =
            HashMap::new();
        for flow in flows {
            consumer_map
                .entry((flow.consumer, flow.schema))
                .or_default()
                .insert(flow.producer);
        }

        let mut channel_groups: HashMap<(SchemaId, Vec<EndpointId>), ChannelGroup> = HashMap::new();
        for ((consumer, schema), producers) in consumer_map.into_iter() {
            if producers.is_empty() {
                continue;
            }
            let producers_vec: Vec<EndpointId> = producers.iter().copied().collect();
            let key = (schema, producers_vec.clone());
            let group = channel_groups.entry(key).or_insert_with(|| ChannelGroup {
                schema,
                producers: producers.clone(),
                consumers: BTreeSet::new(),
            });
            group.consumers.insert(consumer);
        }

        let mut merged: Vec<ChannelGroup> = Vec::new();
        let mut by_schema: HashMap<SchemaId, HashMap<BTreeSet<EndpointId>, ChannelGroup>> =
            HashMap::new();
        for group in channel_groups.values() {
            let schema_entry = by_schema.entry(group.schema).or_default();
            let consumer_key = group.consumers.clone();
            let entry = schema_entry
                .entry(consumer_key)
                .or_insert_with(|| group.clone());
            entry.producers.extend(group.producers.iter().copied());
        }
        for schema_entry in by_schema.values() {
            merged.extend(schema_entry.values().cloned());
        }

        let mut input_counts: HashMap<EndpointId, usize> = HashMap::new();
        let mut output_counts: HashMap<EndpointId, usize> = HashMap::new();
        for group in &merged {
            for consumer in &group.consumers {
                *input_counts.entry(*consumer).or_insert(0) += 1;
            }
            for producer in &group.producers {
                *output_counts.entry(*producer).or_insert(0) += 1;
            }
        }

        for (id, endpoint) in endpoints {
            let inputs = *input_counts.get(id).unwrap_or(&0);
            let outputs = *output_counts.get(id).unwrap_or(&0);
            if !endpoint.input().cardinality().allows(inputs)
                || !endpoint.output().cardinality().allows(outputs)
            {
                return Err(SwitchboardError::Unsolveable);
            }
        }

        let mut channel_specs: Vec<ChannelSpec> = merged
            .into_iter()
            .map(|group| {
                ChannelSpec::new(
                    group.schema,
                    group.producers.iter().copied(),
                    group.consumers.iter().copied(),
                )
            })
            .collect();
        channel_specs.sort_by(|a, b| a.key().cmp(b.key()));

        let mut routes: Vec<IntentRoute> = Vec::with_capacity(intents.len());
        for (idx, intent) in intents.iter().enumerate() {
            let mut flow_routes = Vec::new();
            for flow in &flows_by_intent[idx] {
                let channel_index = channel_specs
                    .iter()
                    .position(|spec| {
                        spec.key()
                            .contains(flow.schema, flow.producer, flow.consumer)
                    })
                    .ok_or(SwitchboardError::Unsolveable)?;
                flow_routes.push(FlowRoute {
                    producer: flow.producer,
                    consumer: flow.consumer,
                    channel: channel_index,
                });
            }
            routes.push(IntentRoute {
                from: intent.from,
                to: intent.to,
                flows: flow_routes,
            });
        }

        Ok(Solution {
            channels: channel_specs,
            routes,
        })
    }
}

/// Return the position of the best compatible channel, if any.
pub fn best_compatible_match(available: &[ChannelKey], desired: &ChannelKey) -> Option<usize> {
    let mut best: Option<(usize, usize, usize)> = None;
    for (idx, key) in available.iter().enumerate() {
        if !key.is_compatible(desired) {
            continue;
        }
        let (score, penalty) = compatibility_score(key, desired);
        if best
            .as_ref()
            .map(|(s, p, _)| score > *s || (score == *s && penalty < *p))
            .unwrap_or(true)
        {
            best = Some((score, penalty, idx));
        }
    }
    best.map(|(_, _, idx)| idx)
}

fn compatibility_score(current: &ChannelKey, desired: &ChannelKey) -> (usize, usize) {
    let shared_producers = intersection_len(current.producers(), desired.producers());
    let shared_consumers = intersection_len(current.consumers(), desired.consumers());
    let score = shared_producers + shared_consumers;

    let missing_producers = desired.producers().len().saturating_sub(shared_producers);
    let extra_producers = current.producers().len().saturating_sub(shared_producers);
    let missing_consumers = desired.consumers().len().saturating_sub(shared_consumers);
    let extra_consumers = current.consumers().len().saturating_sub(shared_consumers);
    let penalty = missing_producers + extra_producers + missing_consumers + extra_consumers;

    (score, penalty)
}

fn is_subset<T: Ord + Copy>(a: &[T], b: &[T]) -> bool {
    let mut ia = 0;
    let mut ib = 0;
    while ia < a.len() && ib < b.len() {
        match a[ia].cmp(&b[ib]) {
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Greater => ib += 1,
            std::cmp::Ordering::Equal => {
                ia += 1;
                ib += 1;
            }
        }
    }
    ia == a.len()
}

fn intersection_len<T: Ord + Copy>(a: &[T], b: &[T]) -> usize {
    let mut count = 0;
    let mut ia = 0;
    let mut ib = 0;
    while ia < a.len() && ib < b.len() {
        match a[ia].cmp(&b[ib]) {
            std::cmp::Ordering::Less => ia += 1,
            std::cmp::Ordering::Greater => ib += 1,
            std::cmp::Ordering::Equal => {
                count += 1;
                ia += 1;
                ib += 1;
            }
        }
    }
    count
}
