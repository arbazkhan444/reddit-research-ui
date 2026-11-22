// Add the app icon and version block to the Windows exe.
// If the resource compiler is missing, the build still succeeds without them.
fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    if let Err(err) =
        embed_resource::compile("assets/app.rc", embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=Could not embed the app icon: {err}");
    }
}
