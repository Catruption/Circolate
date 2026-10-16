# Effect profiling

Empirical models of Image-Line native effects for the native FX engine.
Each profile is captured in FL Studio with Edison, then reduced to a
small binary artifact under `share/native_fx/`.

## One-time setup per effect

1. **Generate test signals.**
   ```fish
   python3 profile/generate_signals.py
