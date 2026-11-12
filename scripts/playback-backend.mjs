import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export async function startBackend(root) {
  const directory = await mkdtemp(join(tmpdir(), 'hghub-browser-cache-'));
  const child = spawn('cargo', ['test', '--offline', '--locked', '--lib', 'playback_test_server::browser_media_fixture', '--', '--exact', '--ignored', '--nocapture'], {
    cwd: join(root, 'src-tauri'), env: { ...process.env, HGHUB_TEST_CACHE_DIR: directory }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '', errors = '', timer;
  child.stderr.on('data', chunk => { errors += chunk; });
  const exited = new Promise(resolve => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
    child.once('error', error => resolve({ code: null, error: error.message }));
  });
  try {
    const origin = await new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`Rust fixture startup timed out\n${errors}`)), 120000);
      child.once('error', reject);
      child.stdout.on('data', chunk => {
        output += chunk;
        const match = output.match(/HGHUB_TEST_BRIDGE=(http:\/\/127\.0\.0\.1:\d+)/);
        if (match) resolve(match[1]);
      });
      void exited.then(result => reject(new Error(`Rust fixture exited before startup: ${JSON.stringify(result)}\n${errors}`)));
    });
    clearTimeout(timer);
    const command = async (name, args = {}) => {
      const response = await fetch(`${origin}/api/${name}`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(args) });
      const value = await response.json();
      if (!response.ok) throw new Error(value.error);
      return value;
    };
    return { origin, command, async stop() {
      await command('shutdown').catch(() => {});
      let deadline;
      const result = await Promise.race([exited, new Promise(resolve => { deadline = setTimeout(() => resolve(null), 5000); })]);
      clearTimeout(deadline);
      if (!result) { child.kill('SIGKILL'); await exited; }
      await rm(directory, { recursive: true, force: true });
      if (result?.code !== 0) throw new Error(`Rust fixture did not stop cleanly: ${JSON.stringify(result)}\n${errors}`);
    } };
  } catch (error) {
    clearTimeout(timer); child.kill('SIGKILL'); await exited;
    await rm(directory, { recursive: true, force: true }); throw error;
  }
}
