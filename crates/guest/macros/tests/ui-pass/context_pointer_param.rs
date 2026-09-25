use selium_guest::{
    Context,
    entrypoint,
};

#[entrypoint]
async fn context_pointer_entrypoint(ctx: Context, _resolver: (u64, u64)) {
    let _ = ctx;
}

fn main() {}
