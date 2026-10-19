// Public App Server WebSocket <-> JSONL adapter. No model process and no chat resumption.
import readline from 'node:readline';
const url = new URL(process.argv[2]);
if (url.protocol !== 'ws:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) {
  throw new Error('Only a loopback App Server endpoint is supported');
}
const allowed = new Set(['thread/loaded/list', 'thread/read', 'thread/turns/list', 'thread/start',
  'thread/name/set', 'turn/start', 'turn/steer', 'thread/queue/add', 'thread/queue/list', 'thread/queue/delete']);
const ws = new WebSocket(url);
const pending = new Map();
let sequence = 0;
const write = (value) => process.stdout.write(JSON.stringify(value) + '\n');
const fail = (message) => {
  for (const entry of pending.values()) entry.reject(new Error(message));
  pending.clear();
};
function rpc(method, params = {}) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`RPC timeout: ${method}`)); }, 45000);
    pending.set(id, {
      resolve: (value) => { clearTimeout(timer); resolve(value); },
      reject: (error) => { clearTimeout(timer); reject(error); },
    });
    ws.send(JSON.stringify({ id, method, params }));
  });
}
ws.addEventListener('message', ({ data }) => {
  try {
    const message = JSON.parse(String(data));
    if (message.method) {
      // Approval requests belong in Desktop. Never approve them in the bridge.
      if (message.id !== undefined) write({ event: 'attention', method: message.method });
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  } catch (error) { write({ event: 'error', error: String(error) }); }
});
ws.addEventListener('close', () => { fail('App Server disconnected'); process.exitCode = 1; });
ws.addEventListener('error', () => fail('App Server socket error'));
const connectionTimeout = setTimeout(() => { console.error('App Server unavailable'); process.exit(1); }, 10000);
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true });
  ws.addEventListener('error', reject, { once: true });
});
clearTimeout(connectionTimeout);
const server = await rpc('initialize', {
  clientInfo: { name: 'aitocraft_gridcraft_bridge', title: 'GridCraft Desktop bridge', version: '2' },
  capabilities: { experimentalApi: true },
});
ws.send(JSON.stringify({ method: 'initialized' }));
write({ event: 'ready', server });
const lines = readline.createInterface({ input: process.stdin });
for await (const line of lines) {
  let request;
  try {
    request = JSON.parse(line);
    if (!allowed.has(request.method)) throw new Error('RPC method is not allowed by this adapter');
    const result = await rpc(request.method, request.params);
    write({ id: request.id, result });
  } catch (error) { write({ id: request?.id, error: String(error) }); }
}
ws.close();
