use selium_guest::schema;

#[derive(Debug, Clone, PartialEq)]
#[schema(
    path = "../schemas/live_table.fbs",
    ty = "selium.live_table.LiveTableMessage",
    binding = "crate::fbs::selium::live_table::LiveTableMessage"
)]
pub struct Msg<K, V> {
    pub mutation_id: u64,
    pub key: K,
    pub value: Option<V>,
    pub expected_version: Option<u64>,
}

fn main() {}
