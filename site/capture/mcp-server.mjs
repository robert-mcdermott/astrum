// A stand-in MCP server for the screenshots, over standard input and output:
//   node mcp-server.mjs issues | docs
// It offers a few tools and answers each call with the same made-up result.

import { createInterface } from 'node:readline';

const SERVERS = {
  issues: {
    info: { name: 'issues', version: '1.4.0' },
    tools: [
      ['list_issues', 'List the open issues of a repository, newest first.'],
      ['get_issue', 'Read one issue and its comments.'],
      ['search_issues', 'Search issues and pull requests by words.'],
      ['create_issue', 'Open a new issue.'],
      ['add_comment', 'Comment on an issue.'],
    ],
    result: '#1043 Orders can reserve more stock than there is (bug, 2 comments)\n#1038 Low-stock report misses items with reservations\n#1021 Allow releasing part of a reservation',
  },
  docs: {
    info: { name: 'docs', version: '0.9.2' },
    tools: [
      ['search_docs', "Search the team's documentation."],
      ['read_page', 'Read a documentation page.'],
    ],
    result: 'Warehouse handbook: "A reservation holds stock for an order until it ships or is cancelled."',
  },
};

const server = SERVERS[process.argv[2]] ?? SERVERS.docs;
const say = (msg) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...msg }) + '\n');

createInterface({ input: process.stdin }).on('line', (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  if (msg.id === undefined) return;
  switch (msg.method) {
    case 'initialize':
      return say({ id: msg.id, result: { protocolVersion: msg.params?.protocolVersion ?? '2025-06-18', capabilities: { tools: {} }, serverInfo: server.info } });
    case 'tools/list':
      return say({ id: msg.id, result: { tools: server.tools.map(([name, description]) => ({ name, description, inputSchema: { type: 'object', properties: { query: { type: 'string' } } } })) } });
    case 'tools/call':
      return say({ id: msg.id, result: { content: [{ type: 'text', text: server.result }] } });
    case 'ping':
      return say({ id: msg.id, result: {} });
    default:
      return say({ id: msg.id, error: { code: -32601, message: 'no such method' } });
  }
});
