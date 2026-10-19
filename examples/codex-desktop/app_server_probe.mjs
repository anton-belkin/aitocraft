// Read-only probe for the public Codex App Server protocol. Node 22+ built-ins only.
// This never starts, resumes, forks, or sends a message to a chat.
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: {
  url: { type: 'string' }, thread: { type: 'string' },
}});
const url = new URL(values.url ?? 'ws://127.0.0.1:45123');
if (url.protocol !== 'ws:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) {
  throw new Error('This local prototype only probes a loopback WebSocket endpoint.');
}

async function connect(name) {
  const ws = new WebSocket(url);
  let seq = 0;
  const pending = new Map();
  const fail = (error) => {
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  };
  ws.addEventListener('error', () => fail(new Error('App Server connection failed')));
  ws.addEventListener('close', () => fail(new Error('App Server connection closed')));
  ws.addEventListener('message', ({ data }) => {
    const message = JSON.parse(String(data));
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error)));
    else request.resolve(message.result);
  });
  const rpc = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++seq;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`App Server timeout: ${method}`));
    }, 10000);
    pending.set(id, {
      resolve: (result) => { clearTimeout(timer); resolve(result); },
      reject: (error) => { clearTimeout(timer); reject(error); },
    });
    ws.send(JSON.stringify({ id, method, params }));
  });
  const timer = setTimeout(() => ws.close(), 10000);
  try {
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('error', () => reject(new Error('Cannot open App Server socket')), { once: true });
      ws.addEventListener('close', () => reject(new Error('App Server closed before initialization')), { once: true });
    });
    const initialized = await rpc('initialize', {
      clientInfo: { name, title: 'AItoCraft transport probe', version: '1' },
      capabilities: { experimentalApi: true },
    });
    ws.send(JSON.stringify({ method: 'initialized' }));
    return { ws, rpc, initialized };
  } catch (error) {
    ws.close();
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

const clients = [];
const watchdog = setTimeout(() => { console.error('Probe timed out'); process.exit(1); }, 30000);
try {
  clients.push(await connect('aitocraft_probe_a'));
  clients.push(await connect('aitocraft_probe_b'));
  const loaded = await Promise.all(clients.map((client) => client.rpc('thread/loaded/list')));
  const result = {
    url: url.toString(), clients: clients.length,
    server: clients[0].initialized, loaded,
    sameLoadedThreads: JSON.stringify(loaded[0]) === JSON.stringify(loaded[1]),
    messagesSent: 0, chatsCreated: 0, chatsResumed: 0,
  };
  if (values.thread) {
    result.thread = await clients[0].rpc('thread/read', { threadId: values.thread, includeTurns: false });
  }
  console.log(JSON.stringify(result, null, 2));
} finally {
  for (const client of clients) client.ws.close();
  clearTimeout(watchdog);
}
