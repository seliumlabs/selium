use selium_guest::{
    Context,
    entrypoint,
};

#[entrypoint]
async fn context_mixed_entrypoint(ctx: Context, _app_id: u32, _generation: u64) {
    let _ = ctx;
}

fn main() {}
