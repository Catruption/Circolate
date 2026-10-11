fn main() {
    let sdk = std::env::var("VST3_SDK").unwrap_or_else(|_| "/opt/vst3sdk".to_string());
    let lib_dir = format!("{sdk}/build/lib/Release");

    println!("cargo:rerun-if-changed=src/vst_shim.cpp");
    println!("cargo:rerun-if-env-changed=VST3_SDK");

    cc::Build::new()
    .cpp(true)
    .file("src/vst_shim.cpp")
    // The SDK cmake does not compile these into any library target —
    // they only land in the sample binaries.  Pull them into our
    // static lib so the linker can find Module::create and MemoryStream.
    .file(format!("{sdk}/public.sdk/source/vst/hosting/module_linux.cpp"))
    .file(format!("{sdk}/public.sdk/source/common/memorystream.cpp"))
    .include(&sdk)
    .flag("-std=c++17")
    .flag("-Wno-multichar")
    .warnings(false)
    .compile("vst_shim");

    println!("cargo:rustc-link-search=native={lib_dir}");
    println!("cargo:rustc-link-lib=static=sdk_hosting");
    println!("cargo:rustc-link-lib=static=sdk_common");
    println!("cargo:rustc-link-lib=static=sdk");
    println!("cargo:rustc-link-lib=static=pluginterfaces");
    println!("cargo:rustc-link-lib=static=base");

    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-link-lib=dylib=pthread");
    println!("cargo:rustc-link-lib=dylib=dl");
}
