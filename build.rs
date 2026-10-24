use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/hosting/vst/shim.cpp");
    println!("cargo:rerun-if-env-changed=VST3_SDK");

    let sdk = find_sdk();
    println!("cargo:warning=Using VST3 SDK from {}", sdk.display());

    let mut build = cc::Build::new();
    build
    .cpp(true)
    .include(&sdk)
    .flag("-std=c++17")
    .flag("-Wno-multichar")
    .warnings(false);

    // Our shim.
    build.file("src/hosting/vst/shim.cpp");

    // SDK sources needed to link. Compiling these directly avoids the
    // requirement of running CMake on the SDK beforehand to produce
    // static libraries. It works for both the vendored minimal subset
    // and a full /opt/vst3sdk checkout.
    let sdk_sources = [
        // pluginterfaces/base
        "pluginterfaces/base/funknown.cpp",
        "pluginterfaces/base/coreiids.cpp",
        "pluginterfaces/base/ustring.cpp",
        "pluginterfaces/base/conststringtable.cpp",

        // public.sdk/source/vst — interface IIDs (IComponent, IAudioProcessor, …)
        "public.sdk/source/vst/vstinitiids.cpp",

        // public.sdk/source/common
        "public.sdk/source/common/memorystream.cpp",
        "public.sdk/source/common/commoniids.cpp",
        "public.sdk/source/common/commonstringconvert.cpp",   // ← StringConvert (base)
        "public.sdk/source/common/threadchecker_linux.cpp",   // ← ThreadChecker (Linux)

        // public.sdk/source/vst/hosting
        "public.sdk/source/vst/hosting/hostclasses.cpp",
        "public.sdk/source/vst/hosting/parameterchanges.cpp",
        "public.sdk/source/vst/hosting/pluginterfacesupport.cpp",
        "public.sdk/source/vst/hosting/module.cpp",
        "public.sdk/source/vst/hosting/module_linux.cpp",

        // public.sdk/source/vst/utility
        "public.sdk/source/vst/utility/stringconvert.cpp",
    ];

    for src in sdk_sources {
        let path = sdk.join(src);
        if path.exists() {
            build.file(path);
        } else {
            println!("cargo:warning=Expected SDK source missing: {}", path.display());
        }
    }

    build.compile("vst_shim");

    // Link the system libraries the SDK relies on.
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-lib=dylib=pthread");
        println!("cargo:rustc-link-lib=dylib=dl");
    }
}

fn find_sdk() -> PathBuf {
    // 1. Environment override
    if let Ok(p) = env::var("VST3_SDK") {
        let path = PathBuf::from(p);
        if path.join("pluginterfaces/vst/ivstcomponent.h").exists() {
            return path;
        }
        println!("cargo:warning=VST3_SDK was set but does not look like a valid SDK");
    }

    // 2. Vendored minimal subset (what we just extracted)
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("vendor/vst3sdk");
    if vendor.join("pluginterfaces/vst/ivstcomponent.h").exists() {
        return vendor;
    }

    // 3. Legacy /opt fallback
    let opt = PathBuf::from("/opt/vst3sdk");
    if opt.join("pluginterfaces/vst/ivstcomponent.h").exists() {
        return opt;
    }

    panic!(
        "\n\nCould not find the VST3 SDK.\n\
Please run `./scripts/sync_vst3_sdk.sh` to vendor the minimal subset, \
or set the VST3_SDK environment variable.\n"
    );
}
