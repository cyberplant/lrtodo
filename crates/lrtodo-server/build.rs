fn main() {
    // Re-embeber la PWA cuando se recompila (`cd web && npm run build`).
    println!("cargo:rerun-if-changed=../../web/dist");
}
