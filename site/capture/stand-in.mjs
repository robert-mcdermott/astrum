// A stand-in model for the screenshots: an OpenAI-style chat endpoint that answers from a
// script, so every run draws the same conversation. A script is picked by the words of the
// user's latest message, and its steps are given in turn: the first answer to the question,
// then the one after the first tool result, and so on.
//
//   { match: 'failing', steps: [ { say: 'Let me look.', tool: ['read_file', { path: 'x' }] }, { say: '…' } ] }
//
// A step with `wait: true` never answers, so the agent stays at work in the picture.

import { createServer } from 'node:http';

const words = (s) => s.match(/\S+\s*/g) ?? [];

function textOf(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.map((p) => p.text ?? '').join(' ');
  return '';
}

export function standIn(scripts, { model = 'qwen3-coder:30b' } = {}) {
  const held = [];
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => {
      if (req.method === 'GET' && req.url.startsWith('/v1/models')) {
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ object: 'list', data: [{ id: model, object: 'model', owned_by: 'stand-in' }] }));
      }
      // (What Ollama itself says about its models.)
      if (req.url.startsWith('/api/tags')) {
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ models: [{ name: model, model, details: { family: 'qwen3', parameter_size: '30B' } }] }));
      }
      if (req.url.startsWith('/api/show')) {
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ model_info: { 'qwen3.context_length': 262144 }, details: { family: 'qwen3' } }));
      }
      if (req.method !== 'POST' || !req.url.startsWith('/v1/chat/completions')) {
        res.writeHead(404);
        return res.end();
      }
      const ask = JSON.parse(body);
      const messages = ask.messages ?? [];
      const lastUser = messages.map((m, i) => [m, i]).filter(([m]) => m.role === 'user').pop();
      const question = lastUser ? textOf(lastUser[0].content) : '';
      const step = lastUser ? messages.slice(lastUser[1] + 1).filter((m) => m.role === 'assistant').length : 0;
      let reply;
      if (!ask.tools) reply = { say: 'A short summary of the conversation so far.' };
      else {
        const script = scripts.find((s) => question.includes(s.match));
        reply = script?.steps[step] ?? { say: 'Done.' };
      }
      setTimeout(() => answer(res, reply, step, body), reply.delay ?? 0);
    });
  });
  const answer = (res, reply, step, body) => {
      if (reply.wait) {
        held.push(res);
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        if (reply.say) res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: reply.say }, finish_reason: null }] })}\n\n`);
        return;
      }
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' });
      const chunk = (delta, finish = null) => res.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`);
      chunk({ role: 'assistant', content: '' });
      for (const w of words(reply.say ?? '')) chunk({ content: w });
      if (reply.tool) {
        const [name, args] = reply.tool;
        chunk({ tool_calls: [{ index: 0, id: `call_${step}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] });
      }
      chunk({}, reply.tool ? 'tool_calls' : 'stop');
      const prompt = reply.tokens ?? 3800 + 900 * step + Math.min(4000, body.length / 6);
      res.write(`data: ${JSON.stringify({ choices: [], usage: { prompt_tokens: Math.round(prompt), completion_tokens: 40 + (reply.say?.length ?? 0) / 4 | 0, prompt_tokens_details: { cached_tokens: Math.round(prompt * 0.7) } } })}\n\n`);
      res.end('data: [DONE]\n\n');
  };
  return new Promise((ok) =>
    server.listen(0, '127.0.0.1', () =>
      ok({
        port: server.address().port,
        model,
        close: () => {
          for (const r of held) r.destroy();
          server.closeAllConnections?.();
          server.close();
        },
      }),
    ),
  );
}
