const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const cargoToml = fs.readFileSync(path.join(root, 'src-tauri', 'Cargo.toml'), 'utf8');
const libRs = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'lib.rs'), 'utf8');
const contextStart = libRs.indexOf('pub struct AppContext');
assert.notEqual(contextStart, -1, 'AppContext definition is missing');
const contextDefinition = libRs.slice(contextStart, contextStart + 2_000);
const reqwestDeclaration = cargoToml
  .split(/\r?\n/)
  .find((line) => line.trim().startsWith('reqwest ='));

assert.ok(reqwestDeclaration, 'Rust HTTP client declaration is missing');
assert.match(reqwestDeclaration, /default-features\s*=\s*false/);
assert.match(reqwestDeclaration, /\bjson\b/);
assert.match(reqwestDeclaration, /\brustls-tls\b/);
assert.match(reqwestDeclaration, /\bsystem-proxy\b/, 'system proxy discovery must stay enabled');

const cargoLock = fs.readFileSync(path.join(root, 'src-tauri', 'Cargo.lock'), 'utf8');
const hyperUtil = cargoLock.match(/\[\[package\]\]\s+name = "hyper-util"[\s\S]*?(?=\n\[\[package\]\]|$)/);
assert.ok(hyperUtil, 'Cargo.lock must contain hyper-util');
assert.match(hyperUtil[0], /"system-configuration"/);
assert.match(hyperUtil[0], /"windows-registry"/);

assert.match(libRs, /fn build_http_client\(\)/);
assert.match(libRs, /fn http_client\(&self\)/);
assert.doesNotMatch(contextDefinition, /client:\s*reqwest::Client/,
  'AppContext must not freeze one reqwest client for the process lifetime');
assert.match(libRs, /let client = context\.http_client\(\)/,
  'network operations must rebuild the client from current system settings');

console.log('Network client proxy contract passed.');
