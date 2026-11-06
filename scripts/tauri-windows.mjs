// Builds the Windows app with the C and C++ runtimes linked statically, so it runs on machines
// without the Visual C++ Redistributable. whisper.cpp's CMake setup ignores Rust's `crt-static`,
// so its release flags are set here (Windows only: these flags would break the macOS build).
//
//   npm run tauri:windows                      # on Windows
//   npm run tauri:windows -- --features vulkan # extra `tauri build` arguments pass through
import { spawnSync } from 'node:child_process';

export const WINDOWS_CRT_ENV = {
  CMAKE_MSVC_RUNTIME_LIBRARY: 'MultiThreaded',
  CMAKE_C_FLAGS_RELEASE: '/MT /O2 /Ob2 /DNDEBUG',
  CMAKE_CXX_FLAGS_RELEASE: '/MT /O2 /Ob2 /DNDEBUG',
};

// whisper.cpp runs on the CPU on Windows (unless built with Vulkan/CUDA), so its SIMD level decides
// the speed. `.cargo/config.toml` turns GGML_NATIVE off, and when cross-compiling ggml then drops
// *all* x86 extensions (scalar code, several times slower). Pin the AVX2 baseline (x86-64-v3:
// Intel since 2013, AMD since 2015) so native and cross builds get the same fast code.
// whisper-rs-sys does not rebuild when these change: after editing them, run
// `cargo clean -p whisper-rs-sys --release --target x86_64-pc-windows-msvc` once.
export const WINDOWS_SIMD_ENV = {
  GGML_SSE42: 'ON',
  GGML_AVX: 'ON',
  GGML_AVX2: 'ON',
  GGML_BMI2: 'ON',
  GGML_FMA: 'ON',
  GGML_F16C: 'ON',
};

const env = { ...process.env, ...WINDOWS_CRT_ENV, ...WINDOWS_SIMD_ENV };
// RUSTFLAGS replaces the rustflags in .cargo/config.toml, so keep crt-static when it's set.
if (env.RUSTFLAGS && !env.RUSTFLAGS.includes('crt-static')) env.RUSTFLAGS += ' -C target-feature=+crt-static';

const r = spawnSync('npx', ['tauri', 'build', ...process.argv.slice(2)], { stdio: 'inherit', env, shell: process.platform === 'win32' });
process.exit(r.status ?? 1);
