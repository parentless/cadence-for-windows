fn main() {
    println!("cargo:rerun-if-env-changed=CADENCE_UPDATE_REPOSITORY");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/AppIcon.ico");
        embed_resource::compile(
            "assets/windows.rc",
            embed_resource::ParamsIncludeDirs(["assets"]),
        )
        .manifest_required()
        .expect("could not embed Windows icon");
    }
}
