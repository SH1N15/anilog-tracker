const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const libRs = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'lib.rs'), 'utf8');
const mobileRs = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'mobile.rs'), 'utf8');

const setupStart = libRs.indexOf('.setup(move |app|');
assert.notEqual(setupStart, -1, 'Tauri setup callback is missing');
const setup = libRs.slice(setupStart, setupStart + 6_000);
assert.match(setup, /app\.manage\(context\.clone\(\)\)/);
assert.match(setup, /start_android_initialization\(app\.handle\(\)\.clone\(\), context\.clone\(\)\)/);
assert.doesNotMatch(setup, /mobile::(?:import_legacy_state|consume_events|configure)\(app\.handle/,
  'Android bridge initialization must not block Tauri setup');

const getStateStart = libRs.indexOf('async fn get_state');
assert.notEqual(getStateStart, -1, 'get_state command is missing');
const getState = libRs.slice(getStateStart, getStateStart + 700);
assert.doesNotMatch(getState, /mobile::consume_events/,
  'get_state must return the Rust snapshot without a synchronous bridge round trip');

const initStart = libRs.indexOf('fn start_android_initialization');
assert.notEqual(initStart, -1, 'Android background initializer is missing');
const initializer = libRs.slice(initStart, initStart + 4_500);
assert.match(initializer, /spawn_blocking/);
assert.match(initializer, /mobile::import_legacy_state/);
assert.match(initializer, /mobile::consume_events/);
assert.match(initializer, /mobile::configure/);
assert.match(initializer, /start_webdav_background/);

assert.match(mobileRs, /call_lock:\s*Arc<Mutex<\(\)>>/);
assert.match(mobileRs, /call_lock[\s\S]{0,300}\.lock\(\)/,
  'Android bridge calls must be serialized during startup and foreground actions');

console.log('Android startup bridge contract passed.');
