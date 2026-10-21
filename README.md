## Building

Requires the VST3 SDK built at `/opt/vst3sdk` (or set `VST3_SDK` to
your SDK path):

    git clone --recursive https://github.com/steinbergmedia/vst3sdk.git /opt/vst3sdk
    cd /opt/vst3sdk
    cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=OFF \
        -DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=OFF \
        -DSMTG_ENABLE_VSTGUI_SUPPORT=OFF
    cmake --build build --target sdk_hosting sdk_common sdk pluginterfaces base

Then `cargo build --release`.
