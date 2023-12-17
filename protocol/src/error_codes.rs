use quinn::VarInt;

pub const SHUTDOWN_IN_PROGRESS: VarInt = VarInt::from_u32(0x1);
pub const SHUTDOWN: VarInt = VarInt::from_u32(0x2);
