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

const env = { ...process.env, ...WINDOWS_CRT_ENV };
// RUSTFLAGS replaces the rustflags in .cargo/config.toml, so keep crt-static when it's set.
if (env.RUSTFLAGS && !env.RUSTFLAGS.includes('crt-static')) env.RUSTFLAGS += ' -C target-feature=+crt-static';

const r = spawnSync('npx', ['tauri', 'build', ...process.argv.slice(2)], { stdio: 'inherit', env, shell: process.platform === 'win32' });
process.exit(r.status ?? 1);
