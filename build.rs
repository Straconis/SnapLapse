// Embeds the app icon and version details into the Windows .exe.
fn main() {
    println!("cargo:rerun-if-changed=res/snaplapse.rc");
    println!("cargo:rerun-if-changed=res/snaplapse.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("res/snaplapse.rc", embed_resource::NONE);
    }
}
