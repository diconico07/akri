/// This generates Device Plugin code (in v1beta1.rs) from pluginapi.proto
fn main() {
    tonic_build::configure()
        .build_client(true)
        .out_dir("./src/plugin_manager")
        .compile(
            &["./proto/pluginapi.proto", "./proto/podresources.proto"],
            &["./proto"],
        )
        .expect("failed to compile protos");
    tonic_build::configure()
        .build_client(false)
        .out_dir("./src/plugin_manager")
        .compile(
            &["./proto/pluginregistration.proto"/* Disable dra building for some override, "./proto/dra.proto"*/],
            &["./proto"],
        )
        .expect("failed to compile protos");
}
