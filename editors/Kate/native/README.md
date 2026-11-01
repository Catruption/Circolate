# Kate playhead plugin

This is a native Qt 6 / KDE Frameworks 6 plugin for Kate. It connects to
Circolate's `/tmp/circolate.sock` playhead socket, matches the `source_file` in
the version 1 JSON messages to the open local document, and highlights exact
event spans for active notes. Each note uses the player's pitch-class hue and
volume brightness, including the currently active waypoint span. Socket reads
are asynchronous, and ranges are only recreated when notes or colors change.

## Build and try it

Install the Qt 6, KDE Frameworks 6 TextEditor/CoreAddons, Extra CMake Modules,
and CMake development packages for your distribution. Then build with a user
prefix and launch Kate with that prefix active. The `kf6/ktexteditor` install
namespace is required for current KF6 Kate builds:

```sh
cmake -S editors/Kate/native -B /tmp/halotext-kate-build \
  -DCMAKE_INSTALL_PREFIX="$HOME/.local"
cmake --build /tmp/halotext-kate-build
cmake --install /tmp/halotext-kate-build
source /tmp/halotext-kate-build/prefix.sh
kate
```

Enable **Halotext Playhead** in Kate's plugin settings. Start Circolate playback
of the same saved file to see its active note spans highlighted in color.

The Python file in `../plugins/` is the old Pâté/PyKDE4 implementation. Current
Qt 6 Kate installations cannot load that script; use this native plugin instead.
