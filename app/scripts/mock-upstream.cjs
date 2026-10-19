#!/usr/bin/env node
/**
 * 假上游：一个最小的 OpenAI 兼容服务，用来验证 Freeport 的转发与接力。
 *
 *   node scripts/mock-upstream.cjs [port]
 *
 * 行为：
 *   GET  /v1/models                  → mock-bad + mock-good
 *   POST /v1/chat/completions
 *        model=mock-bad             → 429（带 Retry-After: 1）
 *        model=mock-good            → 正常回复（stream / 非 stream 都支持）
 *   缺少 Bearer test-key            → 401
 */

const http = require('node:http');

const PORT = Number(process.argv[2] || 8799);
const KEY = process.env.MOCK_KEY || 'test-key';

const MODELS = [
  { id: 'mock-bad', object: 'model', owned_by: 'mock', context_length: 8192 },
  { id: 'mock-good', object: 'model', owned_by: 'mock', context_length: 32768 },
];

function json(res, code, body) {
  res.writeHead(code, { 'content-type': 'application/json' });
  res.end(JSON.stringify(body));
}

function readBody(req) {
  return new Promise((resolve) => {
    const c = [];
    req.on('data', (x) => c.push(x));
    req.on('end', () => resolve(Buffer.concat(c).toString('utf8')));
  });
}

http
  .createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    const auth = (req.headers.authorization || '').replace(/^Bearer\s+/i, '');

    if (auth !== KEY) {
      return json(res, 401, { error: { message: 'invalid api key', type: 'invalid_request_error' } });
    }

    if (url.pathname === '/v1/models' && req.method === 'GET') {
      return json(res, 200, { object: 'list', data: MODELS });
    }

    if (url.pathname === '/v1/chat/completions' && req.method === 'POST') {
      const body = JSON.parse((await readBody(req)) || '{}');
      const model = body.model;

      if (model === 'mock-bad') {
        res.writeHead(429, {
          'content-type': 'application/json',
          'retry-after': '1',
        });
        return res.end(
          JSON.stringify({ error: { message: 'rate limit exceeded (mock)', type: 'rate_limit_error' } }),
        );
      }

      if (!MODELS.some((m) => m.id === model)) {
        return json(res, 404, { error: { message: `model ${model} not found` } });
      }

      const reply = `pong from ${model}`;
      const usage = { prompt_tokens: 9, completion_tokens: 5, total_tokens: 14 };

      if (body.stream) {
        res.writeHead(200, {
          'content-type': 'text/event-stream',
          'cache-control': 'no-cache',
          connection: 'keep-alive',
        });
        const words = reply.split(' ');
        let i = 0;
        const timer = setInterval(() => {
          if (i < words.length) {
            res.write(
              `data: ${JSON.stringify({
                id: 'chatcmpl-mock',
                object: 'chat.completion.chunk',
                model,
                choices: [{ index: 0, delta: { content: (i ? ' ' : '') + words[i] }, finish_reason: null }],
              })}\n\n`,
            );
            i += 1;
            return;
          }
          clearInterval(timer);
          res.write(
            `data: ${JSON.stringify({
              id: 'chatcmpl-mock',
              object: 'chat.completion.chunk',
              model,
              choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
              usage,
            })}\n\n`,
          );
          res.write('data: [DONE]\n\n');
          res.end();
        }, 40);
        return;
      }

      return json(res, 200, {
        id: 'chatcmpl-mock',
        object: 'chat.completion',
        model,
        choices: [{ index: 0, message: { role: 'assistant', content: reply }, finish_reason: 'stop' }],
        usage,
      });
    }

    json(res, 404, { error: { message: 'not found' } });
  })
  .listen(PORT, '127.0.0.1', () => {
    console.log(`mock upstream on http://127.0.0.1:${PORT}/v1  (key: ${KEY})`);
  });
