// `sqlx::migrate!()` embeds the migrations at compile time, but cargo doesn't
// know the macro reads that directory. Without this, adding a migration
// leaves a stale binary that never applies it.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
