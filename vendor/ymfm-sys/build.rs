use std::path::Path;

fn main() {
    let ymfm_src = Path::new("components/ymfm/src");

    // .cpp files needed by the chip families exposed through the shim
    let ymfm_sources = [
        "ymfm_adpcm.cpp",
        "ymfm_misc.cpp",
        "ymfm_opl.cpp",
        "ymfm_opm.cpp",
        "ymfm_opn.cpp",
        "ymfm_opq.cpp",
        "ymfm_opz.cpp",
        "ymfm_pcm.cpp",
        "ymfm_ssg.cpp",
    ];

    // bridge + our own shim; ymfm headers are pulled in transitively here too,
    // so silence their (harmless) unused-parameter warnings without losing
    // warnings for our own code
    cxx_build::bridge("src/lib.rs")
        .file("src/shim.cpp")
        .include(ymfm_src)
        .std("c++17")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("ymfm-sys");

    // vendored ymfm sources: silence third-party warnings, built separately
    // so our own code above still gets full warnings
    let mut ymfm = cc::Build::new();
    ymfm.include(ymfm_src).std("c++17").warnings(false);
    for source in ymfm_sources {
        ymfm.file(ymfm_src.join(source));
    }
    ymfm.compile("ymfm-vendor");

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/shim.h");
    println!("cargo:rerun-if-changed=src/shim.cpp");
    println!("cargo:rerun-if-changed={}", ymfm_src.display());
}
