'use strict';

/**
 * Loader for the fals3y native addon.
 *
 * Resolution order:
 *   1. In-tree binary `fals3y.node` next to this file (developer-built via
 *      `npm run build:native`).  Lets you iterate without publishing.
 *   2. Platform-suffixed binary `fals3y.<triple>.node` next to this file
 *      (output of `napi build --platform --release`).
 *   3. Per-platform npm package `fals3y-<platform>` (the published layout).
 *
 * Platform packages declare `os` + `cpu` fields so npm only installs the
 * matching one — `optionalDependencies` failures for the others are silent.
 */

const { existsSync } = require('node:fs');
const { join } = require('node:path');

const { platform, arch } = process;

function isMusl() {
  if (platform !== 'linux') return false;
  // node ≥18 reports glibc version on glibc systems.
  try {
    const report = process.report.getReport();
    return !report.header.glibcVersionRuntime;
  } catch {
    return false;
  }
}

function platformTriple() {
  switch (platform) {
    case 'darwin':
      if (arch === 'arm64') return 'darwin-arm64';
      if (arch === 'x64') return 'darwin-x64';
      break;
    case 'linux':
      if (isMusl()) {
        throw new Error(
          `fals3y: musl-linux is not yet supported. ` +
          `Open an issue if you need it: https://github.com/LukeOfEarth/fals3/issues`,
        );
      }
      if (arch === 'x64') return 'linux-x64-gnu';
      if (arch === 'arm64') return 'linux-arm64-gnu';
      break;
    case 'win32':
      if (arch === 'x64') return 'win32-x64-msvc';
      break;
  }
  throw new Error(
    `fals3y: unsupported platform ${platform}-${arch}. ` +
    `Supported: darwin-arm64, darwin-x64, linux-x64-gnu, linux-arm64-gnu, win32-x64-msvc.`,
  );
}

function loadAddon() {
  // 1. Plain in-tree binary (dev convenience: `cp ../target/release/... fals3y.node`).
  for (const fname of ['fals3y.node', 'fals3.node']) {
    const p = join(__dirname, fname);
    if (existsSync(p)) return require(p);
  }

  const triple = platformTriple();

  // 2. Platform-suffixed in-tree binary (output of `napi build --platform`).
  const localTripled = join(__dirname, `fals3y.${triple}.node`);
  if (existsSync(localTripled)) return require(localTripled);

  // 3. Published per-platform npm package.
  const pkg = `fals3y-${triple}`;
  try {
    return require(pkg);
  } catch (err) {
    throw new Error(
      `fals3y: failed to load native binding for ${triple}.\n` +
      `Tried local binaries and the optional dependency ${pkg}.\n` +
      `Underlying error: ${err && err.message ? err.message : err}\n` +
      `If you installed with --no-optional, reinstall without that flag.`,
    );
  }
}

module.exports = loadAddon();
