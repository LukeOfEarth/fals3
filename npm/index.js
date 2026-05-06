'use strict';

// Load the native .node addon built by NAPI-RS.
// When installed from NPM the binary is pre-built; during local dev it is
// compiled by `napi build` or `cargo build` and placed in the package dir.

const { existsSync } = require('fs');
const { join } = require('path');

function loadAddon() {
  // 1. Try a pre-built binary placed next to this file (npm publish layout).
  const local = join(__dirname, 'fals3.node');
  if (existsSync(local)) {
    return require(local);
  }

  // 2. Try the Cargo debug build output (local development).
  const debug = join(__dirname, '..', 'target', 'debug', 'libfals3_node.dylib');
  const debugSo = join(__dirname, '..', 'target', 'debug', 'libfals3_node.so');
  const debugDll = join(__dirname, '..', 'target', 'debug', 'fals3_node.dll');
  for (const p of [debug, debugSo, debugDll]) {
    if (existsSync(p)) {
      return require(p);
    }
  }

  throw new Error(
    'fals3: could not find native addon. ' +
    'Run `napi build --platform` in the npm/ directory, or `cargo build -p fals3-node` from the repo root.'
  );
}

module.exports = loadAddon();
