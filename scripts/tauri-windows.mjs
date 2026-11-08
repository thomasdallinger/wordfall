// Builds the Windows app with the C and C++ runtimes linked statically, so it runs on machines
// without the Visual C++ Redistributable. whisper.cpp's CMake setup ignores Rust's `crt-static`,
// so its release flags are set here (Windows only: these flags would break the macOS build).
//
//   npm run tauri:windows                      # GPU via Vulkan (the default on Windows)
//   npm run tauri:windows -- --cpu             # CPU only, for PCs without a Vulkan-capable GPU driver
//   npm run tauri:windows -- --features cuda   # GPU via CUDA instead (needs the CUDA toolkit)
//   (other extra arguments pass through to `tauri build`)
import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import path from 'node:path';

export const WINDOWS_CRT_ENV = {
  CMAKE_MSVC_RUNTIME_LIBRARY: 'MultiThreaded',
  CMAKE_C_FLAGS_RELEASE: '/MT /O2 /Ob2 /DNDEBUG',
  CMAKE_CXX_FLAGS_RELEASE: '/MT /O2 /Ob2 /DNDEBUG',
};

// Without a GPU backend whisper.cpp runs on the CPU, where its SIMD level decides the speed.
// `.cargo/config.toml` turns GGML_NATIVE off, and when cross-compiling ggml then drops *all* x86
// extensions (scalar code, several times slower). Pin the AVX2 baseline (x86-64-v3: Intel since
// 2013, AMD since 2015) so native and cross builds get the same fast code. With a GPU backend this
// still matters for the parts that stay on the CPU (mel spectrogram, VAD).
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

// Large v3 Turbo on even a fast desktop CPU takes about as long as the clip itself (8-core Ryzen:
// ~12 s for a 12 s dictation); on the GPU it is well under a second, like Metal on macOS. Vulkan
// works on NVIDIA, AMD and Intel GPUs with the vulkan-1.dll every current graphics driver installs;
// building needs the Vulkan SDK (glslc compiles ggml's shaders). Without a usable GPU at run time,
// whisper.cpp falls back to the CPU.
function vulkanSdk() {
  if (process.env.VULKAN_SDK) return process.env.VULKAN_SDK;
  // A fresh SDK install sets VULKAN_SDK machine-wide, but terminals opened before it don't see it.
  const root = 'C:\\VulkanSDK';
  if (!existsSync(root)) return null;
  const versions = readdirSync(root)
    .filter((v) => /^\d+(\.\d+)+$/.test(v))
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
  return versions.length ? path.join(root, versions.at(-1)) : null;
}

let args = process.argv.slice(2);
const cpuOnly = args.includes('--cpu');
args = args.filter((a) => a !== '--cpu');
const env = { ...process.env, ...WINDOWS_CRT_ENV, ...WINDOWS_SIMD_ENV };
// Vulkan by default for native Windows builds, unless a backend was picked explicitly. Cross builds
// (from macOS, see README) stay CPU unless asked: they'd need the Windows Vulkan SDK on the host.
const pickedFeatures = args.some((a) => a === '--features' || a === '-f' || a.startsWith('--features='));
if (process.platform === 'win32' && !cpuOnly && !pickedFeatures) {
  const sdk = vulkanSdk();
  if (!sdk) {
    console.error(
      'tauri:windows: the Vulkan SDK is missing. Wordfall uses the GPU through Vulkan; install the SDK with\n' +
        '  winget install KhronosGroup.VulkanSDK\n' +
        'or build for the CPU only (several times slower) with: npm run tauri:windows -- --cpu',
    );
    process.exit(1);
  }
  env.VULKAN_SDK = sdk;
  env.PATH = `${path.join(sdk, 'Bin')}${path.delimiter}${env.PATH ?? env.Path ?? ''}`;
  delete env.Path; // Windows env keys are case-insensitive; keep a single PATH for the child
  args.push('--features', 'vulkan');
  shortenBuildDir(env);
}

// ggml's Vulkan build runs a nested CMake project (vulkan-shaders-gen) whose MSBuild file-tracking
// logs sit up to ~236 characters below cargo's build directory, e.g.
//   <build dir>\release\build\whisper-rs-sys-<hash>\out\build\ggml\src\ggml-vulkan\vulkan-shaders-gen-prefix\src\
//   vulkan-shaders-gen-build\CMakeFiles\CMakeScratch\TryCompile-xxxxxx\cmTC_xxxxx.dir\Debug\cmTC_xxxxx.tlog\link-cvtres.write.1.tlog
// MSBuild can't write paths over 260 characters (even with long paths enabled) and then fails with
// "No CMAKE_C_COMPILER could be found". That leaves ~20 characters for the build directory, which
// no clone under C:\Users\<name> fits in, so cargo's intermediate files (its `build-dir`) move to
// a short directory at the drive root; the app and installer still land in src-tauri\target.
// Set CARGO_BUILD_BUILD_DIR yourself to put them elsewhere.
function shortenBuildDir(env) {
  const DEEPEST_BUILD_PATH = 236;
  if (env.CARGO_BUILD_BUILD_DIR) return;
  const target = env.CARGO_TARGET_DIR ?? path.resolve(import.meta.dirname, '..', 'src-tauri', 'target');
  if (target.length + DEEPEST_BUILD_PATH < 260) return;
  env.CARGO_BUILD_BUILD_DIR = `${env.SystemDrive ?? env.SYSTEMDRIVE ?? 'C:'}\\wfb`;
  console.log(`tauri:windows: the Vulkan build needs short paths; cargo's intermediate files go to ${env.CARGO_BUILD_BUILD_DIR}`);
}
// RUSTFLAGS replaces the rustflags in .cargo/config.toml, so keep crt-static when it's set.
if (env.RUSTFLAGS && !env.RUSTFLAGS.includes('crt-static')) env.RUSTFLAGS += ' -C target-feature=+crt-static';

const r = spawnSync('npx', ['tauri', 'build', ...args], { stdio: 'inherit', env, shell: process.platform === 'win32' });
process.exit(r.status ?? 1);
