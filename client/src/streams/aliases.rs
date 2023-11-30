use selium_std::traits::compression::{Compress, Decompress};
use std::sync::Arc;

pub type Comp = Arc<dyn Compress + Send + Sync>;
pub type Decomp = Arc<dyn Decompress + Send + Sync>;
